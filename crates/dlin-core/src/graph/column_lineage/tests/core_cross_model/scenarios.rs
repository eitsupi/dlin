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
    assert_eq!(result.total_columns, 2);
    assert_eq!(result.traced_columns, 2);
    assert_eq!(result.columns.len(), 2);

    let real_col = result
        .columns
        .iter()
        .find(|column| column.column == "real_col")
        .unwrap();
    assert_eq!(
        real_col
            .sources
            .iter()
            .map(|source| (source.table.as_str(), source.column.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "real_col")]
    );

    let id = result
        .columns
        .iter()
        .find(|column| column.column == "id")
        .unwrap();
    let mut id_sources = id
        .sources
        .iter()
        .map(|source| (source.table.as_str(), source.column.as_str()))
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
        .columns
        .iter()
        .find(|column| column.column == "first_item")
        .expect("first_item should be present");
    assert_eq!(
        first_item.sources.len(),
        1,
        "sources: {:?}",
        first_item.sources
    );
    let source = &first_item.sources[0];
    assert_eq!(source.column, "items_array");
    assert_eq!(source.table, "external_table_a");
    assert!(
        source
            .model_path
            .iter()
            .any(|(model, column, _)| model == "upstream_model" && column == "items_array"),
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
        .columns
        .iter()
        .find(|column| column.column == "user_id")
        .expect("user_id should be present");
    let plain_column = result
        .columns
        .iter()
        .find(|column| column.column == "plain_column")
        .expect("plain_column should be present");
    for entry in [user_id, plain_column] {
        assert_eq!(
            entry.sources,
            vec![ColumnSource {
                table: "p.d.external_table_a".to_string(),
                column: "user_id".to_string(),
                model_path: vec![(
                    "upstream_model".to_string(),
                    "user_id".to_string(),
                    TransformationType::Direct,
                )],
            }]
        );
    }

    let qualified = result
        .columns
        .iter()
        .find(|column| column.column == "qualified_field");
    let bare = result
        .columns
        .iter()
        .find(|column| column.column == "bare_field");
    let qualified = qualified.expect("qualified_field should be present");
    let bare = bare.expect("bare_field should be present");
    assert_eq!(qualified.sources, bare.sources);
    for entry in [qualified, bare] {
        assert_eq!(
            entry.sources,
            vec![ColumnSource {
                table: "p.d.upstream_model".to_string(),
                column: "event".to_string(),
                model_path: Vec::new(),
            }]
        );
    }
    assert!(
        result.errors.iter().any(|error| {
            error.what.contains("event") && error.what.contains("no visible binding")
        }),
        "row-value child should remain indeterminate: {:?}",
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
        .columns
        .iter()
        .find(|column| column.column == "item")
        .expect("item should be present");
    assert_eq!(
        item.sources
            .iter()
            .map(|source| (source.table.as_str(), source.column.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "items_array")]
    );
    assert!(
        item.sources
            .iter()
            .all(|source| !(source.table == "upstream_model" && source.column == "item")),
        "UNNEST must not fabricate an upstream_model.item source: {:?}",
        item.sources
    );
}

#[test]
fn test_cross_model_preserves_distinct_same_column_errors() {
    let manifest = duplicate_column_error_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "duplicate_target",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let duplicate_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|error| error.what.starts_with("column 'dup_col':"))
        .collect();
    assert_eq!(duplicate_errors.len(), 2, "errors: {:?}", result.errors);
    assert_eq!(
        duplicate_errors
            .iter()
            .filter(|error| error.hint.is_some())
            .count(),
        1,
        "expected one unresolved-star hint: {:?}",
        result.errors
    );
    assert_eq!(
        duplicate_errors
            .iter()
            .filter(|error| error.hint.is_none())
            .count(),
        1,
        "expected one no-mapping diagnostic: {:?}",
        result.errors
    );
    assert!(
        duplicate_errors
            .iter()
            .any(|error| error.what.contains("unexpanded SELECT *")),
        "expected unresolved-star reason: {:?}",
        result.errors
    );
    assert!(
        duplicate_errors
            .iter()
            .any(|error| error.what.contains("no sqllineage mapping")),
        "expected no-mapping reason: {:?}",
        result.errors
    );
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
        .columns
        .iter()
        .find(|column| column.column == "user_id")
        .expect("user_id should remain in the known lineage columns");
    assert!(
        user_id
            .sources
            .iter()
            .any(|source| source.table == "raw_table" && source.column == "user_id"),
        "user_id should trace to raw_table.user_id, got: {:?}; errors: {:?}",
        user_id.sources,
        result.errors
    );
    assert!(
        !result
            .columns
            .iter()
            .any(|column| column.column == "updated_at"),
        "nested field star should remain unresolved"
    );
    assert!(
        result.errors.iter().any(|error| {
            error.kind == ColumnLineageErrorKind::ColumnNotFound
                && error.what.starts_with("column 'updated_at':")
        }),
        "expected an uncertainty/error for nested field star, got: {:?}",
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
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    // Should trace through stg_orders to raw source (orders table)
    assert!(
        order_id
            .sources
            .iter()
            .any(|s| s.column == "id" && s.table.contains("orders")),
        "order_id should trace to raw orders.id, got: {:?}",
        order_id.sources
    );
    // model_path should show the hop through stg_orders
    let src = order_id.sources.iter().find(|s| s.column == "id").unwrap();
    assert!(
        src.model_path.iter().any(|(m, _, _)| m == "stg_orders"),
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
        .columns
        .iter()
        .find(|c| c.column == "customer_id")
        .unwrap();
    assert!(
        customer_id
            .sources
            .iter()
            .any(|s| s.column == "user_id" && s.table.contains("orders")),
        "customer_id should trace to raw orders.user_id, got: {:?}",
        customer_id.sources
    );
    // model_path should show both hops: orders → stg_orders
    let src = customer_id
        .sources
        .iter()
        .find(|s| s.column == "user_id")
        .unwrap();
    assert!(
        src.model_path.iter().any(|(m, _, _)| m == "orders")
            && src.model_path.iter().any(|(m, _, _)| m == "stg_orders"),
        "model_path should include orders and stg_orders, got: {:?}",
        src.model_path
    );
    // orders should come before stg_orders in the path (closer to target)
    let orders_pos = src
        .model_path
        .iter()
        .position(|(m, _, _)| m == "orders")
        .unwrap();
    let stg_pos = src
        .model_path
        .iter()
        .position(|(m, _, _)| m == "stg_orders")
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
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(
        total_amount
            .sources
            .iter()
            .any(|s| s.column == "amount" && s.table.contains("payments")),
        "total_amount should trace to raw payments.amount, got: {:?}",
        total_amount.sources
    );
    // model_path should show the hop through stg_payments
    let src = total_amount
        .sources
        .iter()
        .find(|s| s.column == "amount")
        .unwrap();
    assert!(
        src.model_path.iter().any(|(m, _, _)| m == "stg_payments"),
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

    assert_eq!(single.columns.len(), cross.columns.len());
    for (s, c) in single.columns.iter().zip(cross.columns.iter()) {
        assert_eq!(s.column, c.column);
        assert_eq!(s.sources, c.sources);
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
    let left_x = left.columns.iter().find(|c| c.column == "x").unwrap();
    assert!(
        left_x.sources.iter().any(|s| s.column == "x"),
        "left_model.x should trace through shared, got: {:?}",
        left_x.sources
    );

    // Verify right_model traces y through shared to raw_data
    let right = compute_cross_model_column_lineage(
        &manifest,
        "right_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(right.errors.is_empty(), "right errors: {:?}", right.errors);
    let right_y = right.columns.iter().find(|c| c.column == "y").unwrap();
    assert!(
        right_y.sources.iter().any(|s| s.column == "y"),
        "right_model.y should trace through shared, got: {:?}",
        right_y.sources
    );

    // Both left and right depend on 'shared' — the key assertion is that
    // resolving one does not prevent the other from being resolved.
    // With the old model-only visited set, whichever resolved first would
    // block the other from tracing through 'shared'.
    assert!(
        !left_x.sources.is_empty() && !right_y.sources.is_empty(),
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
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert_eq!(
        total_amount.sources[0].table, "stg_payments",
        "expected stg_payments, got SQL alias 'p': {:?}",
        total_amount.sources
    );

    // o.order_id → table must be "stg_orders", not the alias "o"
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert_eq!(
        order_id.sources[0].table, "stg_orders",
        "expected stg_orders, got SQL alias 'o': {:?}",
        order_id.sources
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
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(!total_amount.sources.is_empty());
    let src = &total_amount.sources[0];
    assert_ne!(src.table, "p", "source table must not be SQL alias 'p'");
    assert_ne!(
        src.table, "stg_payments",
        "cross-model must trace beyond stg_payments"
    );
    assert_eq!(src.table, "raw.payments");
    assert_eq!(src.column, "amount");
}
