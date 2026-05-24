use super::*;
#[test]
fn test_column_impact_direct_dependent() {
    // stg_orders.order_id is used by orders.order_id
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "order_id",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "orders" && ic.column == "order_id"),
        "orders.order_id should be impacted, got: {:?}",
        result.impacted_columns
    );
}

#[test]
fn test_column_impact_two_hops() {
    // stg_orders.order_id → orders.order_id → customers (via count)
    // stg_orders.customer_id → orders.customer_id → customers.customer_id
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "customer_id",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // orders.customer_id should be impacted (direct dependent)
    assert!(
        result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "orders" && ic.column == "customer_id"),
        "orders.customer_id should be impacted, got: {:?}",
        result.impacted_columns
    );
    // customers.customer_id should also be impacted (two hops)
    assert!(
        result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "customers" && ic.column == "customer_id"),
        "customers.customer_id should be impacted, got: {:?}",
        result.impacted_columns
    );
}

#[test]
fn test_column_impact_model_path() {
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "customer_id",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // customers.customer_id goes through orders
    let cust = result
        .impacted_columns
        .iter()
        .find(|ic| ic.model == "customers" && ic.column == "customer_id")
        .unwrap();
    assert!(
        cust.model_path.iter().any(|(m, _, _)| m == "orders"),
        "model_path should include orders, got: {:?}",
        cust.model_path
    );
}

#[test]
fn test_column_impact_no_dependents() {
    // customers is a leaf model — no downstream
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "customers",
        "customer_id",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(
        result.impacted_columns.is_empty(),
        "leaf model should have no impacted columns, got: {:?}",
        result.impacted_columns
    );
}

#[test]
fn test_column_impact_model_not_found() {
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "nonexistent",
        "col",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_column_impact_json_serialization() {
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "order_id",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string_pretty(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert_eq!(parsed["model"], "stg_orders");
    assert_eq!(parsed["column"], "order_id");
    assert!(parsed["impacted_columns"].is_array());
    // Verify unique_id is serialized for each impacted column
    let first = &parsed["impacted_columns"][0];
    assert!(
        first["unique_id"].is_string(),
        "unique_id should be serialized in impacted_columns"
    );
}

/// Build a manifest with two packages (pkg_a, pkg_b) that each have a model
/// named "customers" depending on the same "stg_orders" model.
#[test]
fn test_column_impact_diamond_different_columns_through_shared_model() {
    // Impact of raw_data.x should flow through shared → left_model
    // Impact of raw_data.y should flow through shared → right_model
    // Both should be detected independently despite sharing the 'shared' model.
    let manifest = make_diamond_manifest();

    let impact_x = compute_column_impact(
        &manifest,
        "raw_data",
        "x",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(impact_x.errors.is_empty(), "errors: {:?}", impact_x.errors);

    let impacted_names: Vec<(&str, &str)> = impact_x
        .impacted_columns
        .iter()
        .map(|ic| (ic.model.as_str(), ic.column.as_str()))
        .collect();
    assert!(
        impacted_names.contains(&("shared", "x")),
        "x should impact shared.x, got: {:?}",
        impacted_names
    );
    assert!(
        impacted_names.contains(&("left_model", "x")),
        "x should impact left_model.x, got: {:?}",
        impacted_names
    );
    // x should NOT impact right_model.y
    assert!(
        !impacted_names.contains(&("right_model", "y")),
        "x should not impact right_model.y"
    );

    let impact_y = compute_column_impact(
        &manifest,
        "raw_data",
        "y",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(impact_y.errors.is_empty(), "errors: {:?}", impact_y.errors);

    let impacted_names_y: Vec<(&str, &str)> = impact_y
        .impacted_columns
        .iter()
        .map(|ic| (ic.model.as_str(), ic.column.as_str()))
        .collect();
    assert!(
        impacted_names_y.contains(&("shared", "y")),
        "y should impact shared.y, got: {:?}",
        impacted_names_y
    );
    assert!(
        impacted_names_y.contains(&("right_model", "y")),
        "y should impact right_model.y, got: {:?}",
        impacted_names_y
    );
    // y should NOT impact left_model.x
    assert!(
        !impacted_names_y.contains(&("left_model", "x")),
        "y should not impact left_model.x"
    );
}

#[test]
fn test_build_downstream_model_map() {
    let manifest = make_cross_model_manifest();
    let map = build_downstream_model_map(&manifest);

    // stg_orders (by unique_id) is depended on by orders
    assert!(
        map.get("model.proj.stg_orders").map_or(false, |deps| deps
            .contains(&"model.proj.orders".to_string())),
        "stg_orders should have orders as downstream, got: {:?}",
        map.get("model.proj.stg_orders")
    );
    // orders (by unique_id) is depended on by customers
    assert!(
        map.get("model.proj.orders").map_or(false, |deps| deps
            .contains(&"model.proj.customers".to_string())),
        "orders should have customers as downstream, got: {:?}",
        map.get("model.proj.orders")
    );
    // customers has no downstream
    assert!(
        map.get("model.proj.customers").is_none(),
        "customers should have no downstream"
    );
}

// --- ColumnLineageCache tests ---
