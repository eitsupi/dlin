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
    assert_eq!(result.slots().len(), 1);
    assert_eq!(result.slots()[0].column(), "id");
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

    // The star cannot be expanded: nothing is listed and the shape is open.
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert!(result.slots().is_empty());
    assert!(matches!(
        result.output.as_ref().map(|output| &output.shape),
        Some(OutputShape::Open { unexpanded }) if unexpanded.len() == 1
    ));
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
    assert_eq!(result.listed_columns, 4);
    assert_eq!(result.slots().len(), 4);
}

#[test]
fn test_dependency_provides_its_sql_output_and_not_its_yaml_columns() {
    let mut manifest = make_test_manifest();
    let dependency = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    dependency.columns = ["yaml_only"]
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
    // Outputs follow the SQL projection order of the reader.
    assert_eq!(result.names(), ["yaml_only", "sql_only"]);

    // The catalog that the reader is analysed against holds the SQL output of
    // the dependency only; its YAML declaration is a separate record.
    let catalog = schema::SchemaContext::new(&manifest, DlinDialect::Generic)
        .catalog_for(manifest.nodes.get("model.proj.orders").unwrap())
        .unwrap();
    assert_eq!(catalog.table_columns("stg_orders"), Some(vec!["sql_only"]));
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
    assert_eq!(result.slots().len(), 4);

    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    assert_eq!(order_id.lineage.known()[0].column, "id");
}

#[test]
fn test_select_star_hint_when_select_star_unresolved() {
    // When SELECT * cannot be expanded (external table not in manifest),
    // Diagnostics for columns that an unexpanded star may provide carry the SELECT * hint.
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
        "diagnostics should include SELECT * hint when stars remain unresolved; got: {:?}",
        result.errors
    );
}

#[test]
fn test_select_star_hint_when_cte_select_star_unresolved() {
    // When a CTE body has SELECT * from an external table, the hint should still
    // fire for the outer query's indeterminate-column diagnostics even though the outermost
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
        "indeterminate-column diagnostics for CTE-nested stars should include SELECT * hint; got: {:?}",
        result.errors
    );
}

#[test]
fn test_select_star_hint_when_derived_table_select_star_unresolved() {
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
        "indeterminate-column diagnostics for derived-table stars should include SELECT * hint; got: {:?}",
        result.errors
    );
}

#[test]
fn test_select_star_hint_when_join_select_star_unresolved() {
    // JOIN-derived-table pattern: SELECT id FROM base JOIN (SELECT * FROM ext) src ON true
    // The star lives inside a JOIN subquery, not the outermost select list or FROM clause.
    let mut manifest = make_test_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some(
        "SELECT src.id FROM some_table JOIN (SELECT * FROM some_external_table) src ON 1=1"
            .to_string(),
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
        "indeterminate-column diagnostics for JOIN-derived-table stars should include SELECT * hint; got: {:?}",
        result.errors
    );
}

#[test]
fn test_select_star_hint_reaches_public_result_for_indeterminate_star() {
    let mut manifest = make_test_manifest();
    let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    node.columns = ["c1"]
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
    node.compiled_code = Some("SELECT * FROM some_external_table".to_string());

    let result = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // The declared column is not an output; the open shape is why it cannot be
    // confirmed, and the hint says so.
    assert!(result.slots().is_empty());
    let c1_error = result
        .errors
        .iter()
        .find(|error| error.what.contains("column c1 "))
        .expect("c1 should have a declaration diagnostic");
    assert_eq!(
        c1_error.kind,
        ColumnLineageErrorKind::DeclaredColumnNotEnumerated
    );
    assert!(!c1_error.is_fatal());
    assert!(
        c1_error.hint.as_deref().unwrap_or("").contains("SELECT *"),
        "the public result should retain the SELECT * hint: {c1_error:?}"
    );
}
