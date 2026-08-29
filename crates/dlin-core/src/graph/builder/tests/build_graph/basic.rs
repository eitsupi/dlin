#[test]
fn test_build_graph_uses_logical_stem_for_jinja_sql_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    std::fs::create_dir_all(&models_dir).unwrap();
    std::fs::write(models_dir.join("upstream.sql"), "SELECT 1").unwrap();
    std::fs::write(
        models_dir.join("orders.sql.jinja"),
        "SELECT * FROM {{ ref('upstream') }}",
    )
    .unwrap();

    let orders_path = models_dir.join("orders.sql.jinja");
    let files = DiscoveredFiles {
        model_sql_files: vec![models_dir.join("upstream.sql"), orders_path.clone()],
        ..Default::default()
    };
    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    let orders = graph
        .node_indices()
        .find(|&idx| graph[idx].unique_id == "model.orders")
        .expect("suffixed SQL file should use its logical model name");
    assert_eq!(graph[orders].label, "orders");
    assert_eq!(
        graph[orders].file_path.as_deref(),
        Some(std::path::Path::new("models/orders.sql.jinja"))
    );
    let upstream = graph
        .node_indices()
        .find(|&idx| graph[idx].unique_id == "model.upstream")
        .unwrap();
    assert!(graph.contains_edge(upstream, orders));
}

#[test]
fn test_build_graph_sources_and_models() {
    let (_tmp, project_dir) = setup_temp_project();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    // Use no_cache: false to exercise the cache-enabled path end-to-end
    let graph = build_graph(&project_dir, &files, None, false, false, &HashMap::new()).unwrap();

    // Should have source + 2 models = 3 nodes
    assert_eq!(graph.node_count(), 3);

    // Check node types
    let mut types: Vec<NodeType> = graph.node_indices().map(|i| graph[i].node_type).collect();
    types.sort_by_key(|t| format!("{:?}", t));
    assert!(types.contains(&NodeType::Source));
    assert!(types.iter().filter(|t| **t == NodeType::Model).count() == 2);

    // Should have 2 edges: source→stg_orders, stg_orders→orders
    assert_eq!(graph.edge_count(), 2);
}

