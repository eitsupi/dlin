use super::*;

#[test]
fn test_build_graph_versioned_model_creates_version_nodes() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // v1 and v2 SQL files (default naming: {name}_v{v}.sql)
    fs::write(models_dir.join("my_model_v1.sql"), "SELECT 1 as id").unwrap();
    fs::write(models_dir.join("my_model_v2.sql"), "SELECT 2 as id").unwrap();

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

    // Two version nodes: my_model.v1 and my_model.v2
    assert_eq!(graph.node_count(), 2);
    let labels: Vec<&str> = graph
        .node_indices()
        .map(|i| graph[i].label.as_str())
        .collect();
    assert!(labels.contains(&"my_model.v1"));
    assert!(labels.contains(&"my_model.v2"));

    // unique_ids should use dotted form
    let ids: Vec<&str> = graph
        .node_indices()
        .map(|i| graph[i].unique_id.as_str())
        .collect();
    assert!(ids.contains(&"model.my_model.v1"));
    assert!(ids.contains(&"model.my_model.v2"));
}

#[test]
fn test_build_graph_versioned_ref_without_version_kwarg_resolves_to_latest() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    fs::write(models_dir.join("my_model_v1.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("my_model_v2.sql"), "SELECT 2").unwrap();
    // downstream refs 'my_model' without version → should go to v2 (latest)
    fs::write(
        models_dir.join("downstream.sql"),
        "SELECT * FROM {{ ref('my_model') }}",
    )
    .unwrap();

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
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model_v1.sql"),
            project_dir.join("models/my_model_v2.sql"),
            project_dir.join("models/downstream.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 3 nodes (v1, v2, downstream) — no phantom
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 1);

    // The single edge should be my_model.v2 → downstream
    let downstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "downstream")
        .unwrap();
    let v2_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v2")
        .unwrap();
    assert!(graph.contains_edge(v2_idx, downstream_idx));
}

#[test]
fn test_build_graph_versioned_ref_with_version_kwarg_resolves_to_correct_version() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    fs::write(models_dir.join("my_model_v1.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("my_model_v2.sql"), "SELECT 2").unwrap();
    // downstream explicitly refs version=1
    fs::write(
        models_dir.join("downstream.sql"),
        "SELECT * FROM {{ ref('my_model', version=1) }}",
    )
    .unwrap();

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
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model_v1.sql"),
            project_dir.join("models/my_model_v2.sql"),
            project_dir.join("models/downstream.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 3 nodes (v1, v2, downstream) — no phantom
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 1);

    // The single edge should be my_model.v1 → downstream
    let downstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "downstream")
        .unwrap();
    let v1_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v1")
        .unwrap();
    assert!(graph.contains_edge(v1_idx, downstream_idx));
}

#[test]
fn test_build_graph_versioned_model_sql_edges_processed() {
    // Regression: versioned SQL files must have their ref()/source() edges processed.
    // Earlier, process_sql_edges looked up "model.my_model_v1" (no dot) but nodes
    // were registered as "model.my_model.v1" (with dot), causing all edges to be skipped.
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // v1 refs a source; v2 refs v1
    fs::write(
        models_dir.join("my_model_v1.sql"),
        "SELECT * FROM {{ source('raw', 'events') }}",
    )
    .unwrap();
    fs::write(
        models_dir.join("my_model_v2.sql"),
        "SELECT * FROM {{ ref('my_model', version=1) }}",
    )
    .unwrap();

    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
sources:
  - name: raw
    tables:
      - name: events
models:
  - name: my_model
    latest_version: 2
    versions:
      - v: 1
      - v: 2
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

    // source + v1 + v2 = 3 nodes, no phantoms
    assert_eq!(graph.node_count(), 3);
    // source→v1 and v1→v2 = 2 edges
    assert_eq!(graph.edge_count(), 2);

    let v1_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v1")
        .expect("v1 node must exist");
    let v2_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v2")
        .expect("v2 node must exist");
    let src_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "source.raw.events")
        .expect("source node must exist");

    assert!(
        graph.contains_edge(src_idx, v1_idx),
        "source→v1 edge missing"
    );
    assert!(graph.contains_edge(v1_idx, v2_idx), "v1→v2 edge missing");
}

#[test]
fn test_build_graph_versioned_phantom_for_missing_version() {
    // Regression: ref('name', version=N) when version N does not exist must
    // create a phantom model.name.vN, not link to the latest-version alias.
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // Only v2 exists; downstream refs version=1 which is absent
    fs::write(models_dir.join("my_model_v2.sql"), "SELECT 2").unwrap();
    fs::write(
        models_dir.join("downstream.sql"),
        "SELECT * FROM {{ ref('my_model', version=1) }}",
    )
    .unwrap();

    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
models:
  - name: my_model
    latest_version: 2
    versions:
      - v: 2
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model_v2.sql"),
            project_dir.join("models/downstream.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // v2 + downstream + phantom(v1) = 3 nodes
    assert_eq!(graph.node_count(), 3);

    let phantom = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Phantom)
        .expect("phantom v1 node must exist");
    assert_eq!(graph[phantom].unique_id, "model.my_model.v1");

    // Edge must be phantom_v1 → downstream, NOT v2 → downstream
    let downstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "downstream")
        .unwrap();
    assert!(
        graph.contains_edge(phantom, downstream_idx),
        "phantom.v1 → downstream edge missing"
    );
    let v2_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v2")
        .unwrap();
    assert!(
        !graph.contains_edge(v2_idx, downstream_idx),
        "v2 → downstream edge must not exist"
    );
}

