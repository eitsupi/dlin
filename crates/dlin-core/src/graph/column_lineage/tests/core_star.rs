use super::*;
#[test]
fn test_top_level_set_operation_infers_output_names() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.clear();
    node.compiled_code =
        Some("select id from raw.orders union all select id from raw.orders".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.columns[0].column, "id");
}

#[test]
fn test_bare_select_star_without_catalog_does_not_infer_output_names() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.clear();
    node.depends_on.nodes.clear();
    node.compiled_code = Some("select * from unknown_table".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.columns.is_empty());
    assert_eq!(
        result.errors[0].kind,
        ColumnLineageErrorKind::ColumnInferenceFailed
    );
}

#[test]
fn test_catalog_expands_bare_select_star_output_names() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns.clear();
    node.compiled_code =
        Some("with source as (select * from raw.orders) select * from source".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.total_columns, 4);
    assert_eq!(result.columns.len(), 4);
}

#[test]
fn test_dependency_catalog_unions_yaml_and_compiled_sql_columns() {
    let mut manifest = make_test_manifest();
    let dependency = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    dependency.columns = ["yaml_only"]
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
    dependency.compiled_code = Some("select id as sql_only from raw.orders".to_string());

    let root = manifest.nodes.get_mut("model.proj.orders").unwrap();
    root.columns.clear();
    root.depends_on.nodes = vec!["model.proj.stg_orders".to_string()];
    root.compiled_code = Some("select yaml_only, sql_only from stg_orders".to_string());

    let result = compute_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        result
            .columns
            .iter()
            .map(|column| column.column.as_str())
            .collect::<Vec<_>>(),
        vec!["sql_only", "yaml_only"]
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
        DlinDialect::Generic,
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
fn test_column_not_found_hint_reaches_public_result_for_indeterminate_star() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns = ["c1"]
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
    node.compiled_code = Some("SELECT * FROM some_external_table".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let c1_error = result
        .errors
        .iter()
        .find(|error| error.what.starts_with("column 'c1':"))
        .expect("c1 should have a column-lineage error");
    assert!(
        c1_error.hint.as_deref().unwrap_or("").contains("SELECT *"),
        "the public result should retain the SELECT * hint: {c1_error:?}"
    );
}
