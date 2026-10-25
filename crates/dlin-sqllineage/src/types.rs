use serde::Serialize;
use std::fmt;

/// A reference to a table, possibly multi-part (catalog.schema.table).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub(crate) struct TableRef {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

impl TableRef {
    /// Create a table reference with just a table name.
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: None,
            table: table.into(),
        }
    }

    /// Create a table reference with schema and table name.
    pub fn with_schema(schema: impl Into<String>, table: impl Into<String>) -> Self {
        Self {
            catalog: None,
            schema: Some(schema.into()),
            table: table.into(),
        }
    }
}

impl fmt::Display for TableRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(c) = &self.catalog {
            write!(f, "{c}.")?;
        }
        if let Some(s) = &self.schema {
            write!(f, "{s}.")?;
        }
        write!(f, "{}", self.table)
    }
}

/// A reference to a column, optionally qualified with a table.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub(crate) struct ColumnRef {
    pub table: Option<TableRef>,
    pub column: String,
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

/// The complete result of analyzing a single SQL statement.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnalyzeResult {
    /// The kind of statement.
    pub statement_type: StatementType,
    /// Table-level lineage (inputs and output).
    pub tables: TableLineage,
    /// Column-level lineage mappings.
    pub columns: ColumnLineage,
    /// Warnings encountered during analysis.
    pub warnings: Vec<Warning>,
}

impl Default for AnalyzeResult {
    fn default() -> Self {
        Self {
            statement_type: StatementType::Other,
            tables: TableLineage::default(),
            columns: ColumnLineage::default(),
            warnings: Vec::new(),
        }
    }
}

/// Table-level lineage: which tables are read and which is written.
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct TableLineage {
    /// Tables read by the statement (FROM, JOIN, USING, etc.).
    pub inputs: Vec<TableRef>,
    /// Table written by the statement (INSERT, UPDATE, MERGE target), if any.
    pub output: Option<TableRef>,
}

/// Column-level lineage: a collection of column derivation mappings.
#[derive(Debug, Clone, Serialize, Default)]
pub(crate) struct ColumnLineage {
    pub mappings: Vec<ColumnMapping>,
    /// Whether any `SELECT *` in the statement (including nested CTEs and
    /// derived tables) could not be expanded with the supplied catalog.
    ///
    /// This is deliberately statement-wide: an unresolved star in a JOIN
    /// branch still makes the statement's schema incomplete even when the
    /// selected output happens to come from another relation.
    pub has_unresolved_stars: bool,
}

/// One output column and the source columns it derives from.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct ColumnMapping {
    /// The output column.
    pub target: ColumnRef,
    /// Source columns contributing to this output.
    pub sources: Vec<ColumnOrigin>,
    /// The kind of transformation applied.
    pub transform: TransformKind,
}

/// Resolution state of a source column.
#[derive(Debug, Clone, Serialize)]
pub(crate) enum ColumnOrigin {
    /// Fully resolved to a specific table and column.
    Concrete { table: TableRef, column: String },
    /// Multiple candidate tables. A non-empty `candidates` list means genuine
    /// ambiguity between known tables and can be disambiguated by a catalog.
    /// An empty list means the column could not be resolved to any known table;
    /// catalog refinement is not attempted.
    Ambiguous {
        column: String,
        candidates: Vec<TableRef>,
    },
    /// `SELECT *` or `table.*`; catalog needed to expand.
    Wildcard { table: TableRef },
    /// A named output was selected through an unexpanded star in a CTE or
    /// derived table. The table is known, but the physical column cannot be
    /// asserted to be concrete without schema metadata.
    NamedWildcard { table: TableRef, column: String },
    /// One side of a set operation contributes no column source (for example,
    /// a literal projection). Kept alongside the other branch's origins so a
    /// consumer cannot mistake partial lineage for complete lineage.
    SourceFree { column: String },
    /// Derived via recursive CTE; base case sources only.
    Recursive { base_sources: Vec<ColumnOrigin> },
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

/// Options for checking the internal engine result.
#[cfg(test)]
pub(crate) struct AnalyzeOptions {
    /// SQL dialect for parsing.
    pub dialect: Dialect,
    /// Optional catalog for resolving `SELECT *` and unqualified columns.
    pub catalog: Option<Box<dyn CatalogProvider>>,
    /// Normalize unquoted identifiers to lowercase (default: `true`).
    ///
    /// Quoted identifiers (e.g., `"MyTable"`) always preserve their case.
    pub normalize_case: bool,
}

#[cfg(test)]
impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            dialect: Dialect::Generic,
            catalog: None,
            normalize_case: true,
        }
    }
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

/// Error returned when SQL parsing or semantic validation fails.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ParseError {
    pub message: String,
}

#[cfg(test)]
impl From<SemanticError> for ParseError {
    fn from(error: SemanticError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SQL parse error: {}", self.message)
    }
}

#[cfg(test)]
impl std::error::Error for ParseError {}

/// Optional metadata source for resolving `SELECT *` and ambiguous columns.
pub(crate) trait CatalogProvider {
    /// Return the column names of a table. Used to expand `SELECT *`.
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>>;
    /// Given a column name and candidate tables, return the owning table. This
    /// is only called with a non-empty candidate slice.
    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef>;
}
