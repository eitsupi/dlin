use super::*;
#[test]
fn test_rename_detection() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.model, "stg_orders");
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.slots().len(), 4);

    // order_id comes from orders.id (renamed)
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    assert!(
        !order_id.lineage.known().is_empty(),
        "order_id should have sources"
    );
    assert_eq!(order_id.lineage.known()[0].column, "id");
    // Rename is classified as direct (the rename is evident from column name difference)
    assert_eq!(order_id.transformation, TransformationType::Direct);

    // customer_id comes from orders.user_id (renamed)
    let customer_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "customer_id")
        .unwrap();
    assert_eq!(customer_id.lineage.known()[0].column, "user_id");
    assert_eq!(customer_id.transformation, TransformationType::Direct);
}

#[test]
fn test_join_lineage() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.model, "orders");
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.slots().len(), 3);

    // total_amount is aliased from p.amount
    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert!(!total_amount.lineage.known().is_empty());
    assert_eq!(total_amount.lineage.known()[0].column, "amount");

    // order_id comes from o.order_id
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    assert_eq!(order_id.lineage.known()[0].column, "order_id");
}

#[test]
fn test_model_not_found() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "nonexistent",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.slots().len(), 0);
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
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.slots().is_empty());
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
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // SQL inference should find: customer_id, order_date, order_id, status
    assert_eq!(
        result.slots().len(),
        4,
        "should infer 4 columns from SQL: {:?}",
        result.errors
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
}

#[test]
fn test_no_columns_and_no_sql() {
    // When compiled SQL cannot be parsed there is no output, with or without
    // YAML columns.
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.clear();
    node.compiled_code = Some("INVALID SQL %%%".to_string());
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.output, None);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].kind, ColumnLineageErrorKind::ParseFailure);
    assert_eq!(
        (
            result.listed_columns,
            result.value_complete_columns,
            result.value_partial_columns
        ),
        (0, 0, 0)
    );
}

#[test]
fn test_yaml_only_column_is_reported_and_not_added_to_the_sql_output() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns = ["yaml_only"]
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
    node.compiled_code = Some("select id as sql_only from raw.orders".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.names(), ["sql_only"]);
    assert_eq!(result.listed_columns, 1);
    let [error] = result.errors.as_slice() else {
        panic!("expected one diagnostic: {:?}", result.errors);
    };
    assert_eq!(
        error.kind,
        ColumnLineageErrorKind::DeclaredColumnNotInOutput
    );
    assert!(error.what.contains("yaml_only"), "what: {}", error.what);
    assert!(!error.is_fatal());
}

#[test]
fn test_parse_failure_does_not_substitute_yaml_output_names() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns = ["yaml_only"]
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
    node.compiled_code = Some("INVALID SQL %%%".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.output, None);
    assert_eq!(result.listed_columns, 0);
    assert_eq!(result.errors[0].kind, ColumnLineageErrorKind::ParseFailure);
}

#[test]
fn test_json_serialization() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string_pretty(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["model"], "stg_orders");
    assert!(parsed["output"]["columns"].is_array());
    assert_eq!(parsed["listed_columns"], 4);
    assert_eq!(parsed["output"]["shape"]["kind"], "closed");
}

// --- Cross-model lineage tests ---

#[test]
fn test_json_includes_new_fields() {
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "customers",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string_pretty(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();

    // transformation field should be present on all columns
    for col in parsed["output"]["columns"].as_array().unwrap() {
        assert!(
            col["transformation"].is_string(),
            "transformation should be serialized: {:?}",
            col
        );
    }

    // model_path should be present on sources with cross-model hops
    let customer_id = parsed["output"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"]["ident"]["value"] == "customer_id")
        .unwrap();
    let first_source = &customer_id["lineage"]["known"][0];
    assert!(
        first_source["model_path"].is_array(),
        "model_path should be present for cross-model source: {:?}",
        first_source
    );
}
