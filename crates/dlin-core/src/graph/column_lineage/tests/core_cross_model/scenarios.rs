use super::*;

#[test]
fn test_cross_model_bigquery_source_free_union_reaches_external_sources() {
    let manifest = source_free_union_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "union_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.listed_columns, 2);
    assert_eq!(result.value_complete_columns, 2);
    assert_eq!(result.slots().len(), 2);

    let real_col = result
        .slots()
        .iter()
        .find(|column| column.column() == "real_col")
        .unwrap();
    assert_eq!(
        real_col
            .lineage
            .known()
            .iter()
            .map(|source| (source.table.as_str(), source.column.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "real_col")]
    );

    let id = result
        .slots()
        .iter()
        .find(|column| column.column() == "id")
        .unwrap();
    let mut id_sources = id
        .lineage
        .known()
        .iter()
        .map(|source| (source.table.as_str(), source.column.value.as_str()))
        .collect::<Vec<_>>();
    id_sources.sort_unstable();
    assert_eq!(
        id_sources,
        vec![("external_table_a", "id"), ("external_table_b", "id")]
    );
}

#[test]
fn test_cross_model_bigquery_compound_field_access_reaches_external_array_column() {
    let manifest = bigquery_compound_field_access_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "array_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let first_item = result
        .slots()
        .iter()
        .find(|column| column.column() == "first_item")
        .expect("first_item should be present");
    assert_eq!(
        first_item.lineage.known().len(),
        1,
        "sources: {:?}",
        first_item.lineage.known()
    );
    let source = &first_item.lineage.known()[0];
    assert_eq!(source.column, "items_array");
    assert_eq!(source.table, "external_table_a");
    assert!(
        source
            .model_path
            .iter()
            .any(|hop| hop.output.model == "upstream_model" && hop.output.column == "items_array"),
        "expected upstream model path, got: {:?}",
        source.model_path
    );
}

#[test]
fn test_cross_model_bigquery_struct_field_access_has_one_honest_contract() {
    let manifest = bigquery_struct_field_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "quoting_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let user_id = result
        .slots()
        .iter()
        .find(|column| column.column() == "user_id")
        .expect("user_id should be present");
    let plain_column = result
        .slots()
        .iter()
        .find(|column| column.column() == "plain_column")
        .expect("plain_column should be present");
    for entry in [user_id, plain_column] {
        assert_eq!(
            entry.lineage.known(),
            vec![ColumnSource {
                table: "p.d.external_table_a".to_string(),
                column: Ident::unquoted("user_id"),
                model_path: vec![hop(
                    "model.proj.upstream_model",
                    "upstream_model",
                    0,
                    "user_id",
                    TransformationType::Direct,
                )],
            }]
        );
    }

    let qualified = result
        .slots()
        .iter()
        .find(|column| column.column() == "qualified_field");
    let bare = result
        .slots()
        .iter()
        .find(|column| column.column() == "bare_field");
    let qualified = qualified.expect("qualified_field should be present");
    let bare = bare.expect("bare_field should be present");
    assert_eq!(qualified.lineage.known(), bare.lineage.known());
    for entry in [qualified, bare] {
        assert_eq!(
            entry.lineage.known(),
            vec![ColumnSource {
                table: "p.d.upstream_model".to_string(),
                column: Ident::unquoted("event"),
                model_path: Vec::new(),
            }]
        );
    }
    for output_column in ["qualified_field", "bare_field"] {
        let diagnostic = result
            .errors
            .iter()
            .find(|error| error.column_name() == Some(output_column))
            .unwrap_or_else(|| {
                panic!(
                    "row-value diagnostic should be rebased to {output_column}: {:?}",
                    result.errors
                )
            });
        assert_eq!(diagnostic.kind, ColumnLineageErrorKind::ColumnIndeterminate);
        assert!(
            diagnostic
                .what
                .starts_with(&format!("column '{output_column}':"))
        );
        assert!(!diagnostic.is_fatal());
    }
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.column_name() == Some("event")),
        "upstream origin column must not be exposed as the output diagnostic: {:?}",
        result.errors
    );

    let public = serde_json::to_string(&result).expect("public result should serialize");
    for forbidden in ["\\\"agg.event\\\"", "\\\"event\\\"", "external_table_a.t"] {
        assert!(
            !public.contains(forbidden),
            "public result must not expose {forbidden}: {public}"
        );
    }
}

