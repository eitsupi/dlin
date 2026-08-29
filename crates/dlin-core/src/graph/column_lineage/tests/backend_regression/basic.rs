#[test]
fn plain_select_resolves_each_column_directly() {
    assert_resolved(
        "select id, name from raw_table",
        DlinDialect::Generic,
        None,
        "id",
        &[("raw_table", "id")],
        TransformationType::Direct,
    );
    assert_resolved(
        "select id, name from raw_table",
        DlinDialect::Generic,
        None,
        "name",
        &[("raw_table", "name")],
        TransformationType::Direct,
    );
}

#[test]
fn cte_traces_through_to_the_base_table() {
    assert_resolved(
        "with base as (select id as order_id from raw_orders) select order_id from base",
        DlinDialect::Generic,
        None,
        "order_id",
        &[("raw_orders", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn qualified_and_bare_struct_access_share_top_level_relation_contract() {
    let sql = "SELECT agg.event.qualified_field AS qualified_field, event.bare_field AS bare_field FROM upstream_model AS agg";
    for column in ["qualified_field", "bare_field"] {
        assert_resolved(
            sql,
            DlinDialect::BigQuery,
            None,
            column,
            &[("upstream_model", "event")],
            TransformationType::Direct,
        );
    }
}

#[test]
fn select_star_with_known_schema_resolves_the_named_column() {
    assert_resolved(
        "select * from known_table",
        DlinDialect::Generic,
        Some(("known_table", &["id", "amount"])),
        "id",
        &[("known_table", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn select_star_with_unknown_schema_cannot_resolve_a_column() {
    assert_failed(
        "select * from unknown_table",
        DlinDialect::Generic,
        None,
        "id",
        "cannot match output",
    );
}

/// Pins the fix for a bug where top-level `UNION`/`INTERSECT`/`EXCEPT`
/// statements were flagged as not lineage-bearing and rejected outright
/// before any column was even attempted.
#[test]
fn top_level_union_all_traces_both_operands() {
    assert_resolved(
        "select id, amt from t1 union all select id, amt from t2",
        DlinDialect::Generic,
        None,
        "id",
        &[("t1", "id"), ("t2", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn cli_ordered_schema_aligns_select_star_union_columns() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("t", ["b".to_string(), "a".to_string()]);
    assert_eq!(catalog.table_columns("t").unwrap(), ["b", "a"]);

    let sql = "select * from t union all select * from t";
    let backend = backend_for_tests(BackendId::Sqllineage);
    let duplicate_output_names = BTreeSet::new();

    for (column, expected_sources) in [
        ("b", vec![("t", "b"), ("t", "b")]),
        ("a", vec![("t", "a"), ("t", "a")]),
    ]
    .into_iter()
    {
        let outputs = [OutputColumnRequest {
            slot: AnalysisSlot(0),
            name: column.to_string(),
        }];
        let request = LineageRequest {
            sql,
            dialect: DlinDialect::Generic,
            catalog: Some(&catalog),
            outputs: &outputs,
            duplicate_output_names: &duplicate_output_names,
        };
        let analysis = backend.analyze(&request).unwrap();
        let statement = require_single_lineage_statement(analysis).unwrap();
        let (outcomes, diagnostics) = normalize_column_outcomes(&outputs, statement.columns);
        assert!(
            diagnostics.is_empty(),
            "backend contract diagnostics: {diagnostics:?}"
        );
        assert_eq!(outcomes.len(), outputs.len());
        let outcome = outcomes.into_iter().next().unwrap();
        let target = match &outcome {
            BackendColumnOutcome::Resolved(result) => &result.target,
            BackendColumnOutcome::Failed(failure) => &failure.target,
        };
        assert_eq!(target.slot, outputs[0].slot);
        assert_eq!(target.name, outputs[0].name);
        let sources = match outcome {
            BackendColumnOutcome::Resolved(result) => result
                .sources
                .into_iter()
                .map(|source| match source {
                    BackendSource::Concrete { relation, column } => (relation.render(), column),
                    other => panic!("unexpected source: {:?}", other),
                })
                .collect::<Vec<_>>(),
            other => panic!("expected resolved output, got: {:?}", other),
        };
        let expected = expected_sources
            .into_iter()
            .map(|(table, column)| (table.to_string(), column.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(sources, expected, "lineage for output column {column}");
    }
}

#[test]
fn top_level_except_traces_both_operands() {
    assert_resolved(
        "select id from t1 except select id from t2",
        DlinDialect::Generic,
        None,
        "id",
        &[("t1", "id"), ("t2", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn generic_cte_union_with_source_free_branch_resolves() {
    assert_resolved(
        r#"WITH branches AS (
            SELECT customer_id FROM upstream_model
            UNION ALL
            SELECT CAST(NULL AS STRING) AS customer_id
        ) SELECT customer_id FROM branches"#,
        DlinDialect::Generic,
        Some(("upstream_model", &["customer_id"])),
        "customer_id",
        &[("upstream_model", "customer_id")],
        TransformationType::Direct,
    );
}

#[test]
fn unresolvable_column_fails_with_column_resolution_kind() {
    let sql = "select id from t1";
    let (manifest, node_id) = make_manifest(sql, None);
    let node = manifest.nodes.get(&node_id).unwrap();
    let dialect = DlinDialect::Generic;
    let backend = backend_for_tests(BackendId::Sqllineage);
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect, &backend);

    match analyze_one_column(
        &manifest,
        &node_id,
        dialect,
        catalog.as_ref(),
        "totally_missing_column",
    ) {
        Ok(PathOutcome::Failed(message)) => {
            assert!(
                message.contains("no sqllineage mapping"),
                "message: {}",
                message
            );
        }
        other => panic!("expected a column-resolution failure, got: {:?}", other),
    }
}

/// Pins the fix for a bug where a `parse_one` failure was misclassified as
/// `BackendErrorKind::ColumnResolution` instead of `BackendErrorKind::Parse`.
#[test]
fn unparseable_sql_fails_with_parse_kind() {
    let sql = "select from from";
    let (manifest, node_id) = make_manifest(sql, None);
    let node = manifest.nodes.get(&node_id).unwrap();
    let dialect = DlinDialect::Generic;
    let backend = backend_for_tests(BackendId::Sqllineage);
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect, &backend);

    match analyze_one_column(&manifest, &node_id, dialect, catalog.as_ref(), "anything") {
        Err((kind, _message)) => {
            assert_eq!(
                kind,
                BackendErrorKind::Parse,
                "an unparseable statement must be classified as a parse failure, not a \
                 column-resolution failure"
            );
        }
        Ok(outcome) => panic!("expected the analysis to fail to parse, got {:?}", outcome),
    }
}
