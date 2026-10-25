use std::collections::BTreeSet;

use sqllineage::{Analyzer, Dialect, Lineage, OutputName, Transform, Unresolved, parse};

use super::catalog::CatalogSnapshot;
use super::catalog_provider::CatalogAdapter;
use super::dialect::DlinDialect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputFailureKind {
    NotFound,
    Ambiguous,
    Indeterminate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OutputFailure {
    pub kind: OutputFailureKind,
    pub message: String,
}

#[cfg(test)]
pub(crate) struct SqllineageBackend;

#[cfg(test)]
impl SqllineageBackend {
    pub(crate) const fn new() -> Self {
        Self
    }
}

pub(crate) fn engine_dialect(dialect: DlinDialect) -> Result<Dialect, String> {
    dialect.to_engine_dialect()
}

pub(crate) fn analyze_sql(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
) -> Result<sqllineage::Analysis, String> {
    let engine_dialect = engine_dialect(dialect)?;
    let parsed = parse(sql, engine_dialect).map_err(|error| error.to_string())?;
    Ok(analyze_parsed(&parsed, dialect, catalog))
}

pub(crate) fn parse_sql(sql: &str, dialect: DlinDialect) -> Result<sqllineage::ParsedSql, String> {
    parse(sql, engine_dialect(dialect)?).map_err(|error| error.to_string())
}

pub(crate) fn analyze_parsed(
    parsed: &sqllineage::ParsedSql,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
) -> sqllineage::Analysis {
    let engine_dialect = dialect
        .to_engine_dialect()
        .expect("dialect checked during parsing");
    let adapter = catalog.map(|catalog| CatalogAdapter::new(catalog, dialect));
    let analyzer = match adapter.as_ref() {
        Some(catalog) => Analyzer::new(engine_dialect).with_catalog(catalog),
        None => Analyzer::new(engine_dialect),
    };
    parsed.analyze(&analyzer)
}

/// Parse-only validation used by the CLI before validating optional schemas.
pub fn check_sql_parses(sql: &str, dialect: DlinDialect) -> Result<(), String> {
    let dialect = engine_dialect(dialect)?;
    parse(sql, dialect)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Parse SQL and render its engine AST using Rust's debug representation.
pub fn debug_parse_sql_ast_debug(sql: &str, dialect: DlinDialect) -> Result<String, String> {
    let dialect = engine_dialect(dialect)?;
    parse(sql, dialect)
        .map(|parsed| parsed.ast_debug())
        .map_err(|error| format!("parse error: {error}"))
}

/// Parse SQL and render its engine AST as JSON.
pub fn debug_parse_sql_json(
    sql: &str,
    dialect: DlinDialect,
    pretty: bool,
) -> Result<String, String> {
    let dialect = engine_dialect(dialect)?;
    let parsed = parse(sql, dialect).map_err(|error| format!("parse error: {error}"))?;
    parsed.ast_json(pretty).map_err(|error| error.to_string())
}

/// Analyze a column for the CLI's debug trace command.
pub fn debug_trace_column_json(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
    column: &str,
    pretty: bool,
) -> Result<String, String> {
    let analysis = analyze_sql(sql, dialect, catalog)?;
    let statement = analysis.into_single().map_err(legacy_single_error)?;
    let wanted = column.to_lowercase();
    let matches = statement
        .output
        .columns
        .iter()
        .filter(|output| output_column_name(output).to_lowercase() == wanted)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return Err(format!("lineage error: no mapping for column '{column}'"));
    }
    let result = serde_json::json!({
        "column": column,
        "mappings": matches,
        "shape": statement.output.shape,
    });
    if pretty {
        serde_json::to_string_pretty(&result)
    } else {
        serde_json::to_string(&result)
    }
    .map_err(|error| error.to_string())
}

pub(crate) fn output_names(
    analysis: &sqllineage::StatementAnalysis,
) -> (Vec<String>, BTreeSet<String>) {
    let mut names = Vec::new();
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for column in &analysis.output.columns {
        let name = output_column_name(column).to_string();
        if !seen.insert(name.clone()) {
            duplicates.insert(name.clone());
        }
        names.push(name);
    }
    (names, duplicates)
}

/// Name under which an unnamed output expression is listed. Columns without a
/// name keep the placeholder that dlin has always reported for them.
const ANONYMOUS_COLUMN: &str = "?column?";

/// Message for a statement-level failure that rejects the whole model, or
/// `None` when the statement may still be traced.
///
/// An empty projection is checked first, ahead of any semantic error. Other
/// findings (duplicate output names, lineage cycles) never reject a statement.
pub(crate) fn statement_error(statement: &sqllineage::StatementAnalysis) -> Option<String> {
    use sqllineage::DiagnosticKind;
    if statement
        .diagnostics
        .iter()
        .any(|diagnostic| matches!(diagnostic.kind, DiagnosticKind::EmptyProjection))
    {
        return Some("dlin found a SELECT with an empty projection".to_string());
    }
    semantic_error(statement)
}

/// Message for an analysis error that is not an empty projection.
fn semantic_error(statement: &sqllineage::StatementAnalysis) -> Option<String> {
    use sqllineage::DiagnosticKind;
    statement
        .diagnostics
        .iter()
        .find_map(|diagnostic| match &diagnostic.kind {
            DiagnosticKind::SetOperationArityMismatch { left, right } => Some(format!(
                "SQL parse error: set operation arity mismatch: left has {left} columns, \
                 right has {right} columns"
            )),
            DiagnosticKind::UnsupportedSyntax { what } => Some(match *what {
                "pipe operators" => "SQL parse error: pipe operators are not supported".to_string(),
                other => format!("SQL parse error: {other} is not supported"),
            }),
            DiagnosticKind::EmptyProjection
            | DiagnosticKind::UnexpectedCycle
            | DiagnosticKind::DuplicateOutputName { .. } => None,
        })
}

/// Message for the first failure that rejects the whole input: an empty
/// projection in any statement, otherwise the first semantic error.
pub(crate) fn analysis_error(analysis: &sqllineage::Analysis) -> Option<String> {
    analysis
        .statements
        .iter()
        .find(|statement| {
            statement
                .diagnostics
                .iter()
                .any(|d| matches!(d.kind, sqllineage::DiagnosticKind::EmptyProjection))
        })
        .and_then(statement_error)
        .or_else(|| analysis.statements.iter().find_map(semantic_error))
}

/// Output names a model exposes, or nothing unless the input is exactly one
/// lineage-carrying statement that analyzed without a semantic error.
pub(crate) fn discovered_output_names(analysis: &sqllineage::Analysis) -> Vec<String> {
    match analysis.statements.as_slice() {
        [statement] if statement.kind.carries_lineage() && semantic_error(statement).is_none() => {
            output_names(statement).0
        }
        _ => Vec::new(),
    }
}

/// Find the output column a dlin column name refers to.
///
/// `duplicate_hint` is set when the caller already knows the name is used by
/// more than one output.
pub(crate) fn resolve_statement_output<'a>(
    statement: &'a sqllineage::StatementAnalysis,
    name: &str,
    duplicate_hint: bool,
) -> Result<&'a sqllineage::OutputColumn, OutputFailure> {
    if duplicate_hint {
        return Err(OutputFailure {
            kind: OutputFailureKind::Ambiguous,
            message: format!(
                "cannot resolve output '{name}' because the output name is duplicated"
            ),
        });
    }
    // TODO: Names are matched with `to_lowercase()` instead of `Output::lookup`
    // to keep dlin output unchanged. Move to the dialect's column rules and keep
    // dlin-specific compatibility handling separate.
    let wanted = name.to_lowercase();
    let matches = statement
        .output
        .columns
        .iter()
        .filter(|column| output_column_name(column).to_lowercase() == wanted)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [column] => Ok(column),
        [] if statement.has_unresolved_stars => Err(OutputFailure {
            kind: OutputFailureKind::Indeterminate,
            message: format!(
                "cannot match output '{name}' because an unexpanded SELECT * leaves the output columns unknown"
            ),
        }),
        [] => Err(OutputFailure {
            kind: OutputFailureKind::NotFound,
            message: format!("no sqllineage mapping for output '{name}'"),
        }),
        _ => Err(OutputFailure {
            kind: OutputFailureKind::Ambiguous,
            message: format!("more than one sqllineage mapping matches output '{name}'"),
        }),
    }
}

