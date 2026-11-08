use crate::Ident;
use serde::Serialize;
use std::fmt;

/// A reference to a table, possibly multi-part (catalog.schema.table).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub(crate) struct TableRef {
    pub catalog: Option<Ident>,
    pub schema: Option<Ident>,
    pub table: Ident,
}

impl TableRef {
    /// Create a table reference with just a table name.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: None,
            table: Ident::unquoted(table),
        }
    }
}

impl fmt::Display for TableRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(c) = &self.catalog {
            write!(f, "{}.", c.value)?;
        }
        if let Some(s) = &self.schema {
            write!(f, "{}.", s.value)?;
        }
        write!(f, "{}", self.table.value)
    }
}

/// A reference to a column, optionally qualified with a table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct ColumnRef {
    pub table: Option<TableRef>,
    pub column: Ident,
}

/// The type of SQL statement that was analyzed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum StatementType {
    /// `SELECT` (no output table).
    Query,
    /// `INSERT INTO ... SELECT`.
    Insert,
    /// `CREATE TABLE ... AS SELECT`.
    CreateTable,
    /// `UPDATE ... SET`.
    Update,
    /// `DELETE`.
    Delete,
    /// `MERGE INTO ... USING`.
    Merge,
    /// DDL, DCL, or other statement with no lineage.
    Other,
}

/// Table-level lineage: which tables are read and which is written.
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct TableLineage {
    /// Tables read by the statement (FROM, JOIN, USING, etc.).
    pub inputs: Vec<TableRef>,
    /// Table written by the statement (INSERT, UPDATE, MERGE target), if any.
    pub output: Option<TableRef>,
}

/// Evidence for the position of an unknown interval, independent of listed slots.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub(crate) enum StarPosition {
    /// A syntactic star follows the preceding projection slot.
    #[default]
    Inferred,
    /// Catalog evidence does not establish the interval's position.
    Unproven,
    /// The interval follows a proven output position.
    After(usize),
}

impl StarPosition {
    pub(crate) fn shifted(self, base: Option<usize>) -> Self {
        match self {
            Self::After(after) => base
                .and_then(|base| base.checked_add(after))
                .map_or(Self::Unproven, Self::After),
            other => other,
        }
    }
}

/// One output column and the source columns it derives from.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ColumnMapping {
    /// Position evidence for an unenumerated interval.
    pub star_position: StarPosition,
    /// The output column.
    pub target: ColumnRef,
    /// Name and naming provenance for this single output slot. `None` marks
    /// an unexpanded, variable-width star.
    pub output_name: Option<crate::OutputName>,
    /// Proven zero-based position in the complete output order.
    pub ordinal: Option<usize>,
    /// Source columns contributing to this output.
    pub sources: Vec<ColumnOrigin>,
    /// The kind of transformation applied.
    pub transform: TransformKind,
    /// The column or segment of the scope's published model that this mapping
    /// stands for. `None` is used only before publication or for a cut marker.
    #[serde(skip)]
    pub slot: Option<crate::resolve::OutputRef>,
}

/// Resolution state of a source column.
#[derive(Debug, Clone, Serialize)]
pub(crate) enum ColumnOrigin {
    /// Fully resolved to a specific table and column.
    Concrete { table: TableRef, column: Ident },
    /// Multiple candidate tables. A non-empty `candidates` list means genuine
    /// ambiguity between known tables and can be disambiguated by a catalog.
    /// An empty list means the column could not be resolved to any known table;
    /// catalog refinement is not attempted.
    Ambiguous {
        column: Ident,
        candidates: Vec<TableRef>,
    },
    /// `SELECT *` or `table.*`; catalog needed to expand.
    Wildcard { table: TableRef },
    /// A named output was selected through an unexpanded star in a CTE or
    /// derived table. The table is known, but the physical column cannot be
    /// asserted to be concrete without schema metadata.
    NamedWildcard { table: TableRef, column: Ident },
    /// One side of a set operation contributes no column source (for example,
    /// a literal projection). Kept alongside the other branch's origins so a
    /// consumer cannot mistake partial lineage for complete lineage.
    SourceFree { column: Ident },
    /// Derived via recursive CTE; base case sources only.
    Recursive { base_sources: Vec<ColumnOrigin> },
    /// Star whose relation or width cannot be determined.
    UnknownStar,
    /// Typed unresolved contribution preserved by the resolver.
    Unresolved(crate::Unresolved),
}

