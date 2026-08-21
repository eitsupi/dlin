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
fn test_cte_select_star() {
    // CTE + SELECT * now works with the expand_cte_stars preprocessing
    let sql = r#"with renamed as (select id as customer_id from source) select * from renamed"#;
    let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
    let result = polyglot_sql::lineage::lineage("customer_id", &expr, None, false);
    assert!(
        result.is_ok(),
        "CTE + SELECT * should work: {:?}",
        result.err()
    );
    let node = result.unwrap();
    assert_eq!(node.name, "customer_id");
}

#[test]
fn test_nested_cte_select_star() {
    // Nested CTE: cte2 references cte1 via SELECT *
    let sql = r#"
            with
                cte1 as (select id as order_id, amount from raw_orders),
                cte2 as (select * from cte1)
            select * from cte2
        "#;
    let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
    let result = polyglot_sql::lineage::lineage("order_id", &expr, None, false);
    assert!(
        result.is_ok(),
        "nested CTE + SELECT * should work: {:?}",
        result.err()
    );
}

#[test]
fn test_cte_select_star_in_manifest_model() {
    // Integration test: typical dbt pattern with CTE + SELECT *
    let mut manifest = make_test_manifest();
    let sql = r#"with renamed as (
            select
                id as order_id,
                user_id as customer_id,
                order_date,
                status
            from raw.orders
        )
        select * from renamed"#;
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(sql.to_string());
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 4);

    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert_eq!(order_id.sources[0].column, "id");
}

#[test]
fn test_schema_resolves_cte_star_from_external_table() {
    // Test that lineage_with_schema can resolve columns through CTEs that
    // reference external tables registered in the schema.
    let sql = r#"with
orders as (
    select * from stg_orders
),
enriched as (
    select orders.*, 'extra' as extra_col
    from orders
)
select * from enriched"#;
    let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();

    let mut schema = polyglot_sql::MappingSchema::new();
    let cols = vec![
        (
            "order_id".to_string(),
            polyglot_sql::expressions::DataType::Unknown,
        ),
        (
            "customer_id".to_string(),
            polyglot_sql::expressions::DataType::Unknown,
        ),
        (
            "order_total".to_string(),
            polyglot_sql::expressions::DataType::Unknown,
        ),
    ];
    schema.add_table("stg_orders", &cols, None).unwrap();

    let result = polyglot_sql::lineage::lineage_with_schema(
        "order_id",
        &expr,
        Some(&schema as &dyn polyglot_sql::Schema),
        None,
        false,
    );
    assert!(
        result.is_ok(),
        "should resolve order_id: {:?}",
        result.err()
    );
}

