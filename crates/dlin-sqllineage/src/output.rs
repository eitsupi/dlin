//! Public SQL lineage results.

use super::{Diagnostic, Ident, Relation};
use serde::Serialize;
use std::fmt;

/// Optional controls for SQL analysis.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Options {
    /// Maximum parser recursion depth, if a limit is desired.
    pub max_recursion_depth: Option<usize>,
}

/// Results for all statements in one SQL input, in source order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Analysis {
    /// Results for each parsed statement.
    pub statements: Vec<StatementAnalysis>,
}

impl Analysis {
    /// Consume the analysis when exactly one statement is expected.
    pub fn into_single(self) -> Result<StatementAnalysis, SingleStatementError> {
        match self.statements.len() {
            0 => Err(SingleStatementError::NoStatements),
            1 => Ok(self.statements.into_iter().next().expect("length checked")),
            count => Err(SingleStatementError::Multiple { count }),
        }
    }
}

/// Error returned when an analysis does not contain exactly one statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum SingleStatementError {
    /// The SQL input contained no statements.
    NoStatements,
    /// The SQL input contained more than one statement.
    Multiple {
        /// Number of statements in the input.
        count: usize,
    },
}

impl fmt::Display for SingleStatementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoStatements => f.write_str("SQL input contains no statements"),
            Self::Multiple { count } => write!(f, "SQL input contains {count} statements"),
        }
    }
}

impl std::error::Error for SingleStatementError {}

/// Analysis details for a single SQL statement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StatementAnalysis {
    /// Statement category.
    pub kind: StatementKind,
    /// Relation written by this statement, if any.
    pub written: Option<Relation>,
    /// Relations read by this statement, sorted by [`Relation::cmp`] and deduplicated.
    pub read: Vec<Relation>,
    /// Ordered output columns and their lineage.
    pub output: Output,
    /// Whether any star in the statement, including nested queries, remains unexpanded.
    /// This does not imply that the final output shape is open.
    pub has_unresolved_stars: bool,
    /// Diagnostics emitted while analyzing this statement.
    pub diagnostics: Vec<Diagnostic>,
}

/// Category of a SQL statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum StatementKind {
    /// A query statement.
    Query,
    /// An insert statement.
    Insert,
    /// A create-table-as-select statement.
    CreateTableAs,
    /// An update statement.
    Update,
    /// A delete statement.
    Delete,
    /// A merge statement.
    Merge,
    /// A statement without modeled lineage.
    Other,
}

impl StatementKind {
    /// Whether this statement kind can carry column lineage.
    pub const fn carries_lineage(&self) -> bool {
        !matches!(self, Self::Other)
    }
}

/// Ordered output columns and whether the output schema is fully known.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Output {
    /// Output columns in SQL projection order.
    pub columns: Vec<OutputColumn>,
    /// Whether the output contains columns beyond those listed.
    pub shape: Shape,
}

impl Output {
    /// Find output columns whose name matches `name`, written as an unquoted
    /// identifier.
    ///
    /// Names are compared by [`Ident::column_key`], which applies each
    /// dialect's column-name rules. In `BigQuery`, `DuckDB`, MySQL, Hive,
    /// Databricks, Spark, Trino, SQLite, SQL Server and Redshift the match is
    /// case-insensitive whether or not the output name is quoted. In Snowflake,
    /// PostgreSQL, Generic and ANSI the unquoted `name` is folded like an
    /// unquoted identifier and a quoted output name matches only its folded
    /// spelling (upper case for Snowflake, Generic and ANSI; lower case for
    /// PostgreSQL). `ClickHouse` compares exactly. Duplicate output name
    /// diagnostics use the same key.
    pub fn lookup(&self, name: &str, dialect: crate::types::Dialect) -> Lookup<'_> {
        let wanted_ident = Ident::unquoted(name);
        let wanted = wanted_ident.column_key(dialect);
        let matches: Vec<_> = self
            .columns
            .iter()
            .filter(|column| {
                column
                    .name
                    .ident()
                    .is_some_and(|ident| ident.column_key(dialect) == wanted)
            })
            .collect();

        match matches.as_slice() {
            [column] => Lookup::Found(column),
            [] => Lookup::Absent {
                may_hide_behind_star: matches!(&self.shape, Shape::Open { .. }),
            },
            _ => Lookup::Duplicate(matches),
        }
    }
}

/// Whether all output columns are known or an unexpanded star remains.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Shape {
    /// Every output column is enumerated.
    Closed,
    /// An unexpanded star may contribute additional unknown columns.
    Open {
        /// Unexpanded stars in projection order.
        unexpanded: Vec<UnexpandedStar>,
    },
}

/// A star projection that could not be expanded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UnexpandedStar {
    /// Relation qualifier, or `None` for an expression star whose relation is
    /// not known. An unqualified `*` over multiple relations produces one
    /// marker per source relation, each with `Some(relation)`.
    pub relation: Option<Relation>,
    /// Ordinal of the last known output column before this star.
    ///
    /// `None` means no earlier column has a known position: either the star
    /// comes first, or an earlier unexpanded star already made later positions
    /// unknown (as in `SELECT *, a, *`). The two cases are not distinguished.
    pub after_ordinal: Option<usize>,
    /// Known value dependencies and unresolved contributions for this projection.
    pub lineage: Lineage,
}

