use super::*;

#[test]
fn test_rename_detection() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.model, "stg_orders");
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 4);

    // order_id comes from orders.id (renamed)
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert!(!order_id.sources.is_empty(), "order_id should have sources");
    assert_eq!(order_id.sources[0].column, "id");
    // Rename is classified as direct (the rename is evident from column name difference)
    assert_eq!(order_id.transformation, TransformationType::Direct);

    // customer_id comes from orders.user_id (renamed)
    let customer_id = result
        .columns
        .iter()
        .find(|c| c.column == "customer_id")
        .unwrap();
    assert_eq!(customer_id.sources[0].column, "user_id");
    assert_eq!(customer_id.transformation, TransformationType::Direct);
}

#[test]
fn test_join_lineage() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.model, "orders");
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 3);

    // total_amount is aliased from p.amount
    let total_amount = result
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(!total_amount.sources.is_empty());
    assert_eq!(total_amount.sources[0].column, "amount");

    // order_id comes from o.order_id
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert_eq!(order_id.sources[0].column, "order_id");
}

#[test]
fn test_model_not_found() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "nonexistent",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.columns.len(), 0);
    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_no_compiled_code() {
    let mut manifest = make_test_manifest();
    // Remove compiled_code from stg_orders
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = None;
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.columns.is_empty());
    assert!(result.errors[0].what.contains("compiled_code"));
}

#[test]
fn test_no_yaml_columns_uses_sql_inference() {
    // When YAML columns are empty, column names should be inferred from compiled SQL
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .columns
        .clear();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // SQL inference should find: customer_id, order_date, order_id, status
    assert_eq!(
        result.columns.len(),
        4,
        "should infer 4 columns from SQL: {:?}",
        result.errors
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn test_no_columns_and_no_sql() {
    // When YAML columns are empty AND compiled SQL cannot be parsed, error
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.clear();
    node.compiled_code = Some("INVALID SQL %%%".to_string());
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.columns.is_empty());
    assert!(!result.errors.is_empty());
    assert!(
        result.errors[0]
            .what
            .contains("could not determine output columns")
    );
}

#[test]
fn test_json_serialization() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string_pretty(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["model"], "stg_orders");
    assert!(parsed["columns"].is_array());
}

// --- Cross-model lineage tests ---

#[test]
fn test_partial_failure_summary() {
    // Model with some columns that can be traced and some that fail
    let mut manifest = make_test_manifest();
    // Add a column to stg_orders that doesn't exist in the SQL
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.insert(
        "nonexistent_col".to_string(),
        ManifestColumn {
            name: "nonexistent_col".to_string(),
        },
    );
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // Should have 4 successful columns and 1 failed
    assert_eq!(result.columns.len(), 4);
    assert_eq!(result.traced_columns, 4);
    assert_eq!(result.total_columns, 5);
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.what.contains("nonexistent_col")),
        "should include per-column error, got: {:?}",
        result.errors
    );
    assert!(
        result
            .errors
            .iter()
            .all(|e| matches!(e.kind, ColumnLineageErrorKind::ColumnNotFound)),
        "all errors should be column_not_found, got: {:?}",
        result.errors
    );
}

#[test]
fn test_transformation_classification() {
    // customers model has: customer_id (direct) and order_count (aggregation via count(*))
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "customers",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let customer_id = result
        .columns
        .iter()
        .find(|c| c.column == "customer_id")
        .unwrap();
    assert_eq!(
        customer_id.transformation,
        TransformationType::Direct,
        "customer_id should be direct"
    );

    let order_count = result
        .columns
        .iter()
        .find(|c| c.column == "order_count")
        .unwrap();
    assert_eq!(
        order_count.transformation,
        TransformationType::Aggregation,
        "order_count (count(*)) should be aggregation"
    );
}

