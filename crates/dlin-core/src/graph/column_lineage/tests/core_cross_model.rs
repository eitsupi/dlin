use super::*;
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
