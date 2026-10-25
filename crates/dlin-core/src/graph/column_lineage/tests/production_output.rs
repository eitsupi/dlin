//! Column-lineage results and messages as `compute_column_lineage` reports
//! them, checked against the full production path.

use super::*;

const PARSE_HINT: &str =
    "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set";
const STAR_HINT: &str = "column may be introduced via SELECT * that could not be expanded; \
                         define upstream columns in the model YAML to enable full resolution";

fn analyze(sql: &str, yaml_columns: &[&str], dialect: DlinDialect) -> ModelColumnLineage {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.depends_on.nodes.clear();
    node.columns = yaml_columns
        .iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            )
        })
        .collect();
    node.compiled_code = Some(sql.to_string());
    compute_column_lineage(
        &manifest,
        "stg_orders",
        dialect,
        &mut ColumnLineageCache::disabled(),
    )
}

fn generic(sql: &str, yaml_columns: &[&str]) -> ModelColumnLineage {
    analyze(sql, yaml_columns, DlinDialect::Generic)
}

fn column_names(result: &ModelColumnLineage) -> Vec<&str> {
    result
        .columns
        .iter()
        .map(|entry| entry.column.as_str())
        .collect()
}

fn source_pairs(entry: &ColumnLineageEntry) -> Vec<(&str, &str)> {
    entry
        .sources
        .iter()
        .map(|source| (source.table.as_str(), source.column.as_str()))
        .collect()
}

/// The single error of a model-level failure.
fn only_error(result: &ModelColumnLineage) -> &ColumnLineageError {
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
    &result.errors[0]
}

fn assert_parse_failure(result: &ModelColumnLineage, why: &str) {
    let error = only_error(result);
    assert_eq!(error.kind, ColumnLineageErrorKind::ParseFailure);
    assert_eq!(error.column, None);
    assert_eq!(error.what, "failed to parse SQL for 'stg_orders'");
    assert_eq!(error.why.as_deref(), Some(why));
    assert_eq!(error.hint.as_deref(), Some(PARSE_HINT));
    assert!(result.columns.is_empty());
}

fn assert_column_error(
    result: &ModelColumnLineage,
    kind: ColumnLineageErrorKind,
    column: &str,
    what: &str,
    hint: Option<&str>,
) {
    let error = result
        .errors
        .iter()
        .find(|error| error.column.as_deref() == Some(column))
        .unwrap_or_else(|| panic!("no error for '{column}': {:?}", result.errors));
    assert_eq!(error.kind, kind);
    assert_eq!(error.what, what);
    assert_eq!(error.why, None);
    assert_eq!(error.hint.as_deref(), hint);
}

#[test]
fn unnamed_expressions_are_listed_as_question_mark_column() {
    let result = generic("SELECT 1, a FROM t", &[]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.total_columns, 2);
    assert_eq!(result.traced_columns, 2);
    assert_eq!(column_names(&result), ["?column?", "a"]);
    assert!(result.columns[0].sources.is_empty());
    assert_eq!(source_pairs(&result.columns[1]), [("t", "a")]);

    let result = generic("SELECT a + 1, COUNT(*), a FROM t", &[]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.total_columns, 3);
    assert_eq!(column_names(&result), ["?column?", "COUNT", "a"]);
    assert_eq!(
        result.columns[0].transformation,
        TransformationType::Expression
    );
    assert_eq!(source_pairs(&result.columns[0]), [("t", "a")]);
    assert_eq!(
        result.columns[1].transformation,
        TransformationType::Aggregation
    );
}

#[test]
fn several_unnamed_expressions_are_a_duplicate_output() {
    let result = generic("SELECT 1, 2 FROM t", &[]);
    assert_eq!(result.total_columns, 1);
    assert_eq!(result.traced_columns, 0);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnAmbiguous,
        "?column?",
        "column '?column?': more than one sqllineage mapping matches output '?column?'",
        None,
    );
}

