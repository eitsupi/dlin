use super::*;
use std::fs;

#[test]
fn test_resource_type_to_node_type() {
    assert_eq!(resource_type_to_node_type("model"), NodeType::Model);
    assert_eq!(resource_type_to_node_type("source"), NodeType::Source);
    assert_eq!(resource_type_to_node_type("seed"), NodeType::Seed);
    assert_eq!(resource_type_to_node_type("snapshot"), NodeType::Snapshot);
    assert_eq!(resource_type_to_node_type("test"), NodeType::Test);
    assert_eq!(resource_type_to_node_type("analysis"), NodeType::Model);
    assert_eq!(resource_type_to_node_type("exposure"), NodeType::Exposure);
    assert_eq!(resource_type_to_node_type("unknown"), NodeType::Model);
}

#[test]
fn test_simplify_unique_id_model() {
    assert_eq!(
        simplify_unique_id("model.my_project.stg_orders", "model"),
        "model.stg_orders"
    );
}

#[test]
fn test_simplify_unique_id_source() {
    assert_eq!(
        simplify_unique_id("source.my_project.raw.orders", "source"),
        "source.raw.orders"
    );
}

#[test]
fn test_simplify_unique_id_short() {
    assert_eq!(
        simplify_unique_id("model.stg_orders", "model"),
        "model.stg_orders"
    );
}

#[test]
fn test_simplify_unique_id_source_short() {
    assert_eq!(
        simplify_unique_id("source.raw.orders", "source"),
        "source.raw.orders"
    );
}

#[test]
fn test_simplify_unique_id_test() {
    // test.project.test_name.hash -> test.test_name
    assert_eq!(
        simplify_unique_id(
            "test.jaffle_shop.not_null_orders_order_id.cf6c17daed",
            "test"
        ),
        "test.not_null_orders_order_id"
    );
}

#[test]
fn test_simplify_unique_id_test_short() {
    assert_eq!(
        simplify_unique_id("test.not_null_orders_order_id", "test"),
        "test.not_null_orders_order_id"
    );
}

#[test]
fn test_simplify_unique_id_versioned_model() {
    // dbt versioned model unique_ids: model.project.name.v{N} → model.name.v{N}
    assert_eq!(
        simplify_unique_id("model.my_project.my_model.v1", "model"),
        "model.my_model.v1"
    );
    assert_eq!(
        simplify_unique_id("model.my_project.my_model.v2", "model"),
        "model.my_model.v2"
    );
    // Unversioned model must still work
    assert_eq!(
        simplify_unique_id("model.my_project.stg_orders", "model"),
        "model.stg_orders"
    );
}

#[test]
fn test_infer_edge_type() {
    assert_eq!(
        infer_edge_type("source.my_project.raw.orders"),
        EdgeType::Source
    );
    assert_eq!(
        infer_edge_type("model.my_project.stg_orders"),
        EdgeType::Ref
    );
    assert_eq!(infer_edge_type("test.my_project.some_test"), EdgeType::Test);
    assert_eq!(infer_edge_type("seed.my_project.countries"), EdgeType::Ref);
}

#[test]
fn test_non_empty_string() {
    assert_eq!(non_empty_string(&None), None);
    assert_eq!(non_empty_string(&Some("".to_string())), None);
    assert_eq!(non_empty_string(&Some("  ".to_string())), None);
    assert_eq!(
        non_empty_string(&Some("hello".to_string())),
        Some("hello".to_string())
    );
}