fn output_column_name(column: &sqllineage::OutputColumn) -> &str {
    match &column.name {
        OutputName::Explicit(name) | OutputName::Inferred(name) => &name.value,
        OutputName::Anonymous => ANONYMOUS_COLUMN,
    }
}

pub(crate) fn lineage_failure(lineage: &Lineage) -> Option<OutputFailure> {
    let Lineage::Partial { unresolved, .. } = lineage else {
        return None;
    };
    // A recursive origin is reported as such even when its base case also has
    // unresolved parts: the recursion is what makes the column untraceable.
    let first = unresolved
        .iter()
        .find(|item| matches!(item, Unresolved::Recursive))
        .or_else(|| unresolved.first());
    let (kind, message) = match first {
        Some(Unresolved::Ambiguous { column, candidates }) => {
            let candidates = candidates
                .iter()
                .map(candidate_name)
                .collect::<Vec<_>>()
                .join(", ");
            (
                OutputFailureKind::Ambiguous,
                if candidates.is_empty() {
                    format!("column '{}' is ambiguous", column.value)
                } else {
                    format!(
                        "column '{}' is ambiguous between {candidates}",
                        column.value
                    )
                },
            )
        }
        Some(Unresolved::BehindStar { relation }) => (
            OutputFailureKind::Indeterminate,
            format!(
                "column origin remains wildcard for table '{}'",
                relation_display(relation)
            ),
        ),
        Some(Unresolved::NamedBehindStar { relation, column }) => (
            OutputFailureKind::Indeterminate,
            format!(
                "column '{}' remains behind an unexpanded wildcard for table '{}'",
                column.value,
                relation_display(relation)
            ),
        ),
        // A row value alias was reported as an unbound column before it had
        // its own variant; keep that wording.
        Some(Unresolved::Unbound { column }) | Some(Unresolved::RowValue { alias: column }) => (
            OutputFailureKind::Indeterminate,
            format!("column '{}' has no visible binding", column.value),
        ),
        Some(Unresolved::Recursive) => (
            OutputFailureKind::Indeterminate,
            "column origin is recursive and cannot be represented by dlin".to_string(),
        ),
        Some(Unresolved::Unsupported { what }) => (
            OutputFailureKind::Indeterminate,
            format!("column origin uses unsupported syntax '{what}'"),
        ),
        None => (
            OutputFailureKind::Indeterminate,
            "output column has unresolved lineage".to_string(),
        ),
    };
    Some(OutputFailure { kind, message })
}