#[test]
fn set_operation_arity_mismatch_message() {
    let result = generic("SELECT a FROM t UNION ALL SELECT a, b FROM u", &["a"]);
    assert_eq!(result.total_columns, 1);
    assert_parse_failure(
        &result,
        "SQL parse error: set operation arity mismatch: left has 1 columns, right has 2 columns",
    );
}

#[test]
fn set_operation_arity_mismatch_without_yaml_columns_cannot_infer_outputs() {
    let result = generic("SELECT a FROM t UNION ALL SELECT a, b FROM u", &[]);
    assert_eq!(
        only_error(&result).kind,
        ColumnLineageErrorKind::ColumnInferenceFailed
    );
}

#[test]
fn empty_projection_message() {
    for sql in [
        "SELECT FROM t",
        "SELECT 1 UNION ALL SELECT FROM foo",
        "SELECT id FROM (SELECT FROM foo) AS s",
    ] {
        let result = generic(sql, &["x"]);
        assert_parse_failure(&result, "dlin found a SELECT with an empty projection");
    }
}

#[test]
fn empty_projection_wins_over_an_arity_mismatch() {
    let result = generic(
        "SELECT a FROM t UNION ALL SELECT a, b FROM u; SELECT FROM v",
        &["a"],
    );
    assert_parse_failure(&result, "dlin found a SELECT with an empty projection");
}

#[test]
fn duckdb_from_first_without_select_is_an_empty_projection() {
    let result = analyze("FROM t", &["x"], DlinDialect::DuckDB);
    assert_eq!(result.total_columns, 1);
    assert_parse_failure(&result, "dlin found a SELECT with an empty projection");

    let result = analyze("FROM t", &[], DlinDialect::DuckDB);
    assert_eq!(
        only_error(&result).kind,
        ColumnLineageErrorKind::ColumnInferenceFailed
    );
}

#[test]
fn from_first_with_a_select_list_is_not_an_empty_projection() {
    let result = analyze("FROM t SELECT a", &[], DlinDialect::DuckDB);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(column_names(&result), ["a"]);
    assert_eq!(source_pairs(&result.columns[0]), [("t", "a")]);
}

#[test]
fn multiple_statements_message() {
    let result = generic("SELECT a FROM t; SELECT b FROM u", &["a"]);
    assert_eq!(result.total_columns, 1);
    assert_parse_failure(&result, "expected exactly one statement, found 2");

    // Output names are only inferred from a single statement.
    let result = generic("SELECT a FROM t; SELECT b FROM u", &[]);
    assert_eq!(
        only_error(&result).kind,
        ColumnLineageErrorKind::ColumnInferenceFailed
    );
}

#[test]
fn empty_sql_message() {
    let result = generic("", &["a"]);
    assert_parse_failure(&result, "no statements in analysis result");
}

#[test]
fn non_lineage_statement_message() {
    let result = generic("CREATE TABLE x (a INT)", &["a"]);
    assert_parse_failure(&result, "statement is not lineage-bearing");
}

#[test]
fn unsupported_syntax_messages() {
    let result = generic("SELECT * FROM t |> SELECT x", &["x"]);
    assert_parse_failure(&result, "SQL parse error: pipe operators are not supported");

    let result = analyze(
        "SELECT v FROM t, UNPIVOT t.attrs AS v AT k",
        &["v"],
        DlinDialect::Redshift,
    );
    assert_parse_failure(
        &result,
        "SQL parse error: UNPIVOT on an expression is not supported",
    );
}

#[test]
fn syntax_error_message_keeps_the_parser_text() {
    let result = generic("SELECT a FROM (", &["a"]);
    let error = only_error(&result);
    assert_eq!(error.kind, ColumnLineageErrorKind::ParseFailure);
    assert!(
        error
            .why
            .as_deref()
            .is_some_and(|why| why.starts_with("SQL parse error: ")),
        "why: {:?}",
        error.why
    );
}

#[test]
fn missing_output_message() {
    let result = generic("SELECT a FROM t", &["x"]);
    assert_eq!(result.traced_columns, 1);
    assert_eq!(result.total_columns, 2);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnNotFound,
        "x",
        "column 'x': no sqllineage mapping for output 'x'",
        None,
    );
}