#[test]
fn test_build_graph_from_minimal_manifest() {
    let manifest = Manifest {
        nodes: HashMap::from([(
            "model.proj.stg_orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_orders".to_string(),
                name: "stg_orders".to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["source.proj.raw.orders".to_string()],
                },
                config: ManifestConfig {
                    alias: None,
                    materialized: Some("view".to_string()),
                    tags: vec!["staging".to_string()],
                },
                description: Some("Staged orders".to_string()),
                path: Some("models/staging/stg_orders.sql".to_string()),
                original_file_path: None,
                columns: HashMap::new(),
                compiled_code: None,
                database: None,
                schema: None,
            },
        )]),
        sources: HashMap::from([(
            "source.proj.raw.orders".to_string(),
            ManifestSource {
                unique_id: "source.proj.raw.orders".to_string(),
                name: "orders".to_string(),
                source_name: "raw".to_string(),
                resource_type: "source".to_string(),
                description: Some("Raw orders table".to_string()),
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

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();

    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);

    // Find the model node
    let model = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Model)
        .expect("Should have a model node");
    assert_eq!(graph[model].label, "stg_orders");
    assert_eq!(graph[model].unique_id, "model.proj.stg_orders");
    assert_eq!(graph[model].aliases, vec!["model.stg_orders"]);
    assert_eq!(graph[model].materialization.as_deref(), Some("view"));
    assert_eq!(graph[model].tags, vec!["staging"]);
    assert_eq!(graph[model].description.as_deref(), Some("Staged orders"));

    // Find the source node
    let source = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Source)
        .expect("Should have a source node");
    assert_eq!(graph[source].label, "raw.orders");
    assert_eq!(graph[source].unique_id, "source.proj.raw.orders");
    assert_eq!(graph[source].aliases, vec!["source.raw.orders"]);
}

#[test]
fn test_build_graph_preserves_canonical_manifest_ids_and_ambiguous_aliases() {
    let mut nodes = HashMap::new();
    for package in ["package_a", "package_b"] {
        let orig_id = format!("model.{package}.orders");
        nodes.insert(
            orig_id.clone(),
            ManifestNode {
                unique_id: orig_id,
                name: "orders".to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn::default(),
                config: ManifestConfig::default(),
                description: None,
                path: None,
                original_file_path: None,
                columns: HashMap::new(),
                compiled_code: Some(format!("select '{package}'")),
                database: None,
                schema: None,
            },
        );
    }
    nodes.insert(
        "model.consumer.reporting".to_string(),
        ManifestNode {
            unique_id: "model.consumer.reporting".to_string(),
            name: "reporting".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec![
                    "model.package_a.orders".to_string(),
                    "model.package_b.orders".to_string(),
                ],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: HashMap::new(),
            compiled_code: None,
            database: None,
            schema: None,
        },
    );

    let manifest = Manifest {
        nodes,
        ..Default::default()
    };
    let sql_contents = manifest.collect_sql_contents();
    assert_eq!(
        sql_contents
            .get("model.package_a.orders")
            .map(String::as_str),
        Some("select 'package_a'")
    );
    assert_eq!(
        sql_contents
            .get("model.package_b.orders")
            .map(String::as_str),
        Some("select 'package_b'")
    );
    assert!(!sql_contents.contains_key("model.orders"));

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();

    let package_a = graph
        .node_indices()
        .find(|&index| graph[index].unique_id == "model.package_a.orders")
        .expect("package_a model should retain its full unique_id");
    let package_b = graph
        .node_indices()
        .find(|&index| graph[index].unique_id == "model.package_b.orders")
        .expect("package_b model should retain its full unique_id");
    let reporting = graph
        .node_indices()
        .find(|&index| graph[index].unique_id == "model.consumer.reporting")
        .expect("non-colliding model should retain its canonical unique_id");

    assert_ne!(package_a, package_b);
    assert_eq!(graph[package_a].aliases, vec!["model.orders"]);
    assert_eq!(graph[package_b].aliases, vec!["model.orders"]);
    assert_eq!(graph[reporting].aliases, vec!["model.reporting"]);
    assert!(graph.find_edge(package_a, reporting).is_some());
    assert!(graph.find_edge(package_b, reporting).is_some());
}

#[test]
fn test_build_graph_with_exposures() {
    let manifest = Manifest {
        nodes: HashMap::from([(
            "model.proj.orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.orders".to_string(),
                name: "orders".to_string(),
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
        )]),
        sources: HashMap::new(),
        exposures: HashMap::from([(
            "exposure.proj.weekly_report".to_string(),
            ManifestExposure {
                unique_id: "exposure.proj.weekly_report".to_string(),
                name: "weekly_report".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.orders".to_string()],
                },
                description: Some("Weekly dashboard".to_string()),
                label: None,
                exposure_type: None,
                url: None,
                maturity: None,
                owner: None,
            },
        )]),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);

    let exposure = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Exposure)
        .expect("Should have an exposure node");
    assert_eq!(graph[exposure].label, "weekly_report");
    assert_eq!(
        graph[exposure].description.as_deref(),
        Some("Weekly dashboard")
    );
}

