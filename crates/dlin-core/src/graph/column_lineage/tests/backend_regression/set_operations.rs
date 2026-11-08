use super::*;

#[test]
fn unknown_set_operation_outputs_are_indeterminate() {
    let cases = [
        (
            "SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source",
            ["id", "explicit_col"].as_slice(),
        ),
        (
            "SELECT id, 1 AS explicit_col FROM raw.orders UNION SELECT id, * FROM some_unknown_source",
            ["explicit_col"].as_slice(),
        ),
        (
            "WITH u AS (SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source) SELECT id, explicit_col FROM u",
            ["id", "explicit_col"].as_slice(),
        ),
    ];
    for (sql, names) in cases {
        let result = generic(sql, names);
        for name in names {
            assert_column_error(
                &result,
                name,
                ColumnLineageErrorKind::ColumnIndeterminate,
                "",
            );
        }
    }
}

#[test]
fn nonleading_name_after_leading_star_is_indeterminate() {
    // The leading SELECT * determines the CTE's output names. Since its schema
    // is unknown, the second branch's col_a cannot establish the requested
    // output name or its ordinal alignment.
    let result = generic(
        "WITH u AS (SELECT * FROM ext_a UNION ALL SELECT 2 AS col_a) SELECT col_a FROM u",
        &["col_a"],
    );
    assert_column_error(
        &result,
        "col_a",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "",
    );
}

#[test]
fn leading_name_with_unresolved_aligned_branch_is_indeterminate() {
    // c1 is correctly named by the leading operand and therefore exists. Only
    // its lineage is incomplete because the ordinally aligned a.col_x in the
    // other branch cannot resolve through the unknown-schema SELECT * CTE.
    let result = generic(
        "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c1, 2 AS c2 UNION ALL SELECT a.col_x, a.col_y FROM a) SELECT c1 FROM u",
        &["c1"],
    );
    assert_column_error(
        &result,
        "c1",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "",
    );
}

#[test]
fn nonleading_set_output_name_through_cte_is_indeterminate() {
    // Through a CTE, sqllineage still produces a mapping targeting c9 even though
    // the set operation's only output is named c1 by its leading operand, so it
    // cannot tell us the output is absent. Indeterminate is as far as dlin can
    // honestly go: NotFound would claim an absence the engine has not established.
    let result = generic(
        "WITH u AS (SELECT 1 AS c1 UNION ALL SELECT 2 AS c9) SELECT c9 FROM u",
        &["c9"],
    );
    assert_column_error(
        &result,
        "c9",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "",
    );
}

#[test]
fn nonleading_set_output_name_is_not_found() {
    // c9 is named only by the non-leading operand. Set-operation output names
    // come from c1 in the leading operand, so c9 has no output mapping.
    let result = generic("SELECT 1 AS c1 UNION ALL SELECT 2 AS c9", &["c9"]);
    assert_column_error(
        &result,
        "c9",
        ColumnLineageErrorKind::ColumnNotFound,
        "no sqllineage mapping for output 'c9'",
    );
}

#[test]
fn duplicate_output_ambiguity_takes_precedence() {
    // The name is used by two outputs while a star leaves the rest of the
    // output unknown. The duplicate is reported, not the unknown star.
    let result = generic(
        "SELECT *, id AS explicit_col, amount AS explicit_col FROM first_source",
        &["explicit_col"],
    );
    assert_column_error(
        &result,
        "explicit_col",
        ColumnLineageErrorKind::ColumnAmbiguous,
        "more than one sqllineage mapping matches output 'explicit_col'",
    );
}

#[test]
fn plain_star_without_set_operation_keeps_existing_behavior() {
    let result = generic("SELECT * FROM orders", &["id"]);
    assert_column_error(
        &result,
        "id",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "unexpanded SELECT * leaves the output columns unknown",
    );
}

#[test]
fn plain_projection_resolves_case_folded_concrete_source() {
    let result = generic("select ID as output_id from SOURCE_TABLE", &["Output_ID"]);
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "Output_ID",
        // The engine normalizes relation identifiers, while its expression
        // visitor preserves this source column spelling.
        &[("source_table", "ID")],
        TransformationType::Direct,
    );
}

#[test]
fn maps_expression_aggregate_and_case_transformations() {
    let result = generic(
        "select amount + tax as total, sum(amount) as sum_amount, case when amount > 0 then amount else 0 end as flag from orders",
        &[],
    );
    assert_no_errors(&result);
    for (column, transformation) in [
        ("total", TransformationType::Expression),
        ("sum_amount", TransformationType::Aggregation),
        ("flag", TransformationType::Conditional),
    ] {
        assert_eq!(
            entry(&result, column).transformation,
            transformation,
            "transformation for '{column}'"
        );
    }
}

