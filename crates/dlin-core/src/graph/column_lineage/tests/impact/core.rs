use super::*;

#[test]
fn test_column_impact_excludes_unrelated_column_errors() {
    let manifest = duplicate_column_impact_manifest();
    let result = compute_column_impact(
        &manifest,
        "impact_source",
        "other_col",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let duplicate_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|error| error.what.starts_with("column 'dup_col':"))
        .collect();
    assert!(
        duplicate_errors.is_empty(),
        "diagnostics for an unrelated output column must not leak into impact: {:?}",
        result.errors
    );
}

#[test]
fn test_column_impact_direct_dependent() {
    // stg_orders.order_id is used by orders.order_id
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "order_id",
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_column_impact_distinguishes_same_relation_in_different_schemas() {
    let raw_id = "model.proj.raw_model";
    let staging_id = "model.proj.staging_model";
    let downstream_id = "model.proj.downstream";

    let node = |id: &str,
                name: &str,
                alias: Option<&str>,
                schema: Option<&str>,
                deps: Vec<&str>,
                columns: &[&str],
                sql: Option<&str>| ManifestNode {
        unique_id: id.to_string(),
        name: name.to_string(),
        alias: alias.map(str::to_string),
        resource_type: "model".to_string(),
        depends_on: DependsOn {
            nodes: deps.into_iter().map(str::to_string).collect(),
        },
        config: ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: columns
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect(),
        compiled_code: sql.map(str::to_string),
        database: Some("warehouse".to_string()),
        schema: schema.map(str::to_string),
    };

    let manifest = Manifest {
        nodes: HashMap::from([
            (
                raw_id.to_string(),
                node(
                    raw_id,
                    "raw_model",
                    Some("orders"),
                    Some("raw"),
                    vec![],
                    &["id"],
                    None,
                ),
            ),
            (
                staging_id.to_string(),
                node(
                    staging_id,
                    "staging_model",
                    Some("orders"),
                    Some("staging"),
                    vec![],
                    &["id"],
                    None,
                ),
            ),
            (
                downstream_id.to_string(),
                node(
                    downstream_id,
                    "downstream",
                    None,
                    None,
                    vec![raw_id, staging_id],
                    &["raw_id", "staging_id"],
                    Some(
                        "select raw.id as raw_id, staging.id as staging_id from warehouse.raw.orders raw join warehouse.staging.orders staging on raw.id = staging.id",
                    ),
                ),
            ),
        ]),
        ..Default::default()
    };

    let raw_impact = compute_column_impact(
        &manifest,
        "raw_model",
        "id",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(
        raw_impact.errors.is_empty(),
        "errors: {:?}",
        raw_impact.errors
    );
    assert!(
        raw_impact
            .impacted_columns
            .iter()
            .any(|column| column.model == "downstream" && column.column == "raw_id"),
        "raw impact: {:?}, errors: {:?}",
        raw_impact.impacted_columns,
        raw_impact.errors
    );
    assert!(
        !raw_impact
            .impacted_columns
            .iter()
            .any(|column| column.model == "downstream" && column.column == "staging_id")
    );

    let staging_impact = compute_column_impact(
        &manifest,
        "staging_model",
        "id",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(
        staging_impact.errors.is_empty(),
        "errors: {:?}",
        staging_impact.errors
    );
    assert!(
        staging_impact
            .impacted_columns
            .iter()
            .any(|column| column.model == "downstream" && column.column == "staging_id")
    );
    assert!(
        !staging_impact
            .impacted_columns
            .iter()
            .any(|column| column.model == "downstream" && column.column == "raw_id")
    );
}

#[test]
fn test_column_impact_qualified_source_matches_unqualified_model_relation() {
    // A manifest recording no database or schema gives nothing to compare a
    // qualified reference against. Refusing there would drop a real edge, so the
    // bare name decides when either side lacks qualification.
    let source_id = "model.proj.source_model";
    let downstream_id = "model.proj.downstream_model";
    let columns = || {
        HashMap::from([(
            "id".to_string(),
            ManifestColumn {
                name: "id".to_string(),
            },
        )])
    };
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                source_id.to_string(),
                ManifestNode {
                    unique_id: source_id.to_string(),
                    name: "source_model".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: columns(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
            (
                downstream_id.to_string(),
                ManifestNode {
                    unique_id: downstream_id.to_string(),
                    name: "downstream_model".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec![source_id.to_string()],
                    },
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: columns(),
                    compiled_code: Some(
                        "select id from \"warehouse\".\"raw\".\"source_model\"".to_string(),
                    ),
                    database: None,
                    schema: None,
                },
            ),
        ]),
        ..Default::default()
    };

    let report = compute_column_impact(
        &manifest,
        "source_model",
        "id",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert!(
        report
            .impacted_columns
            .iter()
            .any(|column| column.model == "downstream_model" && column.column == "id"),
        "qualified reference should still reach an unqualified model relation: {:?}",
        report.impacted_columns
    );
}

#[test]
fn test_column_impact_unqualified_source_matches_qualified_model_relation() {
    let source_id = "model.proj.source_model";
    let downstream_id = "model.proj.downstream_model";
    let columns = || {
        HashMap::from([(
            "id".to_string(),
            ManifestColumn {
                name: "id".to_string(),
            },
        )])
    };
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                source_id.to_string(),
                ManifestNode {
                    unique_id: source_id.to_string(),
                    name: "source_model".to_string(),
                    alias: Some("orders".to_string()),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: columns(),
                    compiled_code: None,
                    database: Some("warehouse".to_string()),
                    schema: Some("raw".to_string()),
                },
            ),
            (
                downstream_id.to_string(),
                ManifestNode {
                    unique_id: downstream_id.to_string(),
                    name: "downstream_model".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec![source_id.to_string()],
                    },
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: columns(),
                    compiled_code: Some("select orders.id as id".to_string()),
                    database: None,
                    schema: None,
                },
            ),
        ]),
        ..Default::default()
    };

    let report = compute_column_impact(
        &manifest,
        "source_model",
        "id",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert!(
        report
            .impacted_columns
            .iter()
            .any(|column| column.model == "downstream_model" && column.column == "id")
    );
}