#[test]
fn test_exposure_metadata_parsed() {
    let manifest = Manifest {
        nodes: HashMap::new(),
        sources: HashMap::new(),
        exposures: HashMap::from([(
            "exposure.proj.dashboard".to_string(),
            ManifestExposure {
                unique_id: "exposure.proj.dashboard".to_string(),
                name: "dashboard".to_string(),
                depends_on: DependsOn { nodes: vec![] },
                description: Some("Main dashboard".to_string()),
                label: Some("Main Dashboard".to_string()),
                exposure_type: Some("dashboard".to_string()),
                url: Some("https://bi.example.com".to_string()),
                maturity: Some("high".to_string()),
                owner: Some(ManifestExposureOwner {
                    name: Some("Data Team".to_string()),
                    email: Some("data@example.com".to_string()),
                }),
            },
        )]),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    let exp_idx = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Exposure)
        .expect("Should have an exposure node");
    let exp = &graph[exp_idx];

    let info = exp.exposure.as_ref().expect("Should have exposure info");
    assert_eq!(info.label.as_deref(), Some("Main Dashboard"));
    assert_eq!(info.exposure_type.as_deref(), Some("dashboard"));
    assert_eq!(info.url.as_deref(), Some("https://bi.example.com"));
    assert_eq!(info.maturity.as_deref(), Some("high"));

    let owner = info.owner.as_ref().expect("Should have owner");
    assert_eq!(owner.name.as_deref(), Some("Data Team"));
    assert_eq!(owner.email.as_deref(), Some("data@example.com"));
}

#[test]
fn test_exposure_metadata_from_fixture() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/simple_project/target/manifest.json");
    let graph = build_graph_from_manifest(&manifest_path).unwrap();

    let exp_idx = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Exposure)
        .expect("Should have an exposure node from fixture");
    let exp = &graph[exp_idx];
    assert_eq!(exp.label, "weekly_report");

    let info = exp.exposure.as_ref().expect("Should have exposure info");
    assert_eq!(info.label.as_deref(), Some("Weekly Report"));
    assert_eq!(info.exposure_type.as_deref(), Some("dashboard"));
    assert_eq!(info.url.as_deref(), Some("https://bi.example.com/weekly"));
    assert_eq!(info.maturity.as_deref(), Some("high"));

    let owner = info.owner.as_ref().expect("Should have owner");
    assert_eq!(owner.name.as_deref(), Some("Data Team"));
    assert_eq!(owner.email.as_deref(), Some("data@example.com"));
}