/// Render one identifier the way relation names appeared in messages before
/// the engine kept SQL spelling: unquoted names are lower-cased, quoted names
/// keep their spelling, and quotes are not shown.
fn identifier_display(ident: &sqllineage::Ident) -> String {
    if ident.quote.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_lowercase()
    }
}

fn relation_display(relation: &sqllineage::Relation) -> String {
    relation
        .catalog
        .iter()
        .chain(relation.schema.iter())
        .chain(std::iter::once(&relation.name))
        .map(identifier_display)
        .collect::<Vec<_>>()
        .join(".")
}

fn candidate_name(candidate: &sqllineage::Candidate) -> String {
    match candidate {
        sqllineage::Candidate::Relation(relation) => relation_display(relation),
        sqllineage::Candidate::Cte(name) | sqllineage::Candidate::Derived(Some(name)) => {
            identifier_display(name)
        }
        sqllineage::Candidate::Derived(None) => "<derived>".to_string(),
    }
}

#[cfg(test)]
pub(crate) fn has_open_shape(analysis: &sqllineage::StatementAnalysis) -> bool {
    analysis.has_unresolved_stars
}

pub(crate) fn transformation(
    transform: Transform,
) -> crate::graph::column_lineage::TransformationType {
    use crate::graph::column_lineage::TransformationType as T;
    match transform {
        Transform::Direct => T::Direct,
        Transform::Cast => T::Cast,
        Transform::Expression | Transform::Window => T::Expression,
        Transform::Aggregation => T::Aggregation,
        Transform::Conditional => T::Conditional,
        Transform::Unknown => T::Unknown,
    }
}

#[cfg(test)]
impl super::LineageBackend for SqllineageBackend {
    fn id(&self) -> super::BackendId {
        super::BackendId::Sqllineage
    }

    fn discover_output_columns(
        &self,
        request: &super::OutputDiscoveryRequest<'_>,
    ) -> Result<super::OutputDiscovery, super::BackendError> {
        let analysis = analyze_sql(request.sql, request.dialect, request.catalog)
            .map_err(|message| legacy_error(request.dialect, message))?;
        let statement = analysis
            .into_single()
            .map_err(|error| super::BackendError {
                kind: super::BackendErrorKind::IncompleteAnalysis,
                message: legacy_single_error(error),
            })?;
        if !statement.kind.carries_lineage() {
            return Err(super::BackendError {
                kind: super::BackendErrorKind::UnsupportedStatement,
                message: "statement is not lineage-bearing".to_owned(),
            });
        }
        if let Some(message) = statement_error(&statement) {
            return Err(super::BackendError {
                kind: super::BackendErrorKind::Parse,
                message,
            });
        }
        let (names, duplicate_names) = output_names(&statement);
        Ok(super::OutputDiscovery {
            outputs: names
                .into_iter()
                .map(|name| super::DiscoveredOutput {
                    name: super::OutputName::Named(name),
                })
                .collect(),
            duplicate_names,
        })
    }