#[test]
fn duplicate_output_message() {
    let result = generic("SELECT a, a FROM t", &["a"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnAmbiguous,
        "a",
        "column 'a': more than one sqllineage mapping matches output 'a'",
        None,
    );
}

#[test]
fn output_hidden_behind_star_message() {
    let result = generic("SELECT * FROM t", &["x"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnIndeterminate,
        "x",
        "column 'x': cannot match output 'x' because an unexpanded SELECT * leaves the output columns unknown",
        Some(STAR_HINT),
    );
}

#[test]
fn nested_star_hides_a_source_message_with_lowercase_table() {
    let result = generic("SELECT s.x FROM (SELECT * FROM Users) s", &["x"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnIndeterminate,
        "x",
        "column 'x': column 'x' remains behind an unexpanded wildcard for table 'users'",
        Some(STAR_HINT),
    );
}

#[test]
fn ambiguous_candidates_are_listed_in_lowercase() {
    let result = generic("SELECT x FROM A JOIN B ON 1 = 1", &["x"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnAmbiguous,
        "x",
        "column 'x': column 'x' is ambiguous between a, b",
        None,
    );

    // A quoted name keeps its spelling.
    let result = generic("SELECT x FROM \"A\" JOIN b ON 1 = 1", &["x"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnAmbiguous,
        "x",
        "column 'x': column 'x' is ambiguous between A, b",
        None,
    );
}

#[test]
fn qualified_candidates_are_listed_in_lowercase() {
    let result = generic("SELECT x FROM Raw.A JOIN Raw.B ON 1 = 1", &["x"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnAmbiguous,
        "x",
        "column 'x': column 'x' is ambiguous between raw.a, raw.b",
        None,
    );
}

#[test]
fn unbound_column_message() {
    let result = generic("SELECT z", &["z"]);
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnIndeterminate,
        "z",
        "column 'z': column 'z' has no visible binding",
        None,
    );
}

#[test]
fn recursive_origin_wins_over_an_unresolved_base_case() {
    let result = generic(
        "WITH RECURSIVE r AS (SELECT x FROM t JOIN u ON 1 = 1 UNION ALL SELECT x FROM r) \
         SELECT x FROM r",
        &["x"],
    );
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnIndeterminate,
        "x",
        "column 'x': column origin is recursive and cannot be represented by dlin",
        None,
    );
}

#[test]
fn recursive_origin_without_unresolved_base_case_is_still_recursive() {
    let result = generic(
        "WITH RECURSIVE r AS (SELECT x FROM t UNION ALL SELECT x FROM r) SELECT x FROM r",
        &["x"],
    );
    assert_column_error(
        &result,
        ColumnLineageErrorKind::ColumnIndeterminate,
        "x",
        "column 'x': column origin is recursive and cannot be represented by dlin",
        None,
    );
}

#[test]
fn output_names_keep_their_sql_spelling() {
    let result = generic("SELECT X AS Foo, Y FROM t", &[]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(column_names(&result), ["Foo", "Y"]);
}

#[test]
fn analysis_findings_that_are_not_errors_do_not_reject_the_model() {
    // A lineage cycle is reported by the engine but never rejected here.
    use sqllineage::{
        Diagnostic, DiagnosticKind, Output, Severity, Shape, StatementAnalysis, StatementKind,
    };
    let cycle = StatementAnalysis {
        kind: StatementKind::Query,
        written: None,
        read: Vec::new(),
        output: Output {
            columns: Vec::new(),
            shape: Shape::Closed,
        },
        has_unresolved_stars: false,
        diagnostics: vec![Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::UnexpectedCycle,
            location: None,
        }],
    };
    assert_eq!(super::backend::sqllineage::statement_error(&cycle), None);
    assert_eq!(
        super::backend::sqllineage::analysis_error(&sqllineage::Analysis {
            statements: vec![cycle]
        }),
        None
    );
}
