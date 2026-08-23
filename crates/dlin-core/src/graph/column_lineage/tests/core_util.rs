use super::*;
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
fn test_count_star_has_no_sources() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns = ["order_count"]
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
    node.compiled_code = Some("SELECT COUNT(*) AS order_count FROM raw_table".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let order_count = result
        .columns
        .iter()
        .find(|entry| entry.column == "order_count")
        .expect("COUNT(*) output should be traced");
    assert!(
        order_count.sources.is_empty(),
        "sources: {:?}",
        order_count.sources
    );
    assert!(
        order_count
            .sources
            .iter()
            .all(|source| !source.table.is_empty()),
        "COUNT(*) must not publish an empty-table source: {:?}",
        order_count.sources
    );
}

#[test]
fn test_unattributable_leaves_never_publish_empty_tables() {
    for (sql, column) in [
        (
            "SELECT COUNT(*) AS order_count FROM raw_table",
            "order_count",
        ),
        (
            "SELECT 1 AS constant_value FROM raw_table",
            "constant_value",
        ),
        ("SELECT CURRENT_DATE AS run_date FROM raw_table", "run_date"),
    ] {
        let mut manifest = make_test_manifest();
        let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
        node.columns = [column]
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
        node.compiled_code = Some(sql.to_string());

        let result = compute_column_lineage(
            &manifest,
            "stg_orders",
            DlinDialect::Generic,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(
            result.errors.is_empty(),
            "errors for {sql}: {:?}",
            result.errors
        );
        assert!(
            result.columns.iter().any(|entry| entry.column == column),
            "missing output column {column} for {sql}: {:?}",
            result.columns
        );
        assert!(
            result
                .columns
                .iter()
                .flat_map(|entry| &entry.sources)
                .all(|source| !source.table.is_empty()),
            "unattributable leaf published an empty-table source for {sql}: {:?}",
            result.columns
        );
    }
}

#[test]
fn test_source_table_has_empty_model_path() {
    // stg_orders references raw source directly — model_path should be empty
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
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
fn test_traced_total_columns_success() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_string(&result).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["traced_columns"], 4);
    assert_eq!(parsed["total_columns"], 4);
}

// --- Regression tests for known issues ---

#[test]
fn test_bigquery_source_free_unnest_is_traced_without_sources() {
    // A source-free BigQuery UNNEST projection remains a valid output while
    // exposing no physical source columns.
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
            alias: None,
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
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // The output is present and source-free by contract.
    let week_start = result
        .columns
        .iter()
        .find(|c| c.column == "week_start")
        .expect("week_start should be present");
    assert!(
        week_start.sources.is_empty(),
        "source-free UNNEST output should have no sources: {:?}",
        week_start.sources
    );
}