#[test]
fn test_source_table_has_empty_model_path() {
    // stg_orders references raw source directly — model_path should be empty
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    for entry in &result.columns {
        for source in &entry.sources {
            assert!(
                source.model_path.is_empty(),
                "source {}.{} should have empty model_path (no cross-model hops), got: {:?}",
                source.table,
                source.column,
                source.model_path
            );
        }
    }
}

#[test]
fn test_json_includes_new_fields() {
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "customers",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string_pretty(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // transformation field should be present on all columns
    for col in parsed["columns"].as_array().unwrap() {
        assert!(
            col["transformation"].is_string(),
            "transformation should be serialized: {:?}",
            col
        );
    }

    // model_path should be present on sources with cross-model hops
    let customer_id = parsed["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["column"] == "customer_id")
        .unwrap();
    let first_source = &customer_id["sources"][0];
    assert!(
        first_source["model_path"].is_array(),
        "model_path should be present for cross-model source: {:?}",
        first_source
    );
}

#[test]
fn test_traced_total_columns_success() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert_eq!(result.total_columns, 4);
    assert_eq!(result.traced_columns, 4);
}

#[test]
fn test_scalar_function_transformation() {
    // Bug: UPPER(x) and CONCAT(x,y) were classified as unknown instead of expression.
    // COALESCE has a dedicated AST variant and was always correct; UPPER uses the
    // specialized Upper variant and CONCAT uses the generic Function variant.
    let manifest = make_transformation_manifest();
    let result = compute_column_lineage(
        &manifest,
        "scalar_funcs",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let find = |name: &str| {
        result
            .columns
            .iter()
            .find(|c| c.column == name)
            .unwrap_or_else(|| panic!("{name} not found"))
            .transformation
            .clone()
    };

    assert_eq!(
        find("col_upper"),
        TransformationType::Expression,
        "UPPER should be expression"
    );
    assert_eq!(
        find("col_concat"),
        TransformationType::Expression,
        "CONCAT should be expression"
    );
    assert_eq!(
        find("col_coalesce"),
        TransformationType::Expression,
        "COALESCE should remain expression"
    );
}

#[test]
fn test_cte_passthrough_inherits_transformation() {
    // Bug: when a CTE computes an expression and the next SELECT references it by
    // name (pass-through), the transformation type was incorrectly reported as direct.
    let manifest = make_transformation_manifest();

    // UPPER → pass-through: should be expression, not direct
    let result_upper = compute_column_lineage(
        &manifest,
        "passthrough_upper",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(
        result_upper.errors.is_empty(),
        "errors: {:?}",
        result_upper.errors
    );
    let status_upper = result_upper
        .columns
        .iter()
        .find(|c| c.column == "status_upper")
        .expect("status_upper not found");
    assert_eq!(
        status_upper.transformation,
        TransformationType::Expression,
        "UPPER pass-through should be expression, not direct"
    );

    // COALESCE → pass-through: should be expression, not direct
    let result_coalesce = compute_column_lineage(
        &manifest,
        "passthrough_coalesce",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(
        result_coalesce.errors.is_empty(),
        "errors: {:?}",
        result_coalesce.errors
    );
    let status_coalesced = result_coalesce
        .columns
        .iter()
        .find(|c| c.column == "status_coalesced")
        .expect("status_coalesced not found");
    assert_eq!(
        status_coalesced.transformation,
        TransformationType::Expression,
        "COALESCE pass-through should be expression, not direct"
    );
}

#[test]
fn test_traced_total_columns_partial_failure() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.insert(
        "nonexistent_col".to_string(),
        ManifestColumn {
            name: "nonexistent_col".to_string(),
        },
    );
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert_eq!(result.total_columns, 5);
    assert_eq!(result.traced_columns, 4);
}

#[test]
fn test_traced_total_columns_model_not_found() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "nonexistent",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert_eq!(result.total_columns, 0);
    assert_eq!(result.traced_columns, 0);
}

#[test]
fn test_traced_total_columns_in_json() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["traced_columns"], 4);
    assert_eq!(parsed["total_columns"], 4);
}

// --- Regression tests for known issues ---
