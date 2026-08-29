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