    fn analyze(
        &self,
        request: &super::LineageRequest<'_>,
    ) -> Result<super::BackendAnalysis, super::BackendError> {
        let analysis = analyze_sql(request.sql, request.dialect, request.catalog)
            .map_err(|message| legacy_error(request.dialect, message))?;
        if let Some(message) = analysis_error(&analysis) {
            return Err(super::BackendError {
                kind: super::BackendErrorKind::Parse,
                message,
            });
        }
        let statements = analysis
            .statements
            .into_iter()
            .enumerate()
            .map(|(statement_ordinal, statement)| {
                let open = has_open_shape(&statement);
                let columns = request
                    .outputs
                    .iter()
                    .map(|request_output| {
                        let target = super::OutputTarget {
                            slot: request_output.slot.clone(),
                            name: request_output.name.clone(),
                        };
                        let column = match resolve_statement_output(
                            &statement,
                            &request_output.name,
                            request
                                .duplicate_output_names
                                .contains(&request_output.name),
                        ) {
                            Ok(column) => column,
                            Err(failure) => return legacy_failed_output(target, failure),
                        };
                        if let Some(failure) = lineage_failure(&column.lineage) {
                            return legacy_failed_output(target, failure);
                        }
                        let Lineage::Complete(known) = &column.lineage else {
                            unreachable!("lineage failure was handled above")
                        };
                        super::BackendColumnOutcome::Resolved(super::BackendColumnResult {
                            target,
                            resolution: super::ResolutionState::Resolved,
                            transformation: transformation(column.transform),
                            sources: known
                                .iter()
                                .map(|source| super::BackendSource::Concrete {
                                    relation: super::source_relation(
                                        &source.relation,
                                        request.catalog,
                                        request.dialect,
                                    ),
                                    column: source.column.value.clone(),
                                })
                                .collect(),
                        })
                    })
                    .collect();
                super::BackendStatementResult {
                    statement_ordinal,
                    lineage_bearing: statement.kind.carries_lineage(),
                    completeness: super::AnalysisCompleteness::Complete,
                    has_unresolved_stars: open,
                    columns,
                }
            })
            .collect();
        Ok(super::BackendAnalysis { statements })
    }
}

pub(crate) fn legacy_single_error(error: sqllineage::SingleStatementError) -> String {
    match error {
        sqllineage::SingleStatementError::NoStatements => {
            "no statements in analysis result".to_string()
        }
        sqllineage::SingleStatementError::Multiple { count } => {
            format!("expected exactly one statement, found {count}")
        }
    }
}

#[cfg(test)]
fn legacy_error(dialect: DlinDialect, message: String) -> super::BackendError {
    let kind = if engine_dialect(dialect).is_err() {
        super::BackendErrorKind::UnsupportedDialect
    } else {
        super::BackendErrorKind::Parse
    };
    super::BackendError { kind, message }
}

#[cfg(test)]
fn legacy_failed_output(
    target: super::OutputTarget,
    failure: OutputFailure,
) -> super::BackendColumnOutcome {
    let state = match failure.kind {
        OutputFailureKind::NotFound => super::ResolutionState::NotFound,
        OutputFailureKind::Ambiguous => super::ResolutionState::Ambiguous,
        OutputFailureKind::Indeterminate => super::ResolutionState::Indeterminate,
    };
    super::BackendColumnOutcome::Failed(super::BackendColumnFailure {
        target,
        resolution: state,
        error: super::BackendError {
            kind: super::BackendErrorKind::ColumnResolution { state },
            message: failure.message,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::{debug_parse_sql_ast_debug, debug_trace_column_json};
    use crate::graph::column_lineage::DlinDialect;

    #[test]
    fn debug_parse_error_has_a_single_prefix() {
        let error = debug_parse_sql_ast_debug("SELECT FROM", DlinDialect::Generic)
            .expect_err("invalid SQL should fail to parse");
        assert!(error.starts_with("parse error: "));
        assert!(!error.contains("parse error: parse error:"));
    }

    #[test]
    fn debug_trace_rejects_multiple_statements() {
        let error = debug_trace_column_json(
            "SELECT id FROM orders; SELECT id FROM customers",
            DlinDialect::Generic,
            None,
            "id",
            false,
        )
        .expect_err("debug trace accepts exactly one statement");
        assert_eq!(error, "expected exactly one statement, found 2");
    }
}