#[test]
fn test_build_graph_with_seeds() {
    let (_tmp, project_dir) = setup_temp_project();

    // Add a seed
    let seeds_dir = project_dir.join("seeds");
    fs::create_dir_all(&seeds_dir).unwrap();
    fs::write(seeds_dir.join("countries.csv"), "id,name\n1,US\n").unwrap();

    let files = DiscoveredFiles {
        seed_files: vec![project_dir.join("seeds/countries.csv")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    assert_eq!(graph.node_count(), 1);
    let node = &graph[graph.node_indices().next().unwrap()];
    assert_eq!(node.node_type, NodeType::Seed);
    assert_eq!(node.label, "countries");
}

#[test]
fn test_build_graph_with_snapshots() {
    let (_tmp, project_dir) = setup_temp_project();

    let snap_dir = project_dir.join("snapshots");
    fs::create_dir_all(&snap_dir).unwrap();
    fs::write(snap_dir.join("snap_orders.sql"), "SELECT 1").unwrap();

    let files = DiscoveredFiles {
        snapshot_sql_files: vec![project_dir.join("snapshots/snap_orders.sql")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    assert_eq!(graph.node_count(), 1);
    let node = &graph[graph.node_indices().next().unwrap()];
    assert_eq!(node.node_type, NodeType::Snapshot);
    assert_eq!(node.label, "snap_orders");
}

#[test]
fn test_build_graph_with_tests() {
    let (_tmp, project_dir) = setup_temp_project();

    let test_dir = project_dir.join("tests");
    fs::create_dir_all(&test_dir).unwrap();
    fs::write(
        test_dir.join("assert_positive.sql"),
        "SELECT * FROM {{ ref('stg_orders') }} WHERE amount < 0",
    )
    .unwrap();

    // Need the model that the test references
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();
    fs::write(models_dir.join("stg_orders.sql"), "SELECT 1").unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
        test_sql_files: vec![project_dir.join("tests/assert_positive.sql")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // model + test = 2 nodes
    assert_eq!(graph.node_count(), 2);
    // test edge: stg_orders → assert_positive
    assert_eq!(graph.edge_count(), 1);

    // Singular SQL tests should use EdgeType::Test
    use petgraph::visit::IntoEdgeReferences;
    let edge = graph.edge_references().next().unwrap();
    assert_eq!(edge.weight().edge_type, EdgeType::Test);
}

#[test]
fn test_build_graph_with_exposures() {
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();
    fs::write(models_dir.join("orders.sql"), "SELECT 1").unwrap();

    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
sources: []
models: []
exposures:
  - name: weekly_report
    description: "Weekly report dashboard"
    depends_on:
      - ref('orders')
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/orders.sql")],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // model + exposure = 2 nodes
    assert_eq!(graph.node_count(), 2);
    // exposure edge: orders → weekly_report
    assert_eq!(graph.edge_count(), 1);
}

#[test]
fn test_build_graph_exposure_versioned_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    fs::write(models_dir.join("my_model_v1.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("my_model_v2.sql"), "SELECT 2").unwrap();
    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
models:
  - name: my_model
    latest_version: 2
    versions:
      - v: 1
      - v: 2
exposures:
  - name: pinned_report
    depends_on:
      - ref('my_model', version=1)
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model_v1.sql"),
            project_dir.join("models/my_model_v2.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // v1 + v2 + exposure = 3 nodes
    assert_eq!(graph.node_count(), 3);
    // Only one edge: my_model.v1 → pinned_report
    assert_eq!(graph.edge_count(), 1);

    // Confirm the edge is from v1, not v2
    let v1_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v1")
        .expect("model.my_model.v1 should exist");
    let exposure_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "exposure.pinned_report")
        .expect("exposure.pinned_report should exist");
    assert!(
        graph.contains_edge(v1_idx, exposure_idx),
        "exposure edge should be from v1"
    );
}

#[test]
fn test_build_graph_ref_resolves_to_seed() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();

    let models_dir = project_dir.join("models");
    let seeds_dir = project_dir.join("seeds");
    fs::create_dir_all(&models_dir).unwrap();
    fs::create_dir_all(&seeds_dir).unwrap();

    fs::write(seeds_dir.join("countries.csv"), "id,name\n1,US\n").unwrap();
    fs::write(
        models_dir.join("stg_countries.sql"),
        "SELECT * FROM {{ ref('countries') }}",
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/stg_countries.sql")],
        seed_files: vec![project_dir.join("seeds/countries.csv")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // seed + model = 2 nodes (no phantom)
    assert_eq!(graph.node_count(), 2);
    // ref edge: countries → stg_countries
    assert_eq!(graph.edge_count(), 1);

    // Verify the seed node is properly typed (not phantom)
    let seed_node = graph
        .node_indices()
        .find(|&i| graph[i].label == "countries")
        .unwrap();
    assert_eq!(graph[seed_node].node_type, NodeType::Seed);
}

#[test]
fn test_build_graph_phantom_node_for_unresolved_ref() {
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();
    fs::write(
        models_dir.join("orders.sql"),
        "SELECT * FROM {{ ref('nonexistent_model') }}",
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/orders.sql")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // model + phantom = 2 nodes
    assert_eq!(graph.node_count(), 2);
    let phantom = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Phantom)
        .expect("Should have a phantom node");
    assert_eq!(graph[phantom].label, "nonexistent_model");
}

#[test]
fn test_build_graph_phantom_node_for_unresolved_source() {
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();
    fs::write(
        models_dir.join("orders.sql"),
        "SELECT * FROM {{ source('unknown_src', 'unknown_table') }}",
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/orders.sql")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // model + phantom source = 2 nodes
    assert_eq!(graph.node_count(), 2);
    let phantom = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Phantom)
        .expect("Should have a phantom source node");
    assert_eq!(graph[phantom].label, "unknown_src.unknown_table");
}

#[test]
fn test_build_graph_model_descriptions() {
    let (_tmp, project_dir) = setup_temp_project();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    let stg = graph
        .node_indices()
        .find(|&i| graph[i].label == "stg_orders")
        .unwrap();
    assert_eq!(graph[stg].description.as_deref(), Some("Staged orders"));
}

#[test]
fn test_build_graph_edge_types() {
    use petgraph::visit::IntoEdgeReferences;

    let (_tmp, project_dir) = setup_temp_project();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    let edge_types: Vec<EdgeType> = graph
        .edge_references()
        .map(|e| e.weight().edge_type)
        .collect();
    assert!(edge_types.contains(&EdgeType::Source));
    assert!(edge_types.contains(&EdgeType::Ref));
}

#[test]
fn test_build_graph_empty_files() {
    let tmp = tempfile::tempdir().unwrap();
    let files = DiscoveredFiles::default();
    let graph = build_graph(tmp.path(), &files, None, true, false, &HashMap::new()).unwrap();
    assert_eq!(graph.node_count(), 0);
    assert_eq!(graph.edge_count(), 0);
}

#[test]
fn test_build_graph_model_config_merge() {
    // Covers lines 168-170: YAML model config with materialization and tags
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();

    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    fs::write(models_dir.join("stg_orders.sql"), "SELECT 1").unwrap();

    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
sources: []
models:
  - name: stg_orders
    description: "Staged orders"
    tags:
      - staging
    config:
      materialized: table
      tags:
        - daily
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    let stg = graph
        .node_indices()
        .find(|&i| graph[i].label == "stg_orders")
        .unwrap();
    assert_eq!(graph[stg].materialization.as_deref(), Some("table"));
    assert!(graph[stg].tags.contains(&"staging".to_string()));
    assert!(graph[stg].tags.contains(&"daily".to_string()));
}

#[test]
fn test_build_graph_duplicate_model_name() {
    // Covers line 197: duplicate model name warning
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();

    let models_dir = project_dir.join("models");
    let subdir = models_dir.join("subdir");
    fs::create_dir_all(&subdir).unwrap();

    fs::write(models_dir.join("orders.sql"), "SELECT 1").unwrap();
    fs::write(subdir.join("orders.sql"), "SELECT 2").unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/orders.sql"),
            project_dir.join("models/subdir/orders.sql"),
        ],
        ..Default::default()
    };

    // Should not panic, just warn on stderr about the duplicate
    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // Both SQL files produce nodes (duplicate warning is informational)
    let order_nodes: Vec<_> = graph
        .node_indices()
        .filter(|&i| graph[i].label == "orders")
        .collect();
    assert_eq!(order_nodes.len(), 2);
}

#[test]
fn test_build_graph_plain_and_jinja_sql_share_logical_model_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();
    fs::write(models_dir.join("orders.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("orders.sql.j2"), "SELECT 2").unwrap();

    let graph = build_graph(
        &project_dir,
        &DiscoveredFiles {
            model_sql_files: vec![
                models_dir.join("orders.sql"),
                models_dir.join("orders.sql.j2"),
            ],
            ..Default::default()
        },
        None,
        true,
        false,
        &HashMap::new(),
    )
    .unwrap();

    let model_nodes: Vec<_> = graph
        .node_indices()
        .filter(|&idx| graph[idx].node_type == NodeType::Model)
        .collect();
    assert_eq!(model_nodes.len(), 2);
    assert!(
        model_nodes
            .iter()
            .all(|&idx| graph[idx].unique_id == "model.orders")
    );
    assert!(model_nodes.iter().all(|&idx| graph[idx].label == "orders"));
    assert!(model_nodes.iter().all(|&idx| {
        graph[idx]
            .file_path
            .as_ref()
            .is_some_and(|path| path != std::path::Path::new("models/orders.sql.sql"))
    }));
}

#[test]
fn test_build_graph_file_paths_are_relative() {
    let (_tmp, project_dir) = setup_temp_project();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    for idx in graph.node_indices() {
        let node = &graph[idx];
        if let Some(ref fp) = node.file_path {
            assert!(
                fp.is_relative(),
                "file_path for node '{}' should be relative but got: {}",
                node.label,
                fp.display()
            );
            assert!(
                !fp.starts_with(&project_dir),
                "file_path for node '{}' should not start with project_dir: {}",
                node.label,
                fp.display()
            );
        }
    }

    // Verify source node specifically has relative path
    let source_node = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Source)
        .expect("should have a source node");
    assert_eq!(
        graph[source_node].file_path.as_deref(),
        Some(std::path::Path::new("models/schema.yml"))
    );

    // Verify model node has relative path
    let model_node = graph
        .node_indices()
        .find(|&i| graph[i].label == "stg_orders")
        .unwrap();
    assert_eq!(
        graph[model_node].file_path.as_deref(),
        Some(std::path::Path::new("models/stg_orders.sql"))
    );
}
