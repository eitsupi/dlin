use super::*;

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
fn test_schema_resolves_cte_star_from_unknown_source() {
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
fn test_column_not_found_hint_when_select_star_unresolved() {
    // When SELECT * cannot be expanded (external table not in manifest),
    // ColumnNotFound errors for YAML-defined columns should include the SELECT * hint.
    let mut manifest = make_test_manifest();
    // Replace stg_orders SQL with SELECT * from an external table (no schema info)
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("SELECT * FROM some_unknown_source".to_string());

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
fn test_cte_select_star_passthrough_is_traced() {
    // When a CTE body has SELECT * from an external table, the hint should still
    // fire for the outer query's ColumnNotFound errors even though the outermost
    // SELECT list has no star.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code =
        Some("WITH src AS (SELECT * FROM some_unknown_source) SELECT id FROM src".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(
        &result,
        &["id"],
        &["customer_id", "order_date", "order_id", "status"],
    );
    assert_select_star_hint(&result);
}

#[test]
fn test_derived_table_select_star_passthrough_is_traced() {
    // Derived-table pattern: SELECT id FROM (SELECT * FROM ext) src
    // The outermost SELECT has no star; the star is inside a FROM subquery.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("SELECT id FROM (SELECT * FROM some_unknown_source) src".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(
        &result,
        &["id"],
        &["customer_id", "order_date", "order_id", "status"],
    );
    assert_select_star_hint(&result);
}

#[test]
fn test_join_select_star_passthrough_is_traced() {
    // JOIN-derived-table pattern: SELECT id FROM base JOIN (SELECT * FROM ext) src ON true
    // The star lives inside a JOIN subquery, not the outermost select list or FROM clause.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
        "SELECT id FROM some_table JOIN (SELECT * FROM some_unknown_source) src ON 1=1".to_string(),
    );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(
        &result,
        &["id"],
        &["customer_id", "order_date", "order_id", "status"],
    );
    assert_select_star_hint(&result);
}

#[test]
fn test_star_passthrough_query_shapes_are_traceable() {
    let cases = [
        "WITH src AS (SELECT * FROM some_unknown_source) SELECT id FROM src",
        "SELECT id FROM raw.orders UNION ALL SELECT id FROM raw.orders",
        "WITH a AS (SELECT * FROM some_unknown_source), b AS (SELECT * FROM some_unknown_source) SELECT COALESCE(a.id, b.id) AS id FROM a JOIN b ON true",
        "WITH a AS (SELECT * FROM some_unknown_source), b AS (SELECT * FROM some_unknown_source) SELECT CASE WHEN a.id IS NULL THEN b.id ELSE a.id END AS id FROM a JOIN b ON true",
        "WITH left_side AS (SELECT id FROM raw.orders), right_side AS (SELECT * FROM some_unknown_source) SELECT id FROM left_side UNION ALL SELECT id FROM right_side",
        "(SELECT id FROM raw.orders UNION ALL SELECT id FROM raw.orders) INTERSECT SELECT id FROM raw.orders",
        "SELECT id FROM raw.orders EXCEPT SELECT id FROM raw.orders",
    ];

    for sql in cases {
        let mut manifest = make_test_manifest();
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .columns = [(
            "id".to_string(),
            ManifestColumn {
                name: "id".to_string(),
            },
        )]
        .into_iter()
        .collect();
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
        assert!(
            result.errors.is_empty(),
            "SQL: {sql}\nerrors: {:?}",
            result.errors
        );
        assert_eq!(result.traced_columns, 1, "SQL: {sql}");
    }
}

#[test]
fn test_set_operation_oracle_shapes_remain_traced_when_explicit_anywhere() {
    let cases = [
        (
            "col_a",
            "WITH u AS (SELECT * FROM ext_a UNION ALL SELECT 2 AS col_a) SELECT col_a FROM u",
        ),
        (
            "col_a",
            "WITH lit AS (SELECT 1 AS col_a), u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM ext_a) SELECT col_a FROM u",
        ),
        (
            "col_a",
            "WITH lit AS (SELECT 1 AS col_a), lit2 AS (SELECT 2 AS col_a), u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM lit2) SELECT col_a FROM u",
        ),
        (
            "c1",
            "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c1, 2 AS c2 UNION ALL SELECT a.col_x, a.col_y FROM a) SELECT c1 FROM u",
        ),
        (
            "c9",
            "WITH u AS (SELECT 1 AS c1 UNION ALL SELECT 2 AS c9) SELECT c9 FROM u",
        ),
        (
            "a",
            "WITH u AS (SELECT 1 AS a, 2 AS a UNION ALL SELECT 3, 4) SELECT a FROM u",
        ),
        (
            "c",
            "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c UNION ALL SELECT a.col_a FROM a UNION ALL SELECT 3 AS c3) SELECT c FROM u",
        ),
    ];

    for (column, sql) in cases {
        let mut manifest = make_test_manifest();
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .columns = [(
            column.to_string(),
            ManifestColumn {
                name: column.to_string(),
            },
        )]
        .into_iter()
        .collect();
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
        assert!(
            result.errors.is_empty(),
            "SQL: {sql}\nerrors: {:?}",
            result.errors
        );
        assert_eq!(result.traced_columns, 1, "SQL: {sql}");
    }
}

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
fn test_bigquery_unnest_virtual_source_excluded() {
    // BigQuery UNNEST produces a Virtual source node. Before the fix, collect_leaves
    // would include it as a leaf with an empty/synthetic table name. After the fix,
    // Virtual leaf nodes are skipped so only real table sources survive.
    let mut nodes = HashMap::new();
    let mut columns = HashMap::new();
    let name = "week_start";
    columns.insert(
        name.to_string(),
        ManifestColumn {
            name: name.to_string(),
        },
    );
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

fn assert_exact_column_outcomes(
    result: &ModelColumnLineage,
    expected_columns: &[&str],
    expected_errors: &[&str],
) {
    let mut actual_columns: Vec<_> = result
        .columns
        .iter()
        .map(|column| column.column.as_str())
        .collect();
    actual_columns.sort_unstable();
    let mut expected_columns = expected_columns.to_vec();
    expected_columns.sort_unstable();
    assert_eq!(actual_columns, expected_columns);

    let mut actual_errors: Vec<_> = result
        .errors
        .iter()
        .map(|error| {
            assert_eq!(error.kind, ColumnLineageErrorKind::ColumnNotFound);
            error
                .what
                .strip_prefix("column '")
                .and_then(|rest| rest.split_once("':"))
                .map(|(name, _)| name)
                .expect("column errors should identify their column")
        })
        .collect();
    actual_errors.sort_unstable();
    let mut expected_errors = expected_errors.to_vec();
    expected_errors.sort_unstable();
    assert_eq!(actual_errors, expected_errors);
}

fn assert_select_star_hint(result: &ModelColumnLineage) {
    assert!(
        result
            .errors
            .iter()
            .any(|error| error.hint.as_deref().unwrap_or("").contains("SELECT *")),
        "expected SELECT * hint, got errors: {:?}",
        result.errors
    );
}

#[test]
fn test_column_resolution_reasons_map_to_dlin_outcomes() {
    let not_found = compute_star_shape("SELECT id FROM raw.orders", &["missing"]);
    assert!(
        not_found
            .errors
            .iter()
            .any(|error| error.what.starts_with("column 'missing':"))
    );
    assert!(not_found.errors.iter().all(|error| error.hint.is_none()));

    let indeterminate = compute_star_shape("SELECT * FROM unknown_source", &["missing"]);
    assert_exact_column_outcomes(&indeterminate, &[], &["missing"]);
    assert_select_star_hint(&indeterminate);

    // Duplicate output names are ambiguous. They must remain an error rather
    // than being guessed by the legacy set-operation fallback.
    let ambiguous = compute_star_shape(
        "SELECT a.id, b.id FROM raw.orders a JOIN raw.orders b ON a.id = b.id",
        &["id"],
    );
    assert_exact_column_outcomes(&ambiguous, &[], &["id"]);
    assert!(ambiguous.errors.iter().all(|error| error.hint.is_none()));
}

#[test]
fn test_unresolved_star_does_not_reject_unrelated_explicit_column() {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("SELECT order_id, * FROM some_unknown_source".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(
        &result,
        &["order_id"],
        &["customer_id", "order_date", "status"],
    );
}

#[test]
fn test_annotated_star_and_explicit_projection_are_classified_independently() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.depends_on.nodes.clear();
    node.compiled_code = Some(
            "SELECT\n  -- unresolved passthrough\n  some_unknown_source.*,\n  -- explicit output\n  order_id\nFROM some_unknown_source"
                .to_string(),
        );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(
        &result,
        &["order_id"],
        &["customer_id", "order_date", "status"],
    );
}

#[test]
fn test_known_manifest_source_succeeds_alongside_external_join_star() {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
            "SELECT o.id AS order_id, e.*\nFROM raw.orders o\nJOIN some_unknown_source e ON o.id = e.id"
                .to_string(),
        );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(
        &result,
        &["order_id"],
        &["customer_id", "order_date", "status"],
    );
}

#[test]
fn test_set_operations_guard_unresolved_star_branches() {
    for operator in ["UNION", "INTERSECT", "EXCEPT"] {
        let mut manifest = make_test_manifest();
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .columns = ["id", "explicit_col"]
            .into_iter()
            .map(|name| {
                (
                    name.to_string(),
                    ManifestColumn {
                        name: name.to_string(),
                    },
                )
            })
            .collect();

        let set_operation = format!(
            "SELECT id, 1 AS explicit_col FROM raw.orders {operator} SELECT id, * FROM some_unknown_source"
        );
        manifest
            .nodes
            .get_mut("model.proj.stg_orders")
            .unwrap()
            .compiled_code = Some(set_operation.clone());

        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DialectType::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert_exact_column_outcomes(&result, &["id", "explicit_col"], &[]);

        for wrapper in [
            format!("WITH combined AS ({set_operation}) SELECT id, explicit_col FROM combined"),
            format!("SELECT id, explicit_col FROM ({set_operation}) combined"),
        ] {
            manifest
                .nodes
                .get_mut("model.proj.stg_orders")
                .unwrap()
                .compiled_code = Some(wrapper);

            let result = compute_column_lineage(
                &manifest,
                "stg_orders",
                DialectType::Generic,
                &mut ColumnLineageCache::disabled(),
            );

            assert_exact_column_outcomes(&result, &["id", "explicit_col"], &[]);
        }
    }
}

#[test]
fn test_set_operations_match_unresolved_stars_by_ordinal() {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .columns = ["a", "b", "c"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            )
        })
        .collect();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
        "SELECT id AS a, user_id AS b, order_date AS c FROM raw.orders \
             UNION SELECT 3, 4, * FROM some_unknown_source"
            .to_string(),
    );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(&result, &["a", "b", "c"], &[]);
}

