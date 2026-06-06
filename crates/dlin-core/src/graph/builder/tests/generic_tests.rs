use super::*;

#[test]
fn test_generic_tests_from_yaml() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    fs::write(project_dir.join("dbt_project.yml"), "name: test_proj\n").unwrap();
    fs::write(models_dir.join("orders.sql"), "SELECT 1 AS order_id").unwrap();

    // Schema with generic tests on columns
    fs::write(
        models_dir.join("schema.yml"),
        r#"
sources:
  - name: raw
    tables:
      - name: events
        columns:
          - name: event_id
            data_tests:
              - not_null
models:
  - name: orders
    data_tests:
      - dbt_utils.expression_is_true:
          expression: "a = b"
      - dbt_utils.expression_is_true:
          expression: "c = d"
    columns:
      - name: order_id
        data_tests:
          - not_null
          - unique
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/orders.sql")],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 1 model + 1 source + 2 column tests + 1 source test + 2 model-level tests = 7
    assert_eq!(graph.node_count(), 7);

    let test_nodes: Vec<_> = graph
        .node_indices()
        .filter(|&i| graph[i].node_type == NodeType::Test)
        .collect();
    assert_eq!(test_nodes.len(), 5);

    // Verify test unique_ids
    let mut test_ids: Vec<&str> = test_nodes
        .iter()
        .map(|&i| graph[i].unique_id.as_str())
        .collect();
    test_ids.sort();
    assert!(test_ids.contains(&"test.not_null.orders.order_id"));
    assert!(test_ids.contains(&"test.unique.orders.order_id"));
    assert!(test_ids.contains(&"test.not_null.raw.events.event_id"));
    // Model-level tests: first gets base ID, second gets _2 suffix
    assert!(test_ids.contains(&"test.dbt_utils.expression_is_true.orders"));
    assert!(test_ids.contains(&"test.dbt_utils.expression_is_true.orders_2"));

    // All test edges from model (2 column + 2 model-level = 4)
    let model_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.orders")
        .unwrap();
    let source_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "source.raw.events")
        .unwrap();

    let model_test_edges = graph
        .edges_directed(model_idx, petgraph::Direction::Outgoing)
        .filter(|e| e.weight().edge_type == EdgeType::Test)
        .count();
    assert_eq!(model_test_edges, 4);

    let source_test_edges = graph
        .edges_directed(source_idx, petgraph::Direction::Outgoing)
        .filter(|e| e.weight().edge_type == EdgeType::Test)
        .count();
    assert_eq!(source_test_edges, 1);

    // All generic test nodes should have file_path pointing to the YAML file
    for &ti in &test_nodes {
        assert_eq!(
            graph[ti].file_path.as_deref(),
            Some(std::path::Path::new("models/schema.yml")),
            "test node '{}' should have file_path",
            graph[ti].unique_id,
        );
    }

    // Deduped test labels must also be distinct (suffix applied to label)
    let mut test_labels: Vec<&str> = test_nodes
        .iter()
        .map(|&i| graph[i].label.as_str())
        .collect();
    test_labels.sort();
    let deduped_len = test_labels.len();
    test_labels.dedup();
    assert_eq!(
        test_labels.len(),
        deduped_len,
        "All test labels should be unique"
    );
    // Verify the deduped model-level test labels
    assert!(test_labels.contains(&"dbt_utils.expression_is_true_orders"));
    assert!(test_labels.contains(&"dbt_utils.expression_is_true_orders_2"));
}

#[test]
fn test_generic_test_ids_deterministic_across_yaml_order() {
    // Duplicate test names across two YAML files should produce the same
    // suffixed IDs regardless of the order the files are passed in.
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    let sub_dir = models_dir.join("sub");
    fs::create_dir_all(&sub_dir).unwrap();

    fs::write(models_dir.join("orders.sql"), "SELECT 1 AS order_id").unwrap();

    // Two YAML files that both declare a not_null test on orders.order_id
    let yaml_a = models_dir.join("a_schema.yml");
    let yaml_b = sub_dir.join("b_schema.yml");
    let yaml_content = r#"
models:
  - name: orders
    columns:
      - name: order_id
        data_tests:
          - not_null
"#;
    fs::write(&yaml_a, yaml_content).unwrap();
    fs::write(&yaml_b, yaml_content).unwrap();

    // Build with files in forward order
    let files_fwd = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/orders.sql")],
        yaml_files: vec![yaml_a.clone(), yaml_b.clone()],
        ..Default::default()
    };
    let graph_fwd =
        build_graph(&project_dir, &files_fwd, None, true, false, &HashMap::new()).unwrap();

    // Build with files in reverse order
    let files_rev = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/orders.sql")],
        yaml_files: vec![yaml_b, yaml_a],
        ..Default::default()
    };
    let graph_rev =
        build_graph(&project_dir, &files_rev, None, true, false, &HashMap::new()).unwrap();

    // Both should produce the same set of test unique_ids
    let mut ids_fwd: Vec<String> = graph_fwd
        .node_indices()
        .filter(|&i| graph_fwd[i].node_type == NodeType::Test)
        .map(|i| graph_fwd[i].unique_id.clone())
        .collect();
    ids_fwd.sort();

    let mut ids_rev: Vec<String> = graph_rev
        .node_indices()
        .filter(|&i| graph_rev[i].node_type == NodeType::Test)
        .map(|i| graph_rev[i].unique_id.clone())
        .collect();
    ids_rev.sort();

    assert_eq!(ids_fwd, ids_rev);
    assert_eq!(ids_fwd.len(), 2);
    assert!(ids_fwd.contains(&"test.not_null.orders.order_id".to_string()));
    assert!(ids_fwd.contains(&"test.not_null.orders.order_id_2".to_string()));
}
