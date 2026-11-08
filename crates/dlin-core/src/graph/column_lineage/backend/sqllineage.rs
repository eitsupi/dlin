use std::collections::{BTreeMap, BTreeSet};

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
    let lookup = lookup_output(&statement.output, column, engine_dialect(dialect)?);
    if lookup.candidates.is_empty() {
        return Err(format!("lineage error: no mapping for column '{column}'"));
    }
    let mut result = serde_json::json!({
        "column": column,
        "mappings": lookup.candidates,
        "shape": statement.output.shape,
    });
    if let Some(reason) = lookup.ambiguity_reason {
        result["ambiguous"] = serde_json::json!(true);
        result["ambiguity_reason"] = serde_json::json!(reason);
    }
    if pretty {
        serde_json::to_string_pretty(&result)
    } else {
        serde_json::to_string(&result)
    }
    .map_err(|error| error.to_string())
}

pub(crate) fn output_names(
    analysis: &sqllineage::StatementAnalysis,
    dialect: DlinDialect,
) -> (Vec<String>, BTreeSet<String>) {
    let dialect = engine_dialect(dialect).expect("dialect checked during parsing");
    let mut names = Vec::new();
    let mut groups = BTreeMap::<Option<String>, Vec<String>>::new();
    for column in &analysis.output.columns {
        let name = output_column_name(column).to_string();
        groups
            .entry(output_column_key(column, dialect))
            .or_default()
            .push(name.clone());
        names.push(name);
    }
    let duplicates = groups
        .into_values()
        .filter(|names| names.len() > 1)
        .flatten()
        .collect();
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
            DiagnosticKind::TargetColumnCountMismatch { target, source } => Some(format!(
                "SQL parse error: target column count mismatch: target has {target} columns, source has {source} columns"
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
pub(crate) fn discovered_output_names(
    analysis: &sqllineage::Analysis,
    dialect: DlinDialect,
) -> Vec<String> {
    match analysis.statements.as_slice() {
        [statement] if statement.kind.carries_lineage() && semantic_error(statement).is_none() => {
            output_names(statement, dialect).0
        }
        _ => Vec::new(),
    }
}

/// The one lineage-carrying statement of an analysis, or why the input does
/// not have exactly one. An empty projection or a semantic error is a failure.
pub(crate) fn single_lineage_statement(
    analysis: &sqllineage::Analysis,
) -> Result<&sqllineage::StatementAnalysis, String> {
    if let Some(detail) = analysis_error(analysis) {
        return Err(detail);
    }
    match analysis.statements.as_slice() {
        [] => Err("no statements in analysis result".to_string()),
        [statement] if statement.kind.carries_lineage() => Ok(statement),
        [_] => Err("statement is not lineage-bearing".to_string()),
        statements => Err(format!(
            "expected exactly one statement, found {}",
            statements.len()
        )),
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
    dialect: DlinDialect,
) -> Result<&'a sqllineage::OutputColumn, OutputFailure> {
    if duplicate_hint {
        return Err(OutputFailure {
            kind: OutputFailureKind::Ambiguous,
            message: format!(
                "cannot resolve output '{name}' because the output name is duplicated"
            ),
        });
    }
    let dialect = engine_dialect(dialect).expect("dialect checked during parsing");
    let lookup = lookup_output(&statement.output, name, dialect);
    if let Some(message) = lookup.ambiguity_reason {
        return Err(OutputFailure {
            kind: OutputFailureKind::Ambiguous,
            message,
        });
    }
    match lookup.candidates.as_slice() {
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

struct OutputLookup<'a> {
    candidates: Vec<&'a sqllineage::OutputColumn>,
    ambiguity_reason: Option<String>,
}

/// Prefer stored output spellings before interpreting a name as an unquoted
/// SQL reference. Keep all candidates and distinguish display collisions.
fn lookup_output<'a>(
    output: &'a sqllineage::Output,
    name: &str,
    dialect: Dialect,
) -> OutputLookup<'a> {
    let exact: Vec<_> = output
        .columns
        .iter()
        .filter(|column| output_column_name(column) == name)
        .collect();
    let keys: BTreeSet<_> = exact
        .iter()
        .filter_map(|column| output_column_key(column, dialect))
        .collect();
    // Anonymous expressions retain dlin's placeholder lookup, independently of
    // SQL identifier keys (including its collision with a literal alias).
    let candidates = if name == ANONYMOUS_COLUMN {
        exact
    } else if !keys.is_empty() {
        output
            .columns
            .iter()
            .filter(|column| {
                output_column_key(column, dialect).is_some_and(|key| keys.contains(&key))
            })
            .collect()
    } else {
        match output.lookup(name, dialect) {
            sqllineage::Lookup::Found(column) => vec![column],
            sqllineage::Lookup::Duplicate(columns) => columns,
            sqllineage::Lookup::Absent { .. } => Vec::new(),
        }
    };
    let ambiguity_reason = (candidates.len() > 1).then(|| {
        if name != ANONYMOUS_COLUMN && keys.len() > 1 {
            format!("cannot resolve output '{name}' because distinct SQL identifiers share the same display name")
        } else {
            format!("more than one sqllineage mapping matches output '{name}'")
        }
    });
    OutputLookup {
        candidates,
        ambiguity_reason,
    }
}

fn output_column_key(column: &sqllineage::OutputColumn, dialect: Dialect) -> Option<String> {
    match &column.name {
        OutputName::Explicit(ident) | OutputName::Inferred(ident) => {
            Some(ident.column_key(dialect).into_owned())
        }
        OutputName::Anonymous => None,
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
        Some(Unresolved::AlignmentUnknown) => (
            OutputFailureKind::Indeterminate,
            "column origin has unknown positional alignment".to_string(),
        ),
        Some(Unresolved::SchemaDependent { .. }) => (
            OutputFailureKind::Indeterminate,
            "column origin depends on unknown schema".to_string(),
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
