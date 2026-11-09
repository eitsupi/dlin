use super::*;

#[test]
fn plain_select_resolves_each_column_directly() {
    let result = generic("select id, name from raw_table", &[]);
    assert_no_errors(&result);
    assert_counts(&result, 2, 2, 0);
    assert_resolved(
        &result,
        "id",
        &[("raw_table", "id")],
        TransformationType::Direct,
    );
    assert_resolved(
        &result,
        "name",
        &[("raw_table", "name")],
        TransformationType::Direct,
    );
}

#[test]
fn cte_traces_through_to_the_base_table() {
    let result = generic(
        "with base as (select id as order_id from raw_orders) select order_id from base",
        &[],
    );
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "order_id",
        &[("raw_orders", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn qualified_and_bare_struct_access_share_top_level_relation_contract() {
    let result = analyze(
        "SELECT agg.event.qualified_field AS qualified_field, event.bare_field AS bare_field FROM upstream_model AS agg",
        DlinDialect::BigQuery,
        &[],
        &[],
    );
    assert_no_errors(&result);
    for column in ["qualified_field", "bare_field"] {
        assert_resolved(
            &result,
            column,
            &[("upstream_model", "event")],
            TransformationType::Direct,
        );
    }
}

#[test]
fn select_star_with_known_schema_resolves_the_named_column() {
    let result = analyze(
        "select * from known_table",
        DlinDialect::Generic,
        &[("known_table", &["id", "amount"])],
        &[],
    );
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "id",
        &[("known_table", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn select_star_with_unknown_schema_lists_nothing_and_stays_open() {
    // The YAML declares `id`, but the SQL output does not enumerate it: it is
    // reported, not added to the output.
    let result = generic("select * from unknown_table", &["id"]);
    assert_counts(&result, 0, 0, 0);
    let output = result.output.as_ref().expect("the SQL was analysed");
    assert!(output.columns.is_empty());
    assert!(matches!(
        &output.shape,
        OutputShape::Open { unexpanded } if unexpanded.len() == 1
    ));
    assert_declared_difference(
        &result,
        ColumnLineageErrorKind::DeclaredColumnNotEnumerated,
        "id",
        "is declared in YAML but is not among the enumerated SQL outputs",
    );
}

/// Pins the fix for a bug where top-level `UNION`/`INTERSECT`/`EXCEPT`
/// statements were flagged as not lineage-bearing and rejected outright
/// before any column was even attempted.
#[test]
fn top_level_union_all_traces_both_operands() {
    let result = generic(
        "select id, amt from t1 union all select id, amt from t2",
        &[],
    );
    assert_no_errors(&result);
    assert_counts(&result, 2, 2, 0);
    assert_resolved(
        &result,
        "id",
        &[("t1", "id"), ("t2", "id")],
        TransformationType::Direct,
    );
}

/// A schema given to the CLI keeps its declared column order, which a
/// manifest's unordered YAML columns cannot express, so this goes through the
/// CLI's trace entry point rather than a manifest.
#[test]
fn cli_ordered_schema_aligns_select_star_union_columns() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("t", ["b".to_string(), "a".to_string()]);
    assert_eq!(catalog.table_columns("t").unwrap(), ["b", "a"]);

    for (ordinal, column) in ["b", "a"].into_iter().enumerate() {
        let json = debug_trace_column_json(
            "select * from t union all select * from t",
            DlinDialect::Generic,
            Some(&catalog),
            column,
            false,
        )
        .unwrap();
        let trace: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(trace["shape"], "Closed", "shape for output column {column}");
        let mappings = trace["mappings"].as_array().unwrap();
        assert_eq!(mappings.len(), 1, "mappings for output column {column}");
        assert_eq!(mappings[0]["ordinal"], ordinal, "ordinal of {column}");
        assert_eq!(mappings[0]["transform"], "Direct");
        let sources = mappings[0]["lineage"]["Complete"].as_array().unwrap();
        let sources: Vec<_> = sources
            .iter()
            .map(|source| {
                (
                    source["relation"]["name"]["value"].as_str().unwrap(),
                    source["column"]["value"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            sources,
            [("t", column)],
            "lineage for output column {column}"
        );
    }
}

#[test]
fn top_level_except_traces_only_the_left_operand() {
    let result = generic("select id from t1 except select id from t2", &[]);
    assert_no_errors(&result);
    assert_resolved(&result, "id", &[("t1", "id")], TransformationType::Direct);
}

#[test]
fn generic_cte_union_with_source_free_branch_resolves() {
    let result = analyze(
        r#"WITH branches AS (
            SELECT customer_id FROM upstream_model
            UNION ALL
            SELECT CAST(NULL AS STRING) AS customer_id
        ) SELECT customer_id FROM branches"#,
        DlinDialect::Generic,
        &[("upstream_model", &["customer_id"])],
        &[],
    );
    assert_no_errors(&result);
    assert_resolved(
        &result,
        "customer_id",
        &[("upstream_model", "customer_id")],
        TransformationType::Direct,
    );
}

#[test]
fn yaml_only_column_is_reported_and_not_added_to_the_output() {
    let result = generic("select id from t1", &["totally_missing_column"]);
    assert_eq!(result.names(), ["id"]);
    assert_counts(&result, 1, 1, 0);
    assert_declared_difference(
        &result,
        ColumnLineageErrorKind::DeclaredColumnNotInOutput,
        "totally_missing_column",
        "is declared in YAML but is not in the SQL output",
    );
    assert_resolved(&result, "id", &[("t1", "id")], TransformationType::Direct);
}

/// Pins the fix for a bug where SQL that cannot be analyzed was reported as
/// an indeterminate column-resolution failure instead of a parse failure.
#[test]
fn unparseable_sql_fails_with_parse_kind() {
    let result = generic("select from from", &["anything"]);
    assert_parse_failure(&result, "empty projection");
    assert!(
        result
            .errors
            .iter()
            .all(|error| error.kind != ColumnLineageErrorKind::ColumnIndeterminate),
        "an unparseable statement must be a parse failure, not a column-resolution failure"
    );

    let result = generic("select * from", &["anything"]);
    assert_parse_failure(&result, "");
}
