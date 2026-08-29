use super::*;

#[test]
fn test_collect_file_paths() {
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                "model.proj.stg_orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.stg_orders".to_string(),
                    name: "stg_orders".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: Some("models/staging/stg_orders.sql".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
            (
                "model.proj.orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.orders".to_string(),
                    name: "orders".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: Some("models/marts/orders.sql".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
            (
                "model.proj.bare".to_string(),
                ManifestNode {
                    unique_id: "model.proj.bare".to_string(),
                    name: "bare".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
        ]),
        sources: HashMap::from([(
            "source.proj.raw.orders".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.orders".to_string(),
                name: "orders".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: None,
                path: Some("models/staging/schema.yml".to_string()),
                original_file_path: None,
                columns: HashMap::new(),
                database: None,
                schema: None,
                identifier: None,
            },
        )]),
        ..Default::default()
    };

    let paths = manifest.collect_file_paths();
    assert_eq!(paths.len(), 3);
    assert!(paths.contains("models/staging/stg_orders.sql"));
    assert!(paths.contains("models/marts/orders.sql"));
    assert!(paths.contains("models/staging/schema.yml"));
    // bare has no path, should not appear
    assert!(!paths.iter().any(|p| p.contains("bare")));
}

#[test]
fn test_collect_file_paths_deduplicates() {
    // Multiple sources can reference the same YAML file
    let manifest = Manifest {
        nodes: HashMap::new(),
        sources: HashMap::from([
            (
                "source.proj.raw.orders".to_string(),
                ManifestSource {
                    unique_id: "source.proj.raw.orders".to_string(),
                    name: "orders".to_string(),
                    source_name: "raw".to_string(),
                    resource_type: "source".to_string(),
                    description: None,
                    path: Some("models/staging/schema.yml".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    database: None,
                    schema: None,
                    identifier: None,
                },
            ),
            (
                "source.proj.raw.customers".to_string(),
                ManifestSource {
                    unique_id: "source.proj.raw.customers".to_string(),
                    name: "customers".to_string(),
                    source_name: "raw".to_string(),
                    resource_type: "source".to_string(),
                    description: None,
                    path: Some("models/staging/schema.yml".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    database: None,
                    schema: None,
                    identifier: None,
                },
            ),
        ]),
        ..Default::default()
    };

    let paths = manifest.collect_file_paths();
    assert_eq!(paths.len(), 1, "Duplicate paths should be deduplicated");
}

#[test]
fn test_load_manifest() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/simple_project/target/manifest.json");

    let manifest = load_manifest(&fixture_path).unwrap();
    assert!(!manifest.nodes.is_empty());
    assert!(!manifest.sources.is_empty());

    let paths = manifest.collect_file_paths();
    assert!(paths.contains("models/staging/stg_orders.sql"));
    assert!(paths.contains("models/staging/schema.yml"));
}

#[test]
fn test_collect_sql_contents_from_manifest() {
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                "model.proj.stg_orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.stg_orders".to_string(),
                    name: "stg_orders".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: Some("select * from raw.orders".to_string()),
                    database: None,
                    schema: None,
                },
            ),
            (
                "test.proj.not_null_orders_id.abc123".to_string(),
                ManifestNode {
                    unique_id: "test.proj.not_null_orders_id.abc123".to_string(),
                    name: "not_null_orders_id".to_string(),
                    alias: None,
                    resource_type: "test".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: Some("select count(*) from orders where id is null".to_string()),
                    database: None,
                    schema: None,
                },
            ),
            (
                "model.proj.no_compile".to_string(),
                ManifestNode {
                    unique_id: "model.proj.no_compile".to_string(),
                    name: "no_compile".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
        ]),
        sources: HashMap::new(),
        ..Default::default()
    };

    let sql_contents = manifest.collect_sql_contents();

    // compiled_code present → included
    assert_eq!(
        sql_contents
            .get("model.proj.stg_orders")
            .map(|s| s.as_str()),
        Some("select * from raw.orders")
    );
    // Compiled SQL keys use the canonical manifest unique_id.
    assert_eq!(
        sql_contents
            .get("test.proj.not_null_orders_id.abc123")
            .map(|s| s.as_str()),
        Some("select count(*) from orders where id is null")
    );
    // compiled_code absent → omitted
    assert!(!sql_contents.contains_key("model.no_compile"));
}

#[test]
fn test_collect_sql_contents_from_fixture() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/simple_project/target/manifest.json");

    let manifest = load_manifest(&fixture_path).unwrap();
    let sql_contents = manifest.collect_sql_contents();

    // The fixture has compiled_code for stg_orders and the test node
    assert!(
        sql_contents.contains_key("model.simple_project.stg_orders"),
        "stg_orders should have compiled_code"
    );
    assert!(
        sql_contents.contains_key("test.simple_project.assert_orders_positive_amount"),
        "test node should have compiled_code"
    );
    // Nodes without compiled_code should not appear
    assert!(
        !sql_contents.contains_key("model.simple_project.customers"),
        "customers has no compiled_code in fixture"
    );
}
