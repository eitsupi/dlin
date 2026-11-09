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
                    quote: None,
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

fn column_names(result: &ModelColumnLineage) -> Vec<String> {
    result.names()
}

fn source_pairs(entry: &OutputSlot<ColumnSource>) -> Pairs {
    known_pairs_of(entry)
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
    assert_eq!(result.output, None);
    assert_eq!(
        (
            result.listed_columns,
            result.value_complete_columns,
            result.value_partial_columns
        ),
        (0, 0, 0)
    );
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
    assert_eq!(result.listed_columns, 2);
    assert_eq!(result.value_complete_columns, 2);
    assert_eq!(column_names(&result), ["?column?", "a"]);
    assert!(result.slots()[0].lineage.known().is_empty());
    assert_eq!(source_pairs(&result.slots()[1]), [("t", "a")]);

    let result = generic("SELECT a + 1, COUNT(*), a FROM t", &[]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.listed_columns, 3);
    assert_eq!(column_names(&result), ["?column?", "COUNT", "a"]);
    assert_eq!(
        result.slots()[0].transformation,
        TransformationType::Expression
    );
    assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
    assert_eq!(
        result.slots()[1].transformation,
        TransformationType::Aggregation
    );
}

#[test]
fn several_unnamed_expressions_are_separate_anonymous_slots() {
    let result = generic("SELECT 1, 2 FROM t", &[]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.listed_columns, 2);
    assert_eq!(result.value_complete_columns, 2);
    // Anonymous outputs have no name to look up: `?column?` is display only.
    for slot in result.slots() {
        assert_eq!(slot.name, OutputName::Anonymous);
    }
    let output = result.output.as_ref().unwrap();
    assert_eq!(
        output.lookup(&Ident::unquoted("?column?"), DlinDialect::Generic),
        SlotLookup::Absent
    );
}

#[test]
fn set_operation_arity_mismatch_message() {
    let result = generic("SELECT a FROM t UNION ALL SELECT a, b FROM u", &["a"]);
    assert_parse_failure(
        &result,
        "SQL parse error: set operation arity mismatch: left has 1 columns, right has 2 columns",
    );
}

#[test]
fn set_operation_arity_mismatch_without_yaml_columns_has_no_output() {
    let result = generic("SELECT a FROM t UNION ALL SELECT a, b FROM u", &[]);
    assert_eq!(
        only_error(&result).kind,
        ColumnLineageErrorKind::ParseFailure
    );
    assert_eq!(result.output, None);
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
    assert_parse_failure(&result, "dlin found a SELECT with an empty projection");

    let result = analyze("FROM t", &[], DlinDialect::DuckDB);
    assert_eq!(
        only_error(&result).kind,
        ColumnLineageErrorKind::ParseFailure
    );
}

#[test]
fn from_first_with_a_select_list_is_not_an_empty_projection() {
    let result = analyze("FROM t SELECT a", &[], DlinDialect::DuckDB);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(column_names(&result), ["a"]);
    assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
}

