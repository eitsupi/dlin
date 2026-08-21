use super::*;
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

/// Build a manifest for testing that off-path model errors don't leak into the report:
///
///   source_model (col_x, col_y)
///        |
///   +----+------+
///   |            |
/// relevant_model  sibling_model
///  (col_x)        (col_y, sibling_fail)
///
/// relevant_model: SQL = `SELECT col_x FROM source_model`
///   → col_x resolves fine
///
/// sibling_model: SQL = `SELECT col_y FROM source_model`, YAML also has sibling_fail
///   → col_y resolves, but sibling_fail is in YAML only → lineage error
///
/// When tracing source_model.col_x:
/// - relevant_model.col_x is on path
/// - sibling_model has no column referencing col_x → off path
/// - sibling_model's errors (sibling_fail) should NOT appear in the report
fn make_off_path_error_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    // source_model: outputs col_x and col_y
    let mut src_cols = HashMap::new();
    for name in ["col_x", "col_y"] {
        src_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.source_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.source_model".to_string(),
            name: "source_model".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: src_cols,
            compiled_code: Some("select col_x, col_y from raw_table".to_string()),
            database: None,
            schema: None,
        },
    );

    // relevant_model: only col_x from source_model
    let mut rel_cols = HashMap::new();
    rel_cols.insert(
        "col_x".to_string(),
        ManifestColumn {
            name: "col_x".to_string(),
        },
    );
    nodes.insert(
        "model.proj.relevant_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.relevant_model".to_string(),
            name: "relevant_model".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.source_model".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: rel_cols,
            compiled_code: Some("select col_x from source_model".to_string()),
            database: None,
            schema: None,
        },
    );

    // sibling_model: col_y from source_model, plus sibling_fail in YAML (not in SQL)
    let mut sib_cols = HashMap::new();
    sib_cols.insert(
        "col_y".to_string(),
        ManifestColumn {
            name: "col_y".to_string(),
        },
    );
    sib_cols.insert(
        "sibling_fail".to_string(),
        ManifestColumn {
            name: "sibling_fail".to_string(),
        },
    );
    nodes.insert(
        "model.proj.sibling_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.sibling_model".to_string(),
            name: "sibling_model".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.source_model".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: sib_cols,
            compiled_code: Some("select col_y from source_model".to_string()),
            database: None,
            schema: None,
        },
    );

    Manifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        ..Default::default()
    }
}

#[test]
fn test_column_impact_excludes_off_path_errors() {
    let manifest = make_off_path_error_manifest();
    let result = compute_column_impact(
        &manifest,
        "source_model",
        "col_x",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // relevant_model.col_x must be on the impact path
    assert!(
        result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "relevant_model" && ic.column == "col_x"),
        "relevant_model.col_x should be impacted, got: {:?}",
        result.impacted_columns
    );

    // sibling_model is not on the path for col_x
    assert!(
        !result
            .impacted_columns
            .iter()
            .any(|ic| ic.model == "sibling_model"),
        "sibling_model should not be impacted, got: {:?}",
        result.impacted_columns
    );

    // sibling_fail error (from off-path sibling_model) must not appear
    let sibling_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|e| e.what.contains("sibling_fail"))
        .collect();
    assert!(
        sibling_errors.is_empty(),
        "off-path errors from sibling_model should not appear in the impact report, \
         got errors: {:?}",
        result.errors
    );
}

/// Downstream model with no compiled SQL: its NoCompiledCode error must appear in the
/// impact report even though `found_on_path` is false (model-level failures are always
/// propagated so users know the analysis is incomplete).
#[test]
fn test_column_impact_propagates_model_level_errors_from_unreachable_downstream() {
    let mut manifest = make_off_path_error_manifest();

    // Add a model that depends on source_model but has no compiled_code.
    // It references col_x (same column we track), but we can never confirm this
    // because the model can't be analyzed.
    let mut cols = std::collections::HashMap::new();
    cols.insert(
        "col_x".to_string(),
        crate::parser::manifest::ManifestColumn {
            name: "col_x".to_string(),
        },
    );
    manifest.nodes.insert(
        "model.proj.broken_downstream".to_string(),
        crate::parser::manifest::ManifestNode {
            unique_id: "model.proj.broken_downstream".to_string(),
            name: "broken_downstream".to_string(),
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: vec!["model.proj.source_model".to_string()],
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: cols,
            compiled_code: None, // no compiled SQL
            database: None,
            schema: None,
        },
    );

    let result = compute_column_impact(
        &manifest,
        "source_model",
        "col_x",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // broken_downstream can't be analyzed, so its NoCompiledCode error must appear
    let has_broken_error = result
        .errors
        .iter()
        .any(|e| e.what.contains("broken_downstream"));
    assert!(
        has_broken_error,
        "model-level errors from unanalyzable downstream models should appear in the report, \
         got errors: {:?}",
        result.errors
    );
    // But sibling_fail (ColumnNotFound from off-path sibling_model) must still be absent
    let has_sibling_error = result
        .errors
        .iter()
        .any(|e| e.what.contains("sibling_fail"));
    assert!(
        !has_sibling_error,
        "ColumnNotFound from off-path model must not appear, got errors: {:?}",
        result.errors
    );
}

// --- ColumnLineageCache tests ---