#[test]
fn test_set_operation_star_only_branch_keeps_explicit_left_names() {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .columns = ["a", "b"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            )
        })
        .collect();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
        "SELECT id AS a, user_id AS b FROM raw.orders \
             UNION SELECT * FROM some_unknown_source"
            .to_string(),
    );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(&result, &["a", "b"], &[]);
}

#[test]
fn test_set_operation_explicit_projection_before_unresolved_star_is_traced() {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .columns = ["a", "b", "c"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            )
        })
        .collect();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
        "SELECT id AS a, user_id AS b, order_date AS c FROM raw.orders \
             UNION SELECT 3, *, 4 AS extra_col FROM some_unknown_source"
            .to_string(),
    );

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_exact_column_outcomes(&result, &["a", "b", "c"], &[]);
}

#[test]
fn test_parenthesized_unresolved_star_is_detected() {
    // In polyglot-sql 0.6.2, a nested parenthesized query in a FROM clause
    // preserves a Paren node around the inner query.
    let expr = polyglot_sql::parse_one(
        "SELECT id FROM ((SELECT * FROM some_unknown_source))",
        DialectType::Generic,
    )
    .unwrap();

    assert!(format!("{expr:?}").contains("Paren"));
    assert!(has_unresolved_stars(&expr), "expr: {expr:?}");
}