/// One output column and its derivation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutputColumn {
    /// Zero-based output position, if known.
    pub ordinal: Option<usize>,
    /// Explicit, inferred, or absent output name.
    pub name: OutputName,
    /// Source lineage for this output.
    pub lineage: Lineage,
    /// Transformation applied to its sources.
    pub transform: Transform,
}

/// Name of an output column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum OutputName {
    /// Explicit alias or direct column reference.
    Explicit(Ident),
    /// Name inferred from an expression, such as a function name.
    Inferred(Ident),
    /// Expression has no useful inferred name.
    Anonymous,
}

impl OutputName {
    fn ident(&self) -> Option<&Ident> {
        match self {
            Self::Explicit(ident) | Self::Inferred(ident) => Some(ident),
            Self::Anonymous => None,
        }
    }
}

/// Completeness of an output column's source lineage.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Lineage {
    /// All contributing sources are known, sorted by [`SourceColumn::cmp`] and deduplicated.
    /// An empty list represents source-free output.
    Complete(Vec<SourceColumn>),
    /// Known sources coexist with one or more unresolved contributions.
    Partial {
        /// Proven sources, sorted by [`SourceColumn::cmp`] and deduplicated.
        known: Vec<SourceColumn>,
        /// Contributions that could not be proven.
        unresolved: Vec<Unresolved>,
    },
}

/// A source relation and column known to contribute to an output.
///
/// Source lists in lineage results are sorted deterministically and contain no
/// duplicate entries.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SourceColumn {
    /// Source relation.
    pub relation: Relation,
    /// Source column name.
    pub column: Ident,
}

/// A contribution whose source cannot be determined completely.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Unresolved {
    /// An unexpanded star may contribute a column from this relation.
    BehindStar {
        /// Relation selected by the star.
        relation: Relation,
    },
    /// A named output may come from a column hidden behind an unexpanded star.
    NamedBehindStar {
        /// Relation selected by the star.
        relation: Relation,
        /// Possibly contributing column.
        column: Ident,
    },
    /// A column could come from more than one candidate.
    Ambiguous {
        /// Column name.
        column: Ident,
        /// Possible sources, sorted by [`Candidate::cmp`] and deduplicated.
        candidates: Vec<Candidate>,
    },
    /// A column could not be bound to a relation in scope.
    Unbound {
        /// Unbound column name.
        column: Ident,
    },
    /// Recursive lineage is represented by its known base sources only.
    Recursive,
    /// A whole-row alias is used where a column source is expected.
    RowValue {
        /// Whole-row alias.
        alias: Ident,
    },
    /// The syntax is not modeled by the lineage engine.
    Unsupported {
        /// Short stable description of the unsupported construct.
        what: &'static str,
    },
}

/// Kind of candidate source for an ambiguous column.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Candidate {
    /// A physical or catalog relation.
    Relation(Relation),
    /// A common table expression.
    Cte(Ident),
    /// A derived table, optionally named.
    Derived(Option<Ident>),
}

/// Transformation applied to an output column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Transform {
    /// Direct column reference.
    Direct,
    /// Cast or type conversion.
    ///
    /// Reserved: the engine does not classify casts yet, so this is never
    /// produced. A cast reports the transform of the expression it wraps
    /// (`Direct` for a cast of a plain column).
    Cast,
    /// General expression.
    Expression,
    /// Aggregate function.
    Aggregation,
    /// Window function.
    Window,
    /// Conditional expression.
    Conditional,
    /// Transformation could not be classified.
    Unknown,
}

/// Result of looking up an output name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum Lookup<'a> {
    /// Exactly one output column matches.
    Found(&'a OutputColumn),
    /// More than one output column has the matching normalized name.
    Duplicate(Vec<&'a OutputColumn>),
    /// No listed output column matches.
    Absent {
        /// Whether the name could belong to an unexpanded star.
        may_hide_behind_star: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Dialect;

    fn output(names: &[OutputName], shape: Shape) -> Output {
        Output {
            columns: names
                .iter()
                .cloned()
                .map(|name| OutputColumn {
                    ordinal: None,
                    name,
                    lineage: Lineage::Complete(Vec::new()),
                    transform: Transform::Unknown,
                })
                .collect(),
            shape,
        }
    }

    #[test]
    fn lookup_finds_normalized_name_and_reports_duplicates() {
        let one = output(
            &[OutputName::Explicit(Ident::unquoted("VALUE"))],
            Shape::Closed,
        );
        assert!(matches!(
            one.lookup("value", Dialect::PostgreSql),
            Lookup::Found(_)
        ));

        let duplicate = output(
            &[
                OutputName::Explicit(Ident::unquoted("value")),
                OutputName::Inferred(Ident::unquoted("VALUE")),
            ],
            Shape::Closed,
        );
        assert!(matches!(
            duplicate.lookup("value", Dialect::PostgreSql),
            Lookup::Duplicate(columns) if columns.len() == 2
        ));
    }

    #[test]
    fn lookup_reports_names_hidden_by_open_shape() {
        let open = output(
            &[],
            Shape::Open {
                unexpanded: vec![UnexpandedStar {
                    relation: None,
                    after_ordinal: None,
                    lineage: Lineage::Partial {
                        known: Vec::new(),
                        unresolved: vec![Unresolved::Unsupported {
                            what: "star expansion",
                        }],
                    },
                }],
            },
        );
        assert_eq!(
            open.lookup("missing", Dialect::Generic),
            Lookup::Absent {
                may_hide_behind_star: true
            }
        );
    }
}