#[test]
fn cast_is_direct_by_decision() {
    let result = generic("select cast(amount as int) as cast_amount from orders", &[]);
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "cast_amount",
        &[("orders", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn join_without_known_schema_reports_genuine_ambiguity() {
    let result = generic(
        "select id from left_table join right_table on left_table.id = right_table.id",
        &[],
    );
    let error = assert_column_error(
        &result,
        "id",
        ColumnLineageErrorKind::ColumnAmbiguous,
        "left_table",
    );
    assert!(error.what.contains("right_table"), "what: {}", error.what);
}

#[test]
fn join_with_known_schema_returns_canonical_column_spelling() {
    let result = analyze(
        // Resolve the unqualified reference through the schema, retaining the
        // owning table's canonical column spelling rather than the reference.
        "select id from left_table join right_table on left_table.id = right_table.other",
        DlinDialect::Generic,
        &[("left_table", &["ID"]), ("right_table", &["other"])],
        &[],
    );
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "id",
        &[("left_table", "ID")],
        TransformationType::Direct,
    );
}

#[test]
fn preserve_case_does_not_restore_differently_cased_known_relation() {
    let result = analyze(
        "select id from mixedcasetable",
        DlinDialect::MySQL,
        &[("MixedCaseTable", &["id"])],
        &[],
    );
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "id",
        &[("mixedcasetable", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn column_without_visible_binding_reports_indeterminate() {
    let result = generic("select id", &[]);
    assert_column_error(
        &result,
        "id",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "has no visible binding",
    );
}

#[test]
fn unknown_star_is_indeterminate_and_reports_star_hint() {
    let result = generic("select * from orders", &["id"]);
    let error = assert_column_error(
        &result,
        "id",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "",
    );
    // The hint is attached only when the statement has an unexpanded star.
    assert_eq!(error.hint.as_deref(), Some(STAR_HINT));
}

#[test]
fn unmapped_output_depends_on_unresolved_star_state() {
    let without_star = generic("select id from orders", &["missing"]);
    assert_column_error(
        &without_star,
        "missing",
        ColumnLineageErrorKind::ColumnNotFound,
        "no sqllineage mapping for output 'missing'",
    );

    let with_unexpanded_star = generic("select * from orders", &["missing"]);
    let error = assert_column_error(
        &with_unexpanded_star,
        "missing",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "unexpanded SELECT * leaves the output columns unknown",
    );
    assert_eq!(error.hint.as_deref(), Some(STAR_HINT));
}

#[test]
fn known_schema_expands_star_to_concrete_sources() {
    let result = analyze(
        "select * from ORDERS",
        DlinDialect::Generic,
        &[("orders", &["id", "amount"])],
        &["ID"],
    );
    // A fully expanded star leaves no open shape, so nothing is reported.
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "ID",
        &[("orders", "id")],
        TransformationType::Direct,
    );
    assert_resolved(
        &result,
        "amount",
        &[("orders", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn known_schema_set_operation_with_literal_branch_keeps_concrete_sources() {
    let result = analyze_over_sql_models(
        "SELECT * FROM ext_a UNION ALL SELECT 1 AS col_x, 2 AS col_y",
        DlinDialect::Generic,
        &[("ext_a", "SELECT col_x, col_y FROM raw_a")],
    );
    assert_no_errors(&result);
    for column in ["col_x", "col_y"] {
        assert_resolved(
            &result,
            column,
            &[("ext_a", column)],
            TransformationType::Direct,
        );
    }
}

#[test]
fn source_free_branch_does_not_discard_concrete_branch() {
    let result = analyze_over_sql_models(
        "WITH lit AS (SELECT 1 AS col_a), u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM ext_a) SELECT col_a FROM u",
        DlinDialect::Generic,
        &[("ext_a", "SELECT col_x, col_y FROM raw_a")],
    );
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "col_a",
        &[("ext_a", "col_x")],
        TransformationType::Direct,
    );
}

/// A model with only YAML columns has an exhaustive but unordered schema, so
/// a positional set operation over its star cannot be aligned by position.
#[test]
fn yaml_only_upstream_star_has_unknown_positional_alignment() {
    let result = analyze(
        "SELECT * FROM ext_a UNION ALL SELECT 1 AS col_x, 2 AS col_y",
        DlinDialect::Generic,
        &[("ext_a", &["col_x", "col_y"])],
        &[],
    );
    for column in ["col_x", "col_y"] {
        assert_column_error(
            &result,
            column,
            ColumnLineageErrorKind::ColumnIndeterminate,
            "unknown positional alignment",
        );
    }
}

#[test]
fn source_free_only_branch_resolves_without_sources() {
    let result = generic("SELECT CAST(NULL AS STRING) AS col_a", &[]);
    assert_no_errors(&result);
    assert_eq!(result.traced_columns, 1);
    assert!(
        entry(&result, "col_a").lineage.known().is_empty(),
        "unexpected sources: {:?}",
        result.columns
    );
}
