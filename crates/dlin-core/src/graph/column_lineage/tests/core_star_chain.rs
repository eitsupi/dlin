use super::*;
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
            alias: None,
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
            alias: None,
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
        DlinDialect::Generic,
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
            alias: None,
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
            alias: None,
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
            alias: None,
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
        DlinDialect::Generic,
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
            alias: None,
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
            alias: None,
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
            alias: None,
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
        DlinDialect::Generic,
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