impl ColumnOrigin {
    /// Whether this unresolved contribution represents an unknown-width star.
    pub(crate) fn is_unknown_star(&self) -> bool {
        matches!(self, Self::UnknownStar)
    }
}

/// What kind of transformation produced an output column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum TransformKind {
    /// Direct column reference (`SELECT col`).
    Direct,
    /// Expression or window function (`SELECT a + b`, `SUM(x) OVER (...)`).
    Expression,
    /// Aggregate function (`SELECT SUM(col)`).
    Aggregation,
    /// Conditional expression (`SELECT CASE WHEN ...`).
    Conditional,
    /// Reserved for future use.
    Window,
    /// Fallback for unhandled expressions.
    Unknown,
}

/// A warning produced during analysis.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Warning {
    pub kind: WarningKind,
    pub location: Option<SourceLocation>,
}

/// The kind of warning.
#[derive(Debug, Clone, Serialize)]
pub(crate) enum WarningKind {
    /// Cycle detected in lineage graph after back-edge removal.
    UnexpectedCycle,
}

/// Source location in the SQL string.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SourceLocation {
    pub line: usize,
    pub column: usize,
}

/// Supported SQL dialects (maps to sqlparser dialects).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// sqlparser's permissive generic dialect; the default.
    #[default]
    Generic,
    /// ANSI SQL.
    Ansi,
    /// PostgreSQL. Folds unquoted identifiers to lower case.
    PostgreSql,
    /// MySQL.
    MySql,
    /// Apache Hive.
    Hive,
    /// Databricks SQL.
    Databricks,
    /// Snowflake. Folds unquoted identifiers to upper case.
    Snowflake,
    /// Google `BigQuery`.
    BigQuery,
    /// `DuckDB`.
    DuckDb,
    /// Amazon Redshift. Folds unquoted identifiers to lower case.
    Redshift,
    /// Trino syntax is handled by sqlparser's generic dialect because the
    /// pinned sqlparser release does not expose a dedicated Trino dialect.
    Trino,
    /// Apache Spark SQL.
    Spark,
    /// `ClickHouse`.
    ClickHouse,
    /// SQLite.
    SQLite,
    /// Microsoft SQL Server / T-SQL.
    MsSql,
}

impl Dialect {
    /// Whether an unqualified relation alias can denote the complete row.
    ///
    /// `BigQuery` and PostgreSQL permit expressions such as `ARRAY_AGG(t)` when
    /// `t` is a range-variable alias.  Such an expression is a row/record
    /// value, not a physical column named after the alias.  The lineage API
    /// has no whole-row origin, so callers must retain honest uncertainty
    /// instead of fabricating a concrete `table.alias` source.
    pub(crate) const fn supports_relation_alias_row_value(self) -> bool {
        matches!(self, Self::BigQuery | Self::PostgreSql)
    }
}

/// A statement the engine cannot analyze because of what it contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SemanticError {
    /// The branches of a set operation have different, exactly known widths.
    SetOperationArityMismatch { left: usize, right: usize },
    /// A pipe operator reshapes the query result, which is not modelled.
    PipeOperators,
    /// `UNPIVOT` applied to an expression rather than a relation.
    UnpivotExpression,
}

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SetOperationArityMismatch { left, right } => write!(
                f,
                "set operation arity mismatch: left has {left} columns, right has {right} columns"
            ),
            Self::PipeOperators => f.write_str("pipe operators are not supported"),
            Self::UnpivotExpression => f.write_str("UNPIVOT on an expression is not supported"),
        }
    }
}