#[test]
fn test_schema_resolves_three_part_name() {
    // Test with fully-qualified 3-part table name as dbt generates
    let sql = r#"with
orders as (
    select * from "jaffle_shop"."main"."stg_orders"
)
select * from orders"#;
    let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();

    let mut schema = polyglot_sql::MappingSchema::new();
    let cols = vec![
        (
            "order_id".to_string(),
            polyglot_sql::expressions::DataType::Unknown,
        ),
        (
            "customer_id".to_string(),
            polyglot_sql::expressions::DataType::Unknown,
        ),
    ];
    // Register with 3-part name
    schema
        .add_table("jaffle_shop.main.stg_orders", &cols, None)
        .unwrap();

    let result = polyglot_sql::lineage::lineage_with_schema(
        "order_id",
        &expr,
        Some(&schema as &dyn polyglot_sql::Schema),
        None,
        false,
    );
    assert!(
        result.is_ok(),
        "should resolve order_id via 3-part name: {:?}",
        result.err()
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

/// Build a manifest with 3 levels: customers → orders → stg_orders → raw.orders
#[test]
fn test_cross_model_single_hop() {
    // orders.order_id → stg_orders.order_id → raw.orders.id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DialectType::Generic,
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
        DialectType::Generic,
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
        DialectType::Generic,
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
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let cross = compute_cross_model_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
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
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_normalize_table_name() {
    assert_eq!(normalize_table_name("stg_orders"), "stg_orders");
    assert_eq!(
        normalize_table_name("\"jaffle_shop\".\"main\".\"stg_orders\""),
        "stg_orders"
    );
    assert_eq!(normalize_table_name("`raw`.`orders`"), "orders");
    assert_eq!(normalize_table_name("schema.table"), "table");
}

#[test]
fn test_format_lineage_error_strips_position() {
    let err = polyglot_sql::Error::parse("Cannot find column 'x' in query", 0, 0, 0, 0);
    let formatted = format_lineage_error(&err);
    assert_eq!(formatted, "lineage failed: Cannot find column 'x' in query");
    assert!(
        !formatted.contains("line 0"),
        "should strip meaningless position info"
    );
}

#[test]
fn test_format_lineage_error_preserves_real_position() {
    let err = polyglot_sql::Error::parse("unexpected token", 5, 10, 0, 0);
    let formatted = format_lineage_error(&err);
    assert!(
        formatted.contains("line 5"),
        "should preserve real position info: {}",
        formatted
    );
}

#[test]
fn test_format_lineage_error_internal() {
    let err = polyglot_sql::Error::internal("lineage recursion depth exceeded");
    let formatted = format_lineage_error(&err);
    assert_eq!(
        formatted,
        "lineage failed: lineage recursion depth exceeded"
    );
}

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
fn test_column_not_found_hint_when_select_star_unresolved() {
    // When SELECT * cannot be expanded (external table not in manifest),
    // ColumnNotFound errors for YAML-defined columns should include the SELECT * hint.
    let mut manifest = make_test_manifest();
    // Replace stg_orders SQL with SELECT * from an external table (no schema info)
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("SELECT * FROM some_external_table".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // All columns should fail because SELECT * can't be expanded
    assert!(
        result
            .errors
            .iter()
            .any(|e| { e.hint.as_deref().unwrap_or("").contains("SELECT *") }),
        "ColumnNotFound errors should include SELECT * hint when stars remain unresolved; got: {:?}",
        result.errors
    );
}

#[test]
fn test_column_not_found_hint_when_cte_select_star_unresolved() {
    // When a CTE body has SELECT * from an external table, the hint should still
    // fire for the outer query's ColumnNotFound errors even though the outermost
    // SELECT list has no star.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code =
        Some("WITH src AS (SELECT * FROM some_external_table) SELECT id FROM src".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(
        result
            .errors
            .iter()
            .any(|e| { e.hint.as_deref().unwrap_or("").contains("SELECT *") }),
        "ColumnNotFound errors for CTE-nested stars should include SELECT * hint; got: {:?}",
        result.errors
    );
}

#[test]
fn test_column_not_found_hint_when_derived_table_select_star_unresolved() {
    // Derived-table pattern: SELECT id FROM (SELECT * FROM ext) src
    // The outermost SELECT has no star; the star is inside a FROM subquery.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("SELECT id FROM (SELECT * FROM some_external_table) src".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(
        result
            .errors
            .iter()
            .any(|e| { e.hint.as_deref().unwrap_or("").contains("SELECT *") }),
        "ColumnNotFound errors for derived-table stars should include SELECT * hint; got: {:?}",
        result.errors
    );
}

#[test]
fn test_column_not_found_hint_when_join_select_star_unresolved() {
    // JOIN-derived-table pattern: SELECT id FROM base JOIN (SELECT * FROM ext) src ON true
    // The star lives inside a JOIN subquery, not the outermost select list or FROM clause.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
        "SELECT id FROM some_table JOIN (SELECT * FROM some_external_table) src ON 1=1".to_string(),
    );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(
        result
            .errors
            .iter()
            .any(|e| { e.hint.as_deref().unwrap_or("").contains("SELECT *") }),
        "ColumnNotFound errors for JOIN-derived-table stars should include SELECT * hint; got: {:?}",
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

#[test]
fn test_cte_alias_resolution() {
    // Issue mml.6: FROM cte_name AS alias causes lineage to stop at alias
    // Pattern: WITH import_model AS (...) SELECT base.col FROM import_model AS base
    let mut nodes = HashMap::new();
    let mut sources = HashMap::new();

    // Source table
    let mut src_cols = HashMap::new();
    for name in ["id", "name", "status"] {
        src_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.items".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.items".to_string(),
            name: "items".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: src_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // stg_items: simple staging model
    let mut stg_cols = HashMap::new();
    for name in ["item_id", "name", "status"] {
        stg_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_items".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_items".to_string(),
            name: "stg_items".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.items".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_cols,
            compiled_code: Some("select id as item_id, name, status from items".to_string()),
            database: None,
            schema: None,
        },
    );

    // mart_items: uses FROM cte AS alias pattern
    let mut mart_cols = HashMap::new();
    for name in ["item_id", "status"] {
        mart_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.mart_items".to_string(),
        ManifestNode {
            unique_id: "model.proj.mart_items".to_string(),
            name: "mart_items".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.stg_items".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: mart_cols,
            compiled_code: Some(
                concat!(
                    "with import_stg_items as (\n",
                    "    select * from stg_items\n",
                    ")\n",
                    "select base.item_id, base.status\n",
                    "from import_stg_items as base"
                )
                .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    let manifest = Manifest {
        nodes,
        sources,
        exposures: HashMap::new(),
        ..Default::default()
    };
    let result = compute_cross_model_column_lineage(
        &manifest,
        "mart_items",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 2);

    // item_id should trace through stg_items to raw items.id
    // NOT stop at alias "base"
    let item_id = result
        .columns
        .iter()
        .find(|c| c.column == "item_id")
        .unwrap();
    assert!(
        item_id.sources.iter().all(|s| s.table != "base"),
        "item_id should not reference alias 'base', got: {:?}",
        item_id.sources
    );
    assert!(
        item_id.sources.iter().any(|s| s.column == "id"),
        "item_id should trace to raw items.id, got: {:?}",
        item_id.sources
    );
}

#[test]
fn test_select_star_chain_with_join() {
    // Issue mml.7: SELECT * chain + JOIN causes "Cannot find column" errors
    let mut nodes = HashMap::new();
    let mut sources = HashMap::new();

    // Source: raw.users
    let mut user_cols = HashMap::new();
    for name in ["id", "name", "area"] {
        user_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.users".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.users".to_string(),
            name: "users".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: user_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // Source: raw.regions
    let mut region_cols = HashMap::new();
    for name in ["id", "region_name"] {
        region_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.regions".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.regions".to_string(),
            name: "regions".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: region_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // stg_users: SELECT * from raw
    let mut stg_user_cols = HashMap::new();
    for name in ["id", "name", "area"] {
        stg_user_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_users".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_users".to_string(),
            name: "stg_users".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.users".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_user_cols,
            compiled_code: Some("select id, name, area from users".to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        },
    );

    // stg_regions
    let mut stg_region_cols = HashMap::new();
    for name in ["id", "region_name"] {
        stg_region_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_regions".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_regions".to_string(),
            name: "stg_regions".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.regions".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_region_cols,
            compiled_code: Some("select id, region_name from regions".to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        },
    );

    // mart_users: multi-level SELECT * chain + JOIN
    // Uses backtick-quoted 3-part names like real dbt BigQuery compiled SQL
    let mut mart_cols = HashMap::new();
    for name in ["id", "name", "area", "region_name"] {
        mart_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.mart_users".to_string(),
        ManifestNode {
            unique_id: "model.proj.mart_users".to_string(),
            name: "mart_users".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec![
                    "model.proj.stg_users".to_string(),
                    "model.proj.stg_regions".to_string(),
                ],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: mart_cols,
            compiled_code: Some(
                concat!(
                    "with\n",
                    "import_users as (\n",
                    "    select * from `mydb`.`myschema`.`stg_users`\n",
                    "),\n",
                    "base as (\n",
                    "    select * from import_users\n",
                    "),\n",
                    "import_regions as (\n",
                    "    select * from `mydb`.`myschema`.`stg_regions`\n",
                    ")\n",
                    "select base.*, import_regions.region_name\n",
                    "from base\n",
                    "left join import_regions on base.area = import_regions.id"
                )
                .to_string(),
            ),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        },
    );

    let manifest = Manifest {
        nodes,
        sources,
        exposures: HashMap::new(),
        ..Default::default()
    };
    let result = compute_cross_model_column_lineage(
        &manifest,
        "mart_users",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // All 4 columns should resolve without errors
    assert!(
        result.errors.is_empty(),
        "should resolve all columns without errors, got: {:?}",
        result.errors
    );
    assert_eq!(
        result.columns.len(),
        4,
        "should have 4 columns, got: {:?}",
        result.columns.iter().map(|c| &c.column).collect::<Vec<_>>()
    );

    // area should trace through to raw users source
    let area = result.columns.iter().find(|c| c.column == "area").unwrap();
    assert!(
        area.sources
            .iter()
            .any(|s| s.column == "area" && s.table.contains("users")),
        "area should trace to raw users.area, got: {:?}",
        area.sources
    );

    // region_name should trace through to raw regions source
    let region = result
        .columns
        .iter()
        .find(|c| c.column == "region_name")
        .unwrap();
    assert!(
        region
            .sources
            .iter()
            .any(|s| s.column == "region_name" && s.table.contains("regions")),
        "region_name should trace to raw regions.region_name, got: {:?}",
        region.sources
    );
}

#[test]
fn test_select_star_chain_with_cte_alias_and_join() {
    // Combination of mml.6 + mml.7: SELECT * chain + CTE alias + JOIN
    // This is the most common dbt pattern in mart/warehouse layers
    let mut nodes = HashMap::new();
    let mut sources = HashMap::new();

    // Source: raw.users
    let mut user_cols = HashMap::new();
    for name in ["id", "name", "area"] {
        user_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.users".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.users".to_string(),
            name: "users".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: user_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // Source: raw.regions
    let mut region_cols = HashMap::new();
    for name in ["id", "region_name"] {
        region_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.regions".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.regions".to_string(),
            name: "regions".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: region_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // stg_users
    let mut stg_user_cols = HashMap::new();
    for name in ["id", "name", "area"] {
        stg_user_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_users".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_users".to_string(),
            name: "stg_users".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.users".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_user_cols,
            compiled_code: Some("select id, name, area from users".to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        },
    );

    // stg_regions
    let mut stg_region_cols = HashMap::new();
    for name in ["id", "region_name"] {
        stg_region_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_regions".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_regions".to_string(),
            name: "stg_regions".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.regions".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_region_cols,
            compiled_code: Some("select id, region_name from regions".to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        },
    );

    // mart_users: SELECT * chain + CTE alias + JOIN
    // Pattern from mml.7 description but with CTE aliases (mml.6)
    let mut mart_cols = HashMap::new();
    for name in ["id", "name", "area", "region_name"] {
        mart_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.mart_users".to_string(),
        ManifestNode {
            unique_id: "model.proj.mart_users".to_string(),
            name: "mart_users".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec![
                    "model.proj.stg_users".to_string(),
                    "model.proj.stg_regions".to_string(),
                ],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: mart_cols,
            compiled_code: Some(
                concat!(
                    "with\n",
                    "import_users as (\n",
                    "    select * from `mydb`.`myschema`.`stg_users`\n",
                    "),\n",
                    "import_regions as (\n",
                    "    select * from `mydb`.`myschema`.`stg_regions`\n",
                    ")\n",
                    "select u.*, import_regions.region_name\n",
                    "from import_users as u\n",
                    "left join import_regions on u.area = import_regions.id"
                )
                .to_string(),
            ),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        },
    );

    let manifest = Manifest {
        nodes,
        sources,
        exposures: HashMap::new(),
        ..Default::default()
    };
    let result = compute_cross_model_column_lineage(
        &manifest,
        "mart_users",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // All 4 columns should resolve without errors
    assert!(
        result.errors.is_empty(),
        "should resolve all columns without errors, got: {:?}",
        result.errors
    );
    assert_eq!(
        result.columns.len(),
        4,
        "should have 4 columns, got: {:?}",
        result.columns.iter().map(|c| &c.column).collect::<Vec<_>>()
    );

    // area should trace through CTE alias "u" → import_users → stg_users → raw users
    let area = result.columns.iter().find(|c| c.column == "area").unwrap();
    assert!(
        area.sources
            .iter()
            .any(|s| s.column == "area" && s.table.contains("users")),
        "area should trace to raw users.area, got: {:?}",
        area.sources
    );

    // region_name should trace through import_regions → stg_regions → raw regions
    let region = result
        .columns
        .iter()
        .find(|c| c.column == "region_name")
        .unwrap();
    assert!(
        region
            .sources
            .iter()
            .any(|s| s.column == "region_name" && s.table.contains("regions")),
        "region_name should trace to raw regions.region_name, got: {:?}",
        region.sources
    );
}

// --- Column impact tests ---

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
        DialectType::Generic,
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
        DialectType::Generic,
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
        DialectType::Generic,
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
        DialectType::Generic,
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

#[test]
fn test_bigquery_unnest_virtual_source_excluded() {
    // BigQuery UNNEST produces a Virtual source node. Before the fix, collect_leaves
    // would include it as a leaf with an empty/synthetic table name. After the fix,
    // Virtual leaf nodes are skipped so only real table sources survive.
    let mut nodes = HashMap::new();
    let mut columns = HashMap::new();
    for name in ["week_start"] {
        columns.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.unnest_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.unnest_model".to_string(),
            name: "unnest_model".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns,
            compiled_code: Some(
                "SELECT date_val AS week_start FROM UNNEST(GENERATE_DATE_ARRAY('2024-01-01', '2024-12-31', INTERVAL 1 WEEK)) AS date_val".to_string(),
            ),
            database: None,
            schema: None,
        },
    );
    let manifest = Manifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        ..Default::default()
    };
    let result = compute_column_lineage(
        &manifest,
        "unnest_model",
        DialectType::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    // week_start derives from UNNEST — a Virtual source with no physical table.
    // collect_leaves must skip Virtual nodes, so sources should be empty.
    let week_start = result.columns.iter().find(|c| c.column == "week_start");
    if let Some(entry) = week_start {
        for src in &entry.sources {
            assert!(
                !src.table.is_empty(),
                "Virtual UNNEST source should not appear as leaf: got table='{}', column='{}'",
                src.table,
                src.column
            );
        }
    }
}