#[test]
fn multiple_statements_message() {
    let result = generic("SELECT a FROM t; SELECT b FROM u", &["a"]);
    assert_parse_failure(&result, "expected exactly one statement, found 2");

    // Without YAML columns the failure is the same: no statement is analysed.
    let result = generic("SELECT a FROM t; SELECT b FROM u", &[]);
    assert_eq!(
        only_error(&result).kind,
        ColumnLineageErrorKind::ParseFailure
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
fn declared_only_column_message() {
    let result = generic("SELECT a FROM t", &["x"]);
    assert_eq!(result.listed_columns, 1);
    assert_eq!(result.value_complete_columns, 1);
    let error = only_error(&result);
    assert_eq!(
        error.kind,
        ColumnLineageErrorKind::DeclaredColumnNotInOutput
    );
    assert_eq!(error.column.as_deref(), Some("x"));
    assert_eq!(error.slot(), None);
    assert_eq!(
        error.what,
        "column x is declared in YAML but is not in the SQL output"
    );
    assert_eq!(error.hint, None);
}

#[test]
fn duplicate_output_names_are_not_an_error() {
    let result = generic("SELECT a, a FROM t", &["a"]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(column_names(&result), ["a", "a"]);
    assert_eq!(result.listed_columns, 2);
}

#[test]
fn declared_column_behind_an_open_shape_message() {
    let result = generic("SELECT * FROM t", &["x"]);
    let error = only_error(&result);
    assert_eq!(
        error.kind,
        ColumnLineageErrorKind::DeclaredColumnNotEnumerated
    );
    assert_eq!(
        error.what,
        "column x is declared in YAML but is not among the enumerated SQL outputs"
    );
    assert_eq!(error.hint.as_deref(), Some(STAR_HINT));
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
        Diagnostic, DiagnosticKind, Output, RowDependencies, Severity, Shape, StatementAnalysis,
        StatementKind,
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
        row_dependencies: RowDependencies::NotCollected,
    };
    assert_eq!(super::backend::sqllineage::statement_error(&cycle), None);
    assert_eq!(
        super::backend::sqllineage::analysis_error(&sqllineage::Analysis {
            statements: vec![cycle]
        }),
        None
    );
}

/// The dialects whose column rules the identifier tests below compare.
const IDENTIFIER_DIALECTS: [DlinDialect; 5] = [
    DlinDialect::Generic,
    DlinDialect::Snowflake,
    DlinDialect::PostgreSQL,
    DlinDialect::BigQuery,
    DlinDialect::ClickHouse,
];

#[test]
fn quoted_output_spellings_keep_their_quotes_and_match_yaml_by_dialect_rules() {
    for dialect in IDENTIFIER_DIALECTS {
        for name in ["MiXeD", "id", "ID"] {
            // Whether an unquoted YAML name and the quoted output name are the
            // same column: Generic and Snowflake fold unquoted names to upper
            // case, PostgreSQL to lower case, BigQuery ignores case, and
            // ClickHouse compares the spelling exactly.
            let same_column = matches!(
                (dialect, name),
                (DlinDialect::BigQuery | DlinDialect::ClickHouse, _)
                    | (DlinDialect::Generic | DlinDialect::Snowflake, "ID")
                    | (DlinDialect::PostgreSQL, "id")
            );
            let result = analyze(&format!("SELECT a AS \"{name}\" FROM t"), &[name], dialect);
            assert_counts(&result, 1, 1, 0);
            // The output keeps the quote it was written with.
            assert_eq!(column_names(&result), [format!("\"{name}\"")]);
            assert_eq!(
                result.slots()[0].name,
                OutputName::Explicit(Ident::quoted(name, Quote::Double))
            );
            assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
            if same_column {
                assert!(
                    result.errors.is_empty(),
                    "{dialect:?}/{name}: {:?}",
                    result.errors
                );
            } else {
                assert_declared_difference(
                    &result,
                    ColumnLineageErrorKind::DeclaredColumnNotInOutput,
                    name,
                    "is declared in YAML but is not in the SQL output",
                );
                assert_eq!(result.errors.len(), 1);
            }
        }
    }
}

#[test]
fn yaml_names_are_matched_to_outputs_by_dialect_rules_not_by_spelling() {
    for dialect in IDENTIFIER_DIALECTS {
        for (alias, requested) in [("MiXeD", "mixed"), ("id", "ID"), ("ID", "id")] {
            let found = match dialect {
                DlinDialect::BigQuery => true,
                DlinDialect::Generic | DlinDialect::Snowflake => alias == "ID",
                DlinDialect::PostgreSQL => alias == "id",
                DlinDialect::ClickHouse => false,
                _ => unreachable!(),
            };
            let result = analyze(
                &format!("SELECT a AS \"{alias}\" FROM t"),
                &[requested],
                dialect,
            );
            // The declared name never adds an output of its own.
            assert_counts(&result, 1, 1, 0);
            assert_eq!(column_names(&result), [format!("\"{alias}\"")]);
            assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
            if found {
                assert!(result.errors.is_empty(), "{dialect:?}/{alias}");
            } else {
                assert_declared_difference(
                    &result,
                    ColumnLineageErrorKind::DeclaredColumnNotInOutput,
                    requested,
                    "is declared in YAML but is not in the SQL output",
                );
                assert_eq!(result.errors.len(), 1);
            }
        }
    }
}

#[test]
fn differently_quoted_outputs_with_one_display_name_are_distinct_slots() {
    for dialect in IDENTIFIER_DIALECTS {
        let result = analyze(r#"SELECT a AS id, b AS "id" FROM t"#, &[], dialect);
        assert!(result.errors.is_empty(), "{dialect:?}: {:?}", result.errors);
        assert_counts(&result, 2, 2, 0);
        // The two spellings stay distinguishable in the result.
        assert_eq!(column_names(&result), ["id", r#""id""#]);
        assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
        assert_eq!(source_pairs(&result.slots()[1]), [("t", "b")]);
        // Whether a reference to the unquoted name finds one slot or both is
        // the dialect's rule: Generic and Snowflake fold the unquoted name to
        // upper case, which the quoted lower-case output does not match.
        let output = result.output.as_ref().unwrap();
        let lookup = output.lookup(&Ident::unquoted("id"), dialect);
        if matches!(dialect, DlinDialect::Generic | DlinDialect::Snowflake) {
            assert_eq!(lookup, SlotLookup::Unique(OutputSlotId::new(0)));
        } else {
            assert_eq!(
                lookup,
                SlotLookup::Ambiguous(vec![OutputSlotId::new(0), OutputSlotId::new(1)])
            );
        }
    }
}

#[test]
fn merged_using_star_production_output_keeps_both_sources_and_other_columns() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.orders").unwrap();
    node.columns.clear();
    node.compiled_code =
        Some("SELECT * FROM stg_orders JOIN stg_payments USING (order_id)".to_string());

    let result = compute_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.listed_columns, 7);
    assert_eq!(result.value_complete_columns, 7);
    // The merged USING column comes first, then the remaining columns of each
    // side, in the order the schemas prove.
    assert_eq!(
        column_names(&result),
        [
            "order_id",
            "customer_id",
            "order_date",
            "status",
            "payment_id",
            "amount",
            "payment_method",
        ]
    );
    assert_eq!(
        source_pairs(&result.slots()[0]),
        [("stg_orders", "order_id"), ("stg_payments", "order_id")]
    );
    assert_eq!(
        source_pairs(&result.slots()[1]),
        [("stg_orders", "customer_id")]
    );
    assert_eq!(
        source_pairs(&result.slots()[2]),
        [("stg_orders", "order_date")]
    );
    assert_eq!(source_pairs(&result.slots()[3]), [("stg_orders", "status")]);
    assert_eq!(
        source_pairs(&result.slots()[4]),
        [("stg_payments", "payment_id")]
    );
    assert_eq!(
        source_pairs(&result.slots()[5]),
        [("stg_payments", "amount")]
    );
    assert_eq!(
        source_pairs(&result.slots()[6]),
        [("stg_payments", "payment_method")]
    );
}

#[test]
fn differently_cased_outputs_keep_their_own_lineage_in_sql_order() {
    for dialect in IDENTIFIER_DIALECTS {
        let result = analyze(
            r#"SELECT a AS id, b AS "ID" FROM t"#,
            &["ID", "id"],
            dialect,
        );
        assert!(result.errors.is_empty(), "{dialect:?}: {:?}", result.errors);
        assert_counts(&result, 2, 2, 0);
        assert_eq!(column_names(&result), ["id", r#""ID""#]);
        // The quoted uppercase output keeps b's lineage; it is not folded
        // onto the unquoted id output.
        assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
        assert_eq!(source_pairs(&result.slots()[1]), [("t", "b")]);
        let output = result.output.as_ref().unwrap();
        let lookup = output.lookup(&Ident::unquoted("id"), dialect);
        if matches!(dialect, DlinDialect::PostgreSQL | DlinDialect::ClickHouse) {
            assert_eq!(lookup, SlotLookup::Unique(OutputSlotId::new(0)));
        } else {
            assert_eq!(
                lookup,
                SlotLookup::Ambiguous(vec![OutputSlotId::new(0), OutputSlotId::new(1)])
            );
        }
    }
}

#[test]
fn unquoted_outputs_that_differ_only_in_case_are_two_slots() {
    for dialect in IDENTIFIER_DIALECTS {
        let result = analyze("SELECT a AS Foo, b AS foo FROM t", &[], dialect);
        assert!(result.errors.is_empty(), "{dialect:?}: {:?}", result.errors);
        assert_counts(&result, 2, 2, 0);
        assert_eq!(column_names(&result), ["Foo", "foo"]);
        assert_eq!(source_pairs(&result.slots()[0]), [("t", "a")]);
        assert_eq!(source_pairs(&result.slots()[1]), [("t", "b")]);
        let lookup = result
            .output
            .as_ref()
            .unwrap()
            .lookup(&Ident::unquoted("foo"), dialect);
        if dialect == DlinDialect::ClickHouse {
            assert_eq!(lookup, SlotLookup::Unique(OutputSlotId::new(1)));
        } else {
            assert!(matches!(lookup, SlotLookup::Ambiguous(ids) if ids.len() == 2));
        }
    }
}