#[test]
fn test_column_impact_does_not_attribute_ambiguous_bare_source() {
    let columns = || {
        HashMap::from([(
            "id".to_string(),
            ManifestColumn {
                name: "id".to_string(),
            },
        )])
    };
    let node = |unique_id: &str,
                name: &str,
                alias: Option<&str>,
                database: Option<&str>,
                schema: Option<&str>,
                depends_on: Vec<&str>,
                compiled_code: &str| ManifestNode {
        unique_id: unique_id.to_string(),
        name: name.to_string(),
        alias: alias.map(str::to_string),
        resource_type: "model".to_string(),
        depends_on: DependsOn {
            nodes: depends_on.into_iter().map(str::to_string).collect(),
        },
        config: ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: columns(),
        compiled_code: Some(compiled_code.to_string()),
        database: database.map(str::to_string),
        schema: schema.map(str::to_string),
    };

    let orders_a_id = "model.pkg.orders_a";
    let orders_b_id = "model.pkg.orders_b";
    let downstream_id = "model.pkg.downstream";
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                orders_a_id.to_string(),
                node(
                    orders_a_id,
                    "orders_a",
                    Some("orders"),
                    Some("db_a"),
                    Some("raw"),
                    vec![],
                    "select id",
                ),
            ),
            (
                orders_b_id.to_string(),
                node(
                    orders_b_id,
                    "orders_b",
                    Some("orders"),
                    Some("db_b"),
                    Some("raw"),
                    vec![],
                    "select id",
                ),
            ),
            (
                downstream_id.to_string(),
                node(
                    downstream_id,
                    "downstream",
                    None,
                    None,
                    None,
                    vec![orders_a_id, orders_b_id],
                    "select id from orders",
                ),
            ),
        ]),
        ..Default::default()
    };

    for upstream_id in [orders_a_id, orders_b_id] {
        let result = compute_column_impact(
            &manifest,
            upstream_id,
            "id",
            DlinDialect::Generic,
            &mut ColumnLineageCache::disabled(),
        );
        assert!(
            !result
                .impacted_columns
                .iter()
                .any(|column| column.model == "downstream"),
            "ambiguous bare source should not impact downstream from {upstream_id}: {:?}",
            result.impacted_columns
        );
    }
}