#[test]
fn test_cross_model_bigquery_row_value_child_keeps_nearest_terminal_without_fatal_error() {
    let mut manifest = bigquery_struct_field_cross_model_manifest();
    let upstream = manifest.nodes.get_mut("model.proj.upstream_model").unwrap();
    upstream.columns = ["col_a"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                crate::parser::manifest::ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    upstream.compiled_code = Some(
        "WITH latest AS (SELECT ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t) SELECT event.col_a AS col_a FROM latest".to_string(),
    );

    let downstream = manifest.nodes.get_mut("model.proj.quoting_model").unwrap();
    downstream.name = "repro_model".to_string();
    downstream.unique_id = "model.proj.repro_model".to_string();
    downstream.columns = ["col_a"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                crate::parser::manifest::ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    downstream.compiled_code = Some("SELECT col_a FROM `p`.`d`.`upstream_model`".to_string());

    let result = compute_cross_model_column_lineage(
        &manifest,
        "repro_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );
    let col_a = result
        .slots()
        .iter()
        .find(|column| column.column() == "col_a")
        .expect("col_a should remain published");
    assert_eq!(
        col_a.lineage.known().len(),
        1,
        "sources: {:?}",
        col_a.lineage.known()
    );
    assert_eq!(col_a.lineage.known()[0].table, "p.d.upstream_model");
    assert_eq!(col_a.lineage.known()[0].column, "col_a");
    assert!(col_a.lineage.known()[0].model_path.is_empty());
    // The row-value part of upstream_model.col_a stays visible as unresolved.
    assert!(col_a.lineage.is_partial());
    assert!(matches!(
        col_a.lineage.unresolved(),
        [UnresolvedContribution {
            reason: UnresolvedReason::RowValue { .. },
            origin: ModelOutputRef { model, column, .. },
            ..
        }] if model == "upstream_model" && column == "col_a"
    ));
    let diagnostic = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("col_a"))
        .expect("partial lineage should remain explicit");
    assert_eq!(diagnostic.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(!diagnostic.is_fatal());
}

#[test]
fn test_cross_model_rebases_deep_diagnostic_to_root_output() {
    let manifest = same_named_deep_error_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "target_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let x = result
        .slots()
        .iter()
        .find(|column| column.column() == "x")
        .expect("x should be present");
    assert_eq!(x.lineage.known().len(), 1);
    assert_eq!(x.lineage.known()[0].table, "p.d.middle_model");
    assert!(
        result
            .errors
            .iter()
            .all(|error| error.column_name() != Some("x")),
        "diagnostic from middle_model.y must not leak into target_model.x: {:?}",
        result.errors
    );
}

#[test]
fn test_cross_model_rebases_diagnostic_across_rename() {
    let mut manifest = same_named_deep_error_manifest();
    let middle = manifest.nodes.get_mut("model.proj.middle_model").unwrap();
    middle.compiled_code =
        Some("SELECT deep.x AS x, 1 AS y FROM `p`.`d`.`deep_model` AS deep".to_string());
    let target = manifest.nodes.get_mut("model.proj.target_model").unwrap();
    target.columns = ["z"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                crate::parser::manifest::ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    target.compiled_code = Some("SELECT x AS z FROM `p`.`d`.`middle_model`".to_string());

    let result = compute_cross_model_column_lineage(
        &manifest,
        "target_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );
    let diagnostic = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("z"))
        .expect("deep indeterminate diagnostic should follow renamed output");
    assert_eq!(diagnostic.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(diagnostic.what.starts_with("column 'z':"));
    assert!(!diagnostic.is_fatal());
    assert!(
        result
            .errors
            .iter()
            .all(|error| error.column_name() != Some("x")),
        "rebased diagnostic must not retain the upstream column: {:?}",
        result.errors
    );
}

#[test]
fn test_cross_model_bigquery_unnest_reaches_external_array_column() {
    let manifest = bigquery_unnest_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let item = result
        .slots()
        .iter()
        .find(|column| column.column() == "item")
        .expect("item should be present");
    assert_eq!(
        item.lineage
            .known()
            .iter()
            .map(|source| (source.table.as_str(), source.column.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "items_array")]
    );
    assert!(
        item.lineage
            .known()
            .iter()
            .all(|source| !(source.table == "upstream_model" && source.column == "item")),
        "UNNEST must not fabricate an upstream_model.item source: {:?}",
        item.lineage.known()
    );
}

#[test]
fn test_cross_model_rebases_distinct_same_column_errors_to_target_outputs() {
    let manifest = duplicate_column_error_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "duplicate_target",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // Each upstream diagnostic is rebased to the target output that reads it,
    // identified by its slot, and the two are not merged.
    let left_error = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("dup_col"))
        .expect("left diagnostic should remain scoped to dup_col");
    assert_eq!(left_error.slot(), Some(OutputSlotId::new(0)));
    assert_eq!(left_error.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(left_error.what.starts_with("column 'dup_col':"));
    assert!(left_error.what.contains("unexpanded wildcard"));

    let right_error = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("other_col"))
        .expect("right diagnostic should be rebased to other_col");
    assert_eq!(right_error.slot(), Some(OutputSlotId::new(1)));
    assert_eq!(right_error.kind, ColumnLineageErrorKind::ColumnAmbiguous);
    assert!(right_error.what.starts_with("column 'other_col':"));
    assert!(right_error.what.contains("is ambiguous between"));
    assert_eq!(result.errors.len(), 2, "errors: {:?}", result.errors);
}

#[test]
fn test_cross_model_bigquery_nested_star_keeps_known_user_id_lineage() {
    // BigQuery-specific EXCEPT and nested field-star syntax intentionally
    // leaves some STRUCT expansion unresolved; known scalar lineage must
    // remain available rather than being discarded with those errors.
    let manifest = bigquery_nested_star_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let user_id = result
        .slots()
        .iter()
        .find(|column| column.column() == "user_id")
        .expect("user_id should remain in the known lineage columns");
    assert!(
        user_id
            .lineage
            .known()
            .iter()
            .any(|source| source.table == "raw_table" && source.column == "user_id"),
        "user_id should trace to raw_table.user_id, got: {:?}; errors: {:?}",
        user_id.lineage.known(),
        result.errors
    );
    assert!(
        !result
            .slots()
            .iter()
            .any(|column| column.column() == "updated_at"),
        "nested field star should remain unresolved"
    );
    // The nested field star leaves the output open, and the YAML column it
    // might provide is reported as not enumerated rather than as an output.
    assert!(matches!(
        result.output.as_ref().map(|output| &output.shape),
        Some(OutputShape::Open { .. })
    ));
    assert!(
        result.errors.iter().any(|error| {
            error.kind == ColumnLineageErrorKind::DeclaredColumnNotEnumerated
                && error.what.contains("column updated_at ")
                && !error.is_fatal()
        }),
        "expected a non-fatal declaration diagnostic for updated_at, got: {:?}",
        result.errors
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.column_name() == Some("event")),
        "diagnostics for EXCEPT-ed event must not leak: {:?}",
        result.errors
    );
}

/// Build a manifest with 3 levels: customers → orders → stg_orders → raw.orders
#[test]
fn test_cross_model_single_hop() {
    // orders.order_id → stg_orders.order_id → raw.orders.id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    // Should trace through stg_orders to raw source (orders table)
    assert!(
        order_id
            .lineage
            .known()
            .iter()
            .any(|s| s.column == "id" && s.table.contains("orders")),
        "order_id should trace to raw orders.id, got: {:?}",
        order_id.lineage.known()
    );
    // model_path should show the hop through stg_orders
    let src = order_id
        .lineage
        .known()
        .iter()
        .find(|s| s.column == "id")
        .unwrap();
    assert!(
        src.model_path
            .iter()
            .any(|hop| hop.output.model == "stg_orders"),
        "model_path should include stg_orders, got: {:?}",
        src.model_path
    );
}

#[test]
fn test_cross_model_two_hops() {
    // customers.customer_id → orders.customer_id → stg_orders.customer_id → raw.orders.user_id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "customers",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let customer_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "customer_id")
        .unwrap();
    assert!(
        customer_id
            .lineage
            .known()
            .iter()
            .any(|s| s.column == "user_id" && s.table.contains("orders")),
        "customer_id should trace to raw orders.user_id, got: {:?}",
        customer_id.lineage.known()
    );
    // model_path should show both hops: orders → stg_orders
    let src = customer_id
        .lineage
        .known()
        .iter()
        .find(|s| s.column == "user_id")
        .unwrap();
    assert!(
        src.model_path
            .iter()
            .any(|hop| hop.output.model == "orders")
            && src
                .model_path
                .iter()
                .any(|hop| hop.output.model == "stg_orders"),
        "model_path should include orders and stg_orders, got: {:?}",
        src.model_path
    );
    // orders should come before stg_orders in the path (closer to target)
    let orders_pos = src
        .model_path
        .iter()
        .position(|hop| hop.output.model == "orders")
        .unwrap();
    let stg_pos = src
        .model_path
        .iter()
        .position(|hop| hop.output.model == "stg_orders")
        .unwrap();
    assert!(
        orders_pos < stg_pos,
        "orders should precede stg_orders in path"
    );
}

#[test]
fn test_cross_model_join_sources() {
    // orders.total_amount → stg_payments.amount → raw.payments.amount
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert!(
        total_amount
            .lineage
            .known()
            .iter()
            .any(|s| s.column == "amount" && s.table.contains("payments")),
        "total_amount should trace to raw payments.amount, got: {:?}",
        total_amount.lineage.known()
    );
    // model_path should show the hop through stg_payments
    let src = total_amount
        .lineage
        .known()
        .iter()
        .find(|s| s.column == "amount")
        .unwrap();
    assert!(
        src.model_path
            .iter()
            .any(|hop| hop.output.model == "stg_payments"),
        "model_path should include stg_payments, got: {:?}",
        src.model_path
    );
}

#[test]
fn test_cross_model_source_table_is_leaf() {
    // stg_orders directly references a source — cross-model should not change the result
    let manifest = make_cross_model_manifest();
    let single = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let cross = compute_cross_model_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(single.slots().len(), cross.slots().len());
    for (s, c) in single.slots().iter().zip(cross.slots().iter()) {
        assert_eq!(s.name, c.name);
        assert_eq!(s.lineage.known(), c.lineage.known());
    }
}

#[test]
fn test_cross_model_model_not_found() {
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "nonexistent",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_cross_model_diamond_different_columns_through_shared_model() {
    // In a diamond DAG, different columns (x and y) flow through a shared
    // upstream model. Both should be resolved independently — the visited set
    // must not truncate the second column's path through the shared model.
    let manifest = make_diamond_manifest();

    // Verify left_model traces x through shared to raw_data
    let left = compute_cross_model_column_lineage(
        &manifest,
        "left_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(left.errors.is_empty(), "left errors: {:?}", left.errors);
    let left_x = left.slots().iter().find(|c| c.column() == "x").unwrap();
    assert!(
        left_x.lineage.known().iter().any(|s| s.column == "x"),
        "left_model.x should trace through shared, got: {:?}",
        left_x.lineage.known()
    );

    // Verify right_model traces y through shared to raw_data
    let right = compute_cross_model_column_lineage(
        &manifest,
        "right_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(right.errors.is_empty(), "right errors: {:?}", right.errors);
    let right_y = right.slots().iter().find(|c| c.column() == "y").unwrap();
    assert!(
        right_y.lineage.known().iter().any(|s| s.column == "y"),
        "right_model.y should trace through shared, got: {:?}",
        right_y.lineage.known()
    );

    // Both left and right depend on 'shared' — the key assertion is that
    // resolving one does not prevent the other from being resolved.
    // With the old model-only visited set, whichever resolved first would
    // block the other from tracing through 'shared'.
    assert!(
        !left_x.lineage.known().is_empty() && !right_y.lineage.known().is_empty(),
        "both paths through shared should resolve independently"
    );
}

// The orders model uses aliases (stg_orders AS o, stg_payments AS p).
// These two tests guard against regressions where collect_leaves returns
// the SQL alias instead of the actual model name.

#[test]
fn test_join_alias_resolves_to_model_name() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // p.amount → table must be "stg_payments", not the alias "p"
    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert_eq!(
        total_amount.lineage.known()[0].table,
        "stg_payments",
        "expected stg_payments, got SQL alias 'p': {:?}",
        total_amount.lineage.known()
    );

    // o.order_id → table must be "stg_orders", not the alias "o"
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    assert_eq!(
        order_id.lineage.known()[0].table,
        "stg_orders",
        "expected stg_orders, got SQL alias 'o': {:?}",
        order_id.lineage.known()
    );
}

#[test]
fn test_cross_model_join_alias_traces_to_raw_source() {
    // Cross-model lineage must follow through aliases to reach raw sources.
    // Before the fix, "p" didn't match upstream_models so the trace stopped early.
    let manifest = make_test_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert!(!total_amount.lineage.known().is_empty());
    let src = &total_amount.lineage.known()[0];
    assert_ne!(src.table, "p", "source table must not be SQL alias 'p'");
    assert_ne!(
        src.table, "stg_payments",
        "cross-model must trace beyond stg_payments"
    );
    assert_eq!(src.table, "raw.payments");
    assert_eq!(src.column, "amount");
}

#[test]
fn test_cross_model_case_insensitive_catalog_match_uses_upstream_output_name() {
    let mut manifest = make_cross_model_manifest();

    // The upstream SQL and manifest expose ID, while the downstream SQL uses
    // lowercase id. stg_payments has no matching column, so its presence in
    // the join lets the catalog resolve id to stg_orders.ID unambiguously.
    let upstream = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    upstream.columns.shift_remove("order_id");
    upstream.columns.insert(
        "ID".to_string(),
        ManifestColumn {
            name: "ID".to_string(),
            quote: None,
        },
    );
    upstream.compiled_code =
        Some("select id as ID, user_id as customer_id, order_date, status from orders".to_string());

    let downstream = manifest.nodes.get_mut("model.proj.orders").unwrap();
    downstream.columns = ["chosen"]
        .into_iter()
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
    downstream.compiled_code = Some(
        "SELECT id AS chosen FROM stg_orders JOIN stg_payments ON stg_orders.ID = stg_payments.order_id"
            .to_string(),
    );

    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let chosen = result
        .slots()
        .iter()
        .find(|column| column.column() == "chosen")
        .expect("chosen should be present");
    assert_eq!(
        chosen.lineage.known().len(),
        1,
        "sources: {:?}",
        chosen.lineage.known()
    );
    let source = &chosen.lineage.known()[0];
    // The raw source is referenced as orders in the upstream SQL.
    assert_eq!(source.table, "orders");
    assert_eq!(source.column, "id");
    assert!(
        source
            .model_path
            .iter()
            .any(|hop| hop.output.model == "stg_orders" && hop.output.column == "ID"),
        "model_path should retain the upstream output name ID: {:?}",
        source.model_path
    );
}