#[test]
fn test_build_graph_versioned_model_custom_defined_in() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // v1 uses default stem, v2 uses custom stem via defined_in
    fs::write(models_dir.join("my_model_v1.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("custom_v2_file.sql"), "SELECT 2").unwrap();

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
        defined_in: custom_v2_file
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model_v1.sql"),
            project_dir.join("models/custom_v2_file.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    assert_eq!(graph.node_count(), 2);
    let ids: Vec<&str> = graph
        .node_indices()
        .map(|i| graph[i].unique_id.as_str())
        .collect();
    assert!(ids.contains(&"model.my_model.v1"));
    assert!(ids.contains(&"model.my_model.v2"));
}

#[test]
fn test_build_graph_versioned_defined_in_base_name_edges_correct_version() {
    // Regression: when defined_in equals the base model name (e.g. v1 lives in
    // my_model.sql), process_sql_edges must attach edges to the v1 node, not to
    // the latest-version alias (model.my_model → v2).
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // v1 is in my_model.sql (defined_in: my_model), v2 is in my_model_v2.sql
    fs::write(
        models_dir.join("my_model.sql"),
        "SELECT * FROM {{ source('raw', 'events') }}",
    )
    .unwrap();
    fs::write(models_dir.join("my_model_v2.sql"), "SELECT 2").unwrap();

    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
sources:
  - name: raw
    tables:
      - name: events
models:
  - name: my_model
    latest_version: 2
    versions:
      - v: 1
        defined_in: my_model
      - v: 2
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model.sql"),
            project_dir.join("models/my_model_v2.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // source + v1 + v2 = 3 nodes, no phantoms
    assert_eq!(graph.node_count(), 3);
    assert_eq!(graph.edge_count(), 1);

    let v1_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v1")
        .expect("v1 node must exist");
    let src_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "source.raw.events")
        .expect("source node must exist");
    let v2_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.my_model.v2")
        .expect("v2 node must exist");

    // Edge must be source → v1 (not source → v2)
    assert!(
        graph.contains_edge(src_idx, v1_idx),
        "source→v1 edge missing: edges in my_model.sql must attach to v1"
    );
    assert!(
        !graph.contains_edge(src_idx, v2_idx),
        "source→v2 edge must not exist"
    );
}

#[test]
fn test_build_graph_unversioned_ref_resolves_to_versioned_phantom_when_sql_missing() {
    // When the latest_version SQL file is absent, ref('name') (no version kwarg)
    // should resolve to a versioned phantom (model.name.vN), not an unversioned
    // phantom (model.name).
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // Only v1 SQL exists; latest_version=2 is declared but its SQL is absent.
    fs::write(models_dir.join("my_model_v1.sql"), "SELECT 1").unwrap();
    fs::write(
        models_dir.join("downstream.sql"),
        "SELECT * FROM {{ ref('my_model') }}",
    )
    .unwrap();

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
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/my_model_v1.sql"),
            project_dir.join("models/downstream.sql"),
        ],
        yaml_files: vec![project_dir.join("models/schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // v1 + downstream + phantom(v2) = 3 nodes
    assert_eq!(graph.node_count(), 3);

    let phantom = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Phantom)
        .expect("phantom v2 node must exist");
    // Phantom must be the versioned ID, not the unversioned fallback
    assert_eq!(graph[phantom].unique_id, "model.my_model.v2");

    let downstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "downstream")
        .unwrap();
    assert!(
        graph.contains_edge(phantom, downstream_idx),
        "phantom.v2 → downstream edge missing"
    );
}

#[test]
fn test_stem_to_versioned_no_duplicate_overwrite() {
    // When two YAML files define the same model, both stem_to_versioned and
    // version_aliases must use first-file-wins (entry().or_insert()) semantics.
    // Here schema_a (latest_version=2) is processed before schema_b (latest_version=1),
    // so an unversioned ref('orders') must resolve to v2, not v1.
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();

    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // Same versioned model declared in two YAML files
    let yaml_a = r#"
models:
  - name: orders
    versions:
      - v: 1
      - v: 2
    latest_version: 2
"#;
    let yaml_b = r#"
models:
  - name: orders
    versions:
      - v: 1
      - v: 2
    latest_version: 1
"#;
    // SQL stubs so the model files exist
    fs::write(models_dir.join("orders_v1.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("orders_v2.sql"), "SELECT 2").unwrap();
    // Downstream uses unversioned ref
    fs::write(
        models_dir.join("downstream.sql"),
        "SELECT * FROM {{ ref('orders') }}",
    )
    .unwrap();

    fs::write(models_dir.join("schema_a.yml"), yaml_a).unwrap();
    fs::write(models_dir.join("schema_b.yml"), yaml_b).unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/orders_v1.sql"),
            project_dir.join("models/orders_v2.sql"),
            project_dir.join("models/downstream.sql"),
        ],
        yaml_files: vec![
            project_dir.join("models/schema_a.yml"),
            project_dir.join("models/schema_b.yml"),
        ],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // Two versioned nodes + downstream = 3
    assert_eq!(
        graph
            .node_indices()
            .filter(|&i| matches!(graph[i].node_type, NodeType::Model))
            .count(),
        3
    );

    // The unversioned ref('orders') must resolve to v2 (from schema_a, first file)
    let downstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "downstream")
        .unwrap();
    let v2_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.orders.v2")
        .unwrap();
    let v1_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.orders.v1")
        .unwrap();
    assert!(
        graph.contains_edge(v2_idx, downstream_idx),
        "unversioned ref should resolve to v2 (first file's latest_version)"
    );
    assert!(
        !graph.contains_edge(v1_idx, downstream_idx),
        "ref must not resolve to v1 (second file's latest_version)"
    );
}