#[test]
fn test_column_impact_json_serialization() {
    let manifest = make_cross_model_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "order_id",
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        map.get("model.proj.stg_orders")
            .is_some_and(|deps| deps.contains(&"model.proj.orders".to_string())),
        "stg_orders should have orders as downstream, got: {:?}",
        map.get("model.proj.stg_orders")
    );
    // orders (by unique_id) is depended on by customers
    assert!(
        map.get("model.proj.orders")
            .is_some_and(|deps| deps.contains(&"model.proj.customers".to_string())),
        "orders should have customers as downstream, got: {:?}",
        map.get("model.proj.orders")
    );
    // customers has no downstream
    assert!(
        !map.contains_key("model.proj.customers"),
        "customers should have no downstream"
    );
}

/// Reconverging DAG regression: source.x → left.x/right.x → final.x → mart.x → dashboard.x
///
/// The path-local cycle guard allows each reconvergence point to appear once per upstream
/// path. Without it, a global visited set would prevent final/mart/dashboard from being
/// recorded via the second path, silently dropping half the impact.
#[test]
fn test_column_impact_reconverging_dag_multi_path() {
    let manifest = make_reconverging_manifest();
    let result = compute_column_impact(
        &manifest,
        "source_model",
        "x",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // final_model.x must appear at least twice — once via left_model, once via right_model
    let final_entries: Vec<_> = result
        .impacted_columns
        .iter()
        .filter(|ic| ic.model == "final_model" && ic.column == "x")
        .collect();
    assert!(
        final_entries.len() >= 2,
        "final_model.x should appear for each upstream path (left and right), \
         got {} entries. impacted: {:?}",
        final_entries.len(),
        result.impacted_columns
    );

    // mart_model.x must also appear at least twice
    let mart_entries: Vec<_> = result
        .impacted_columns
        .iter()
        .filter(|ic| ic.model == "mart_model" && ic.column == "x")
        .collect();
    assert!(
        mart_entries.len() >= 2,
        "mart_model.x should appear for each upstream path, got {} entries. impacted: {:?}",
        mart_entries.len(),
        result.impacted_columns
    );

    // The two mart_model.x entries must have distinct model_paths
    assert!(
        mart_entries[0].model_path != mart_entries[1].model_path,
        "mart_model.x entries should have distinct model_paths, both are: {:?}",
        mart_entries[0].model_path
    );

    // One path passes through left_model, the other through right_model
    let has_left = mart_entries
        .iter()
        .any(|ic| ic.model_path.iter().any(|(m, _, _)| m == "left_model"));
    let has_right = mart_entries
        .iter()
        .any(|ic| ic.model_path.iter().any(|(m, _, _)| m == "right_model"));
    assert!(
        has_left,
        "one mart_model.x path should pass through left_model, paths: {:?}",
        mart_entries
            .iter()
            .map(|ic| &ic.model_path)
            .collect::<Vec<_>>()
    );
    assert!(
        has_right,
        "one mart_model.x path should pass through right_model, paths: {:?}",
        mart_entries
            .iter()
            .map(|ic| &ic.model_path)
            .collect::<Vec<_>>()
    );

    // dashboard_model.x must also appear at least twice, preserving both upstream paths
    let dashboard_entries: Vec<_> = result
        .impacted_columns
        .iter()
        .filter(|ic| ic.model == "dashboard_model" && ic.column == "x")
        .collect();
    assert!(
        dashboard_entries.len() >= 2,
        "dashboard_model.x should appear for each upstream path, got {} entries. impacted: {:?}",
        dashboard_entries.len(),
        result.impacted_columns
    );

    // The two dashboard_model.x entries must have distinct model_paths
    assert!(
        dashboard_entries[0].model_path != dashboard_entries[1].model_path,
        "dashboard_model.x entries should have distinct model_paths, both are: {:?}",
        dashboard_entries[0].model_path
    );

    // One dashboard path passes through left_model, the other through right_model
    let dashboard_has_left = dashboard_entries
        .iter()
        .any(|ic| ic.model_path.iter().any(|(m, _, _)| m == "left_model"));
    let dashboard_has_right = dashboard_entries
        .iter()
        .any(|ic| ic.model_path.iter().any(|(m, _, _)| m == "right_model"));
    assert!(
        dashboard_has_left,
        "one dashboard_model.x path should pass through left_model, paths: {:?}",
        dashboard_entries
            .iter()
            .map(|ic| &ic.model_path)
            .collect::<Vec<_>>()
    );
    assert!(
        dashboard_has_right,
        "one dashboard_model.x path should pass through right_model, paths: {:?}",
        dashboard_entries
            .iter()
            .map(|ic| &ic.model_path)
            .collect::<Vec<_>>()
    );
}
