use super::*;

#[test]
fn unknown_set_operation_outputs_are_listed_only_as_far_as_they_are_proven() {
    // A leading star of unknown schema decides the names of the whole
    // operation, so nothing is listed and the shape is open.
    let result = generic(
        "SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source",
        &[],
    );
    assert_no_errors(&result);
    assert!(result.slots().is_empty());
    assert!(matches!(
        &result.output.as_ref().unwrap().shape,
        OutputShape::Open { unexpanded } if unexpanded.len() == 1
    ));

    // The names come from the leading operand. The right operand's `id, *`
    // cannot be aligned by position, which leaves `explicit_col` partial while
    // `id` is complete.
    let result = generic(
        "SELECT id, 1 AS explicit_col FROM raw.orders UNION SELECT id, * FROM some_unknown_source",
        &[],
    );
    assert_eq!(result.names(), ["id", "explicit_col"]);
    assert_counts(&result, 2, 1, 1);
    assert_column_error(
        &result,
        "explicit_col",
        ColumnLineageErrorKind::ColumnIndeterminate,
        "",
    );

    // Through a CTE the star's columns stay behind the wildcard.
    let result = generic(
        "WITH u AS (SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source) SELECT id, explicit_col FROM u",
        &[],
    );
    assert_eq!(result.names(), ["id", "explicit_col"]);
    assert_counts(&result, 2, 0, 2);
    for name in ["id", "explicit_col"] {
        assert_column_error(
            &result,
            name,
            ColumnLineageErrorKind::ColumnIndeterminate,
            "",
        );
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
fn nonleading_set_output_name_is_declared_only() {
    // c9 is named only by the non-leading operand. Set-operation output names
    // come from c1 in the leading operand, so c9 is not an output; the YAML
    // declaration of it is reported.
    let result = generic("SELECT 1 AS c1 UNION ALL SELECT 2 AS c9", &["c9"]);
    assert_eq!(result.names(), ["c1"]);
    assert_declared_difference(
        &result,
        ColumnLineageErrorKind::DeclaredColumnNotInOutput,
        "c9",
        "is declared in YAML but is not in the SQL output",
    );
}

#[test]
fn duplicate_names_next_to_a_star_are_two_slots_after_the_unknown_segment() {
    // The name is used by two outputs while a star leaves the rest of the
    // output unknown. Both outputs are listed, in order, without a proven
    // position because the star precedes them.
    let result = generic(
        "SELECT *, id AS explicit_col, amount AS explicit_col FROM first_source",
        &["explicit_col"],
    );
    assert_no_errors(&result);
    assert_eq!(result.names(), ["explicit_col", "explicit_col"]);
    assert_source_pairs(&result.slots()[0], &[("first_source", "id")], "first");
    assert_source_pairs(&result.slots()[1], &[("first_source", "amount")], "second");
    assert!(result.slots().iter().all(|slot| slot.ordinal.is_none()));
    let output = result.output.as_ref().unwrap();
    assert!(matches!(output.shape, OutputShape::Open { .. }));
    assert!(matches!(
        output.lookup(&Ident::unquoted("explicit_col"), DlinDialect::Generic),
        SlotLookup::Ambiguous(ids) if ids.len() == 2
    ));
}

#[test]
fn plain_projection_resolves_case_folded_concrete_source() {
    // The declared `Output_ID` and the output `output_id` are the same column
    // under the dialect's rules, so nothing is reported.
    let result = generic("select ID as output_id from SOURCE_TABLE", &["Output_ID"]);
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "output_id",
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
fn declared_only_column_wording_depends_on_the_shape() {
    let closed = generic("select id from orders", &["missing"]);
    assert_declared_difference(
        &closed,
        ColumnLineageErrorKind::DeclaredColumnNotInOutput,
        "missing",
        "is declared in YAML but is not in the SQL output",
    );
    assert_eq!(closed.errors[0].hint, None);

    // With an unexpanded star the column is not proven absent, so the wording
    // does not say it is missing and the star hint is attached.
    let open = generic("select * from orders", &["missing"]);
    assert_declared_difference(
        &open,
        ColumnLineageErrorKind::DeclaredColumnNotEnumerated,
        "missing",
        "is declared in YAML but is not among the enumerated SQL outputs",
    );
    assert!(!open.errors[0].what.contains("not in the SQL output"));
    assert_eq!(open.errors[0].hint.as_deref(), Some(STAR_HINT));
}

#[test]
fn known_schema_expands_star_to_concrete_sources() {
    let result = analyze(
        "select * from ORDERS",
        DlinDialect::Generic,
        &[("orders", &["id", "amount"])],
        &["ID"],
    );
    // A fully expanded star leaves no open shape, so nothing is reported, and
    // the declared `ID` is the output `id` under the dialect's rules.
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "id",
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
    assert_counts(&result, 1, 1, 0);
    assert!(
        entry(&result, "col_a").lineage.known().is_empty(),
        "unexpected sources: {:?}",
        result.slots()
    );
}