#[test]
fn test_nested_set_operations_guard_unresolved_star_branch() {
    // The 0.6.2 parser represents an unparenthesized UNION chain as a
    // left-nested Union(Union(...), ...).
    let sql = "SELECT id, 1 AS explicit_col FROM raw.orders UNION SELECT id, 2 AS explicit_col FROM raw.orders UNION SELECT id, * FROM some_unknown_source";
    let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();
    assert!(matches!(
        &expr,
        polyglot_sql::Expression::Union(union)
            if matches!(union.left, polyglot_sql::Expression::Union(_))
    ));

    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .columns = ["id", "explicit_col"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            )
        })
        .collect();
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

    assert_exact_column_outcomes(&result, &["id", "explicit_col"], &[]);
}

#[test]
fn test_explicit_output_case_normalization_with_unresolved_star() {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("SELECT ORDER_ID, * FROM some_unknown_source".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Snowflake,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(
        result
            .columns
            .iter()
            .any(|column| column.column == "order_id"),
        "case-folded explicit output should resolve order_id: {:?}",
        result.errors
    );
    assert!(
        result
            .errors
            .iter()
            .all(|error| !error.what.starts_with("column 'order_id':")),
        "order_id should not be rejected because of Snowflake case folding: {:?}",
        result.errors
    );
}

fn compute_star_shape(sql: &str, columns: &[&str]) -> ModelColumnLineage {
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .columns = columns
        .iter()
        .map(|name| {
            (
                (*name).to_string(),
                ManifestColumn {
                    name: (*name).to_string(),
                },
            )
        })
        .collect();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(sql.to_string());
    compute_column_lineage(
        &manifest,
        "stg_orders",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    )
}

fn assert_sources_for(result: &ModelColumnLineage, column: &str, expected: &[(&str, &str)]) {
    let entry = result
        .columns
        .iter()
        .find(|entry| entry.column == column)
        .unwrap_or_else(|| panic!("missing traced column {column}: {:?}", result.errors));
    let mut actual: Vec<_> = entry
        .sources
        .iter()
        .map(|source| (source.table.as_str(), source.column.as_str()))
        .collect();
    actual.sort_unstable();
    let mut expected = expected.to_vec();
    expected.sort_unstable();
    assert_eq!(actual, expected);
}

#[test]
fn test_set_star_with_unknown_source_does_not_fabricate_lineage() {
    let result = compute_star_shape(
        "SELECT * FROM unknown_source UNION ALL SELECT id, amt AS total FROM known_table",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &["total"], &[]);
    assert_sources_for(&result, "total", &[("known_table", "amt")]);
    assert!(result.columns.iter().all(|entry| {
        entry
            .sources
            .iter()
            .all(|source| source.table != "orders" && source.table != "unknown_source")
    }));
}

#[test]
fn test_nested_set_star_does_not_fabricate_lineage() {
    let result = compute_star_shape(
        "SELECT * FROM (SELECT * FROM real_x) sub UNION ALL SELECT id, amt AS total FROM known_table",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &["total"], &[]);
    assert_sources_for(&result, "total", &[("known_table", "amt")]);
    assert!(result.columns.iter().all(|entry| {
        entry.sources.iter().all(|source| {
            source.table != "real_x"
                && source.table != "star_source"
                && source.table != "synthetic_source"
        })
    }));
}

#[test]
fn test_every_explicit_set_operand_contributes_sources() {
    // Each operand that declares the name is traced on its own and the results
    // are merged, so a name present in several operands keeps all of them.
    let result = compute_star_shape(
        "SELECT * FROM unknown_source \
         UNION ALL SELECT id, amt AS total FROM known_table \
         UNION ALL SELECT id, fee AS total FROM third_table",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &["total"], &[]);
    assert_sources_for(
        &result,
        "total",
        &[("known_table", "amt"), ("third_table", "fee")],
    );
}

#[test]
fn test_set_operands_match_explicit_projections_by_ordinal() {
    let result = compute_star_shape(
        "SELECT * FROM unknown_source \
         UNION ALL SELECT id, amt AS total FROM known_table \
         UNION ALL SELECT id, fee FROM third_table",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &["total"], &[]);
    assert_sources_for(
        &result,
        "total",
        &[("known_table", "amt"), ("third_table", "fee")],
    );
}

#[test]
fn test_set_operands_do_not_match_explicit_projections_by_name_at_other_ordinal() {
    let result = compute_star_shape(
        "SELECT * FROM unknown_source \
         UNION ALL SELECT id, amt AS total FROM known_table \
         UNION ALL SELECT fee AS total, id FROM third_table",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &[], &["total"]);
    assert!(result.columns.is_empty());
}

#[test]
fn test_set_with_no_explicit_operand_stays_unresolved() {
    // No operand declares the name, so there is nothing to trace and the
    // column is reported as not found rather than guessed from a star.
    let result = compute_star_shape(
        "SELECT * FROM unknown_a UNION ALL SELECT * FROM unknown_b",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &[], &["total"]);
}

#[test]
fn test_set_star_with_derived_source_and_no_explicit_name_stays_unresolved() {
    let result = compute_star_shape(
        "SELECT * FROM (SELECT * FROM real_x) sub UNION ALL SELECT id, amt FROM known_table",
        &["total"],
    );
    assert_exact_column_outcomes(&result, &[], &["total"]);
    assert!(result.columns.iter().all(|entry| {
        entry.sources.iter().all(|source| {
            source.table != "real_x"
                && source.table != "known_table"
                && source.table != "star_source"
                && source.table != "synthetic_source"
        })
    }));
}

#[test]
fn test_star_replace_introduced_name_is_explicit() {
    let result = compute_star_shape(
        "SELECT * REPLACE (id AS wanted) FROM raw.orders",
        &["wanted"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 1);
    assert_sources_for(&result, "wanted", &[("raw.orders", "id")]);
}

#[test]
fn test_star_rename_introduced_name_traces_original_column() {
    let result = compute_star_shape(
        "SELECT * RENAME (id AS wanted) FROM raw.orders",
        &["wanted"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 1);
    assert_sources_for(&result, "wanted", &[("raw.orders", "id")]);
}

#[test]
fn test_qualified_external_star_is_not_expanded_from_joined_cte() {
    let result = compute_star_shape(
        "WITH c AS (SELECT 1 AS x) SELECT e.* FROM c JOIN external e ON true",
        &["x"],
    );
    assert_exact_column_outcomes(&result, &[], &["x"]);
}

#[test]
fn test_cte_scope_propagates_to_all_set_operation_operands() {
    // The parser attaches a top-level WITH clause to the UNION/INTERSECT/EXCEPT
    // node itself (its own `with` field), not to either operand's SELECT, but the
    // CTE it defines is visible to every operand.
    let result = compute_star_shape(
        "WITH c AS (SELECT id AS x FROM raw.orders) SELECT x FROM c UNION ALL SELECT * FROM c",
        &["x"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 1);
    assert_sources_for(&result, "x", &[("raw.orders", "id")]);
}

#[test]
fn test_star_rename_in_join_keeps_source_table_qualifier() {
    // The RENAME source must keep the star's own qualifier so it resolves against
    // the correct joined table rather than an unqualified (and ambiguous) name.
    let result = compute_star_shape(
        "SELECT b.* RENAME (id AS wanted) FROM raw.orders a JOIN raw.customers b ON a.customer_id = b.id",
        &["wanted"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 1);
    assert_sources_for(&result, "wanted", &[("raw.customers", "id")]);
}

#[test]
fn test_set_operation_nested_in_from_subquery_uses_explicit_branch() {
    let result = compute_star_shape(
        "SELECT col_a FROM (SELECT * FROM ext_a UNION ALL SELECT 2 AS col_a) u",
        &["col_a"],
    );
    assert_exact_column_outcomes(&result, &["col_a"], &[]);
}

#[test]
fn test_nested_cte_name_does_not_shadow_outer_sibling_scope() {
    let result = compute_star_shape(
        "WITH c AS (SELECT id AS outer_id FROM raw.orders) \
         SELECT c.* FROM c \
         JOIN (WITH c AS (SELECT 2 AS inner_id) SELECT * FROM c) nested ON true",
        &["outer_id"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_sources_for(&result, "outer_id", &[("raw.orders", "id")]);
}

#[test]
fn test_star_except_removed_name_remains_unresolved() {
    let result = compute_star_shape(
        "SELECT * EXCEPT (wanted) FROM some_unknown_source",
        &["wanted"],
    );
    assert_eq!(result.traced_columns, 0);
    assert_eq!(result.errors.len(), 1);
    assert!(
        result.errors[0]
            .hint
            .as_deref()
            .unwrap_or("")
            .contains("SELECT *")
    );
}

#[test]
fn test_real_underscore_one_column_is_not_synthetic_ordinal() {
    let result = compute_star_shape("SELECT id AS _1 FROM raw.orders", &["_1"]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 1);
    assert_sources_for(&result, "_1", &[("raw.orders", "id")]);
}

#[test]
fn test_cte_star_expansion_preserves_marker_and_sources() {
    let result = compute_star_shape(
        "WITH known AS (SELECT 1 AS a, 2 AS b) SELECT 9 AS marker, * FROM known",
        &["marker", "a", "b"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 3);
    assert!(result.columns.iter().any(|entry| entry.column == "marker"));
    assert!(result.columns.iter().any(|entry| entry.column == "a"));
    assert!(result.columns.iter().any(|entry| entry.column == "b"));
    assert_sources_for(&result, "a", &[("known", "a")]);
    assert_sources_for(&result, "b", &[("known", "b")]);
}

#[test]
fn test_duplicate_left_output_name_preserves_sources() {
    let result = compute_star_shape(
        "WITH dup AS (SELECT 1 AS a, 2 AS a) SELECT a FROM dup",
        &["a"],
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.traced_columns, 1);
    assert_sources_for(&result, "a", &[("dup", "a")]);
}