#[test]
fn test_build_graph_with_seeds_and_snapshots() {
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                "seed.proj.countries".to_string(),
                ManifestNode {
                    unique_id: "seed.proj.countries".to_string(),
                    name: "countries".to_string(),
                    alias: None,
                    resource_type: "seed".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: Some("seeds/countries.csv".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
            (
                "snapshot.proj.snap_orders".to_string(),
                ManifestNode {
                    unique_id: "snapshot.proj.snap_orders".to_string(),
                    name: "snap_orders".to_string(),
                    alias: None,
                    resource_type: "snapshot".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig {
                        alias: None,
                        materialized: Some("snapshot".to_string()),
                        tags: vec![],
                    },
                    description: None,
                    path: Some("snapshots/snap_orders.sql".to_string()),
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

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    assert_eq!(graph.node_count(), 2);

    let seed = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Seed)
        .expect("Should have a seed node");
    assert_eq!(graph[seed].label, "countries");

    let snap = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Snapshot)
        .expect("Should have a snapshot node");
    assert_eq!(graph[snap].label, "snap_orders");
}

#[test]
fn test_build_graph_with_tests() {
    let manifest = Manifest {
        nodes: HashMap::from([
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
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
            (
                "test.proj.assert_positive".to_string(),
                ManifestNode {
                    unique_id: "test.proj.assert_positive".to_string(),
                    name: "assert_positive".to_string(),
                    alias: None,
                    resource_type: "test".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["model.proj.orders".to_string()],
                    },
                    config: ManifestConfig::default(),
                    description: None,
                    path: Some("tests/assert_positive.sql".to_string()),
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

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);

    let test_node = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Test)
        .expect("Should have a test node");
    assert_eq!(graph[test_node].label, "assert_positive");

    // Edge to test node should use EdgeType::Test, not EdgeType::Ref
    use petgraph::visit::IntoEdgeReferences;
    let edge = graph.edge_references().next().unwrap();
    assert_eq!(edge.weight().edge_type, EdgeType::Test);
}

#[test]
fn test_build_graph_empty_manifest() {
    let manifest = Manifest {
        nodes: HashMap::new(),
        sources: HashMap::new(),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    assert_eq!(graph.node_count(), 0);
    assert_eq!(graph.edge_count(), 0);
}

#[test]
fn test_build_graph_missing_dependency() {
    // A node depends on something not in the manifest -- edge is skipped gracefully
    let manifest = Manifest {
        nodes: HashMap::from([(
            "model.proj.orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.orders".to_string(),
                name: "orders".to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: vec!["model.proj.nonexistent".to_string()],
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                original_file_path: None,
                columns: HashMap::new(),
                compiled_code: None,
                database: None,
                schema: None,
            },
        )]),
        sources: HashMap::new(),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    assert_eq!(graph.node_count(), 1);
    assert_eq!(graph.edge_count(), 0); // Edge to nonexistent node is skipped
}

#[test]
fn test_build_graph_optional_fields() {
    let manifest = Manifest {
        nodes: HashMap::from([(
            "model.proj.bare".to_string(),
            ManifestNode {
                unique_id: "model.proj.bare".to_string(),
                name: "bare".to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn::default(),
                config: ManifestConfig {
                    alias: None,
                    materialized: None,
                    tags: vec![],
                },
                description: None,
                path: None,
                original_file_path: None,
                columns: HashMap::new(),
                compiled_code: None,
                database: None,
                schema: None,
            },
        )]),
        sources: HashMap::new(),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    let node = &graph[graph.node_indices().next().unwrap()];
    assert!(node.description.is_none());
    assert!(node.materialization.is_none());
    assert!(node.tags.is_empty());
    assert!(node.file_path.is_none());
}

#[test]
fn test_build_graph_from_manifest_file() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("manifest.json");

    let manifest_json = r#"{
            "nodes": {
                "model.proj.stg_orders": {
                    "unique_id": "model.proj.stg_orders",
                    "name": "stg_orders",
                    "resource_type": "model",
                    "depends_on": { "nodes": ["source.proj.raw.orders"] },
                    "config": { "materialized": "view", "tags": [] },
                    "description": "Staged orders",
                    "path": "models/staging/stg_orders.sql"
                }
            },
            "sources": {
                "source.proj.raw.orders": {
                    "unique_id": "source.proj.raw.orders",
                    "name": "orders",
                    "source_name": "raw",
                    "resource_type": "source",
                    "description": "Raw orders",
                    "path": "models/staging/schema.yml"
                }
            },
            "exposures": {}
        }"#;

    fs::write(&manifest_path, manifest_json).unwrap();

    let graph = build_graph_from_manifest(&manifest_path).unwrap();
    assert_eq!(graph.node_count(), 2);
    assert_eq!(graph.edge_count(), 1);
}

#[test]
fn test_build_graph_from_manifest_file_not_found() {
    let result = build_graph_from_manifest(Path::new("/nonexistent/manifest.json"));
    assert!(result.is_err());
}

#[test]
fn test_build_graph_from_manifest_invalid_json() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest_path = tmp.path().join("manifest.json");
    fs::write(&manifest_path, "not valid json").unwrap();
    let result = build_graph_from_manifest(&manifest_path);
    assert!(result.is_err());
}

#[test]
fn test_original_file_path_preferred_over_path() {
    // dbt >= 1.x sets path to the models-dir-relative path and
    // original_file_path to the project-root-relative path. The latter is
    // preferred when matching a graph node back to a source file.
    let manifest = Manifest {
        nodes: HashMap::from([(
            "model.proj.stg_orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.stg_orders".to_string(),
                name: "stg_orders".to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn::default(),
                config: ManifestConfig::default(),
                description: None,
                path: Some("staging/stg_orders.sql".to_string()),
                original_file_path: Some("models/staging/stg_orders.sql".to_string()),
                columns: HashMap::new(),
                compiled_code: None,
                database: None,
                schema: None,
            },
        )]),
        sources: HashMap::new(),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    let node = &graph[graph.node_indices().next().unwrap()];
    assert_eq!(
        node.file_path.as_ref().map(|path| path.to_str().unwrap()),
        Some("models/staging/stg_orders.sql")
    );
}

#[test]
fn test_build_graph_analysis_maps_to_model() {
    let manifest = Manifest {
        nodes: HashMap::from([(
            "analysis.proj.my_analysis".to_string(),
            ManifestNode {
                unique_id: "analysis.proj.my_analysis".to_string(),
                name: "my_analysis".to_string(),
                alias: None,
                resource_type: "analysis".to_string(),
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
        )]),
        sources: HashMap::new(),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    let node = &graph[graph.node_indices().next().unwrap()];
    assert_eq!(node.node_type, NodeType::Model);
}

#[test]
fn test_build_graph_complex_chain() {
    // source -> stg_orders -> orders (with multiple deps)
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                "model.proj.stg_orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.stg_orders".to_string(),
                    name: "stg_orders".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["source.proj.raw.orders".to_string()],
                    },
                    config: ManifestConfig {
                        alias: None,
                        materialized: Some("view".to_string()),
                        tags: vec![],
                    },
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
            (
                "model.proj.stg_payments".to_string(),
                ManifestNode {
                    unique_id: "model.proj.stg_payments".to_string(),
                    name: "stg_payments".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["source.proj.raw.payments".to_string()],
                    },
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
            (
                "model.proj.orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.orders".to_string(),
                    name: "orders".to_string(),
                    alias: None,
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec![
                            "model.proj.stg_orders".to_string(),
                            "model.proj.stg_payments".to_string(),
                        ],
                    },
                    config: ManifestConfig {
                        alias: None,
                        materialized: Some("table".to_string()),
                        tags: vec!["marts".to_string()],
                    },
                    description: Some("Order fact table".to_string()),
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            ),
        ]),
        sources: HashMap::from([
            (
                "source.proj.raw.orders".to_string(),
                ManifestSource {
                    unique_id: "source.proj.raw.orders".to_string(),
                    name: "orders".to_string(),
                    source_name: "raw".to_string(),
                    resource_type: "source".to_string(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    database: None,
                    schema: None,
                    identifier: None,
                },
            ),
            (
                "source.proj.raw.payments".to_string(),
                ManifestSource {
                    unique_id: "source.proj.raw.payments".to_string(),
                    name: "payments".to_string(),
                    source_name: "raw".to_string(),
                    resource_type: "source".to_string(),
                    description: None,
                    path: None,
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

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
    // 2 sources + 3 models = 5 nodes
    assert_eq!(graph.node_count(), 5);
    // source.raw.orders -> stg_orders, source.raw.payments -> stg_payments,
    // stg_orders -> orders, stg_payments -> orders = 4 edges
    assert_eq!(graph.edge_count(), 4);
}

#[test]
fn test_build_graph_from_fixture_manifest() {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/simple_project/target/manifest.json");

    if !fixture_path.exists() {
        // Skip if fixture not yet created
        return;
    }

    let graph = build_graph_from_manifest(&fixture_path).unwrap();

    // The fixture has: 3 sources, 3 staging models, 2 mart models, 1 seed, 1 test, 1 exposure
    // = 11 nodes total
    assert!(
        graph.node_count() >= 10,
        "Expected at least 10 nodes, got {}",
        graph.node_count()
    );

    // Check we have all node types present
    let has_source = graph
        .node_indices()
        .any(|i| graph[i].node_type == NodeType::Source);
    let has_model = graph
        .node_indices()
        .any(|i| graph[i].node_type == NodeType::Model);
    let has_seed = graph
        .node_indices()
        .any(|i| graph[i].node_type == NodeType::Seed);
    let has_test = graph
        .node_indices()
        .any(|i| graph[i].node_type == NodeType::Test);
    let has_exposure = graph
        .node_indices()
        .any(|i| graph[i].node_type == NodeType::Exposure);

    assert!(has_source, "Should have source nodes");
    assert!(has_model, "Should have model nodes");
    assert!(has_seed, "Should have seed nodes");
    assert!(has_test, "Should have test nodes");
    assert!(has_exposure, "Should have exposure nodes");

    // Check edges exist
    assert!(graph.edge_count() > 0, "Should have edges");
}

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

#[test]
fn test_build_graph_with_semantic_layer_nodes() {
    let manifest = Manifest {
        nodes: HashMap::from([(
            "model.proj.orders".to_string(),
            ManifestNode {
                unique_id: "model.proj.orders".to_string(),
                name: "orders".to_string(),
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
        )]),
        sources: HashMap::new(),
        semantic_models: HashMap::from([(
            "semantic_model.proj.orders".to_string(),
            ManifestSemanticModel {
                unique_id: "semantic_model.proj.orders".to_string(),
                name: "orders".to_string(),
                label: None,
                depends_on: DependsOn {
                    nodes: vec!["model.proj.orders".to_string()],
                },
                description: Some("Orders semantic model".to_string()),
                path: None,
                original_file_path: None,
            },
        )]),
        metrics: HashMap::from([(
            "metric.proj.order_count".to_string(),
            ManifestMetric {
                unique_id: "metric.proj.order_count".to_string(),
                name: "order_count".to_string(),
                label: None,
                depends_on: DependsOn {
                    nodes: vec!["semantic_model.proj.orders".to_string()],
                },
                description: None,
                path: None,
                original_file_path: None,
            },
        )]),
        saved_queries: HashMap::from([(
            "saved_query.proj.order_metrics".to_string(),
            ManifestSavedQuery {
                unique_id: "saved_query.proj.order_metrics".to_string(),
                name: "order_metrics".to_string(),
                label: None,
                depends_on: DependsOn {
                    nodes: vec!["metric.proj.order_count".to_string()],
                },
                description: None,
                path: None,
                original_file_path: None,
            },
        )]),
        ..Default::default()
    };

    let graph = build_graph_from_parsed_manifest(&manifest).unwrap();

    // 4 nodes: model + semantic_model + metric + saved_query
    assert_eq!(graph.node_count(), 4);
    // 3 edges: model->sem, sem->metric, metric->saved_query
    assert_eq!(graph.edge_count(), 3);

    let sem = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::SemanticModel)
        .expect("Should have a semantic_model node");
    assert_eq!(graph[sem].unique_id, "semantic_model.proj.orders");
    assert_eq!(graph[sem].label, "orders");
    assert_eq!(
        graph[sem].description.as_deref(),
        Some("Orders semantic model")
    );

    let metric = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Metric)
        .expect("Should have a metric node");
    assert_eq!(graph[metric].unique_id, "metric.proj.order_count");

    let sq = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::SavedQuery)
        .expect("Should have a saved_query node");
    assert_eq!(graph[sq].unique_id, "saved_query.proj.order_metrics");
}

#[test]
fn test_semantic_layer_nodes_from_jaffle_shop_manifest() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../refs/jaffle-shop/target/manifest.json");
    if !manifest_path.exists() {
        eprintln!(
            "SKIP: jaffle-shop fixture not found at {manifest_path:?}; run `make fixtures` to enable this test"
        );
        return;
    }
    let graph = build_graph_from_manifest(&manifest_path).unwrap();

    let sem_models: Vec<_> = graph
        .node_indices()
        .filter(|&i| graph[i].node_type == NodeType::SemanticModel)
        .collect();
    assert!(!sem_models.is_empty(), "Should have semantic_model nodes");

    let metrics: Vec<_> = graph
        .node_indices()
        .filter(|&i| graph[i].node_type == NodeType::Metric)
        .collect();
    assert!(!metrics.is_empty(), "Should have metric nodes");

    let saved_queries: Vec<_> = graph
        .node_indices()
        .filter(|&i| graph[i].node_type == NodeType::SavedQuery)
        .collect();
    assert!(!saved_queries.is_empty(), "Should have saved_query nodes");

    // Each semantic_model should have at least one upstream edge (to a model)
    let sem_idx = sem_models[0];
    let has_upstream = graph
        .edges_directed(sem_idx, petgraph::Direction::Incoming)
        .next()
        .is_some();
    assert!(
        has_upstream,
        "semantic_model should have upstream model edge"
    );
}
