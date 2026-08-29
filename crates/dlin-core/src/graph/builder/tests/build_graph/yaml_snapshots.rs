use super::*;
use std::fs;

#[test]
fn test_build_graph_with_macros() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();

    let models_dir = project_dir.join("models");
    let macros_dir = project_dir.join("macros");
    fs::create_dir_all(&models_dir).unwrap();
    fs::create_dir_all(&macros_dir).unwrap();

    // Macro that references a model
    fs::write(
        macros_dir.join("my_macro.sql"),
        r#"
{% macro my_cte() %}
    SELECT * FROM {{ ref('base_table') }}
{% endmacro %}
"#,
    )
    .unwrap();

    // Model that uses the macro
    fs::write(models_dir.join("base_table.sql"), "SELECT 1 as id").unwrap();
    fs::write(
        models_dir.join("derived.sql"),
        "SELECT * FROM ({{ my_cte() }})",
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/base_table.sql"),
            project_dir.join("models/derived.sql"),
        ],
        macro_sql_files: vec![project_dir.join("macros/my_macro.sql")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
    // base_table + derived = 2 nodes
    assert_eq!(graph.node_count(), 2);
    // ref edge: base_table → derived
    assert_eq!(graph.edge_count(), 1);

    // Verify the edge direction
    let base = graph
        .node_indices()
        .find(|&i| graph[i].label == "base_table")
        .unwrap();
    let derived = graph
        .node_indices()
        .find(|&i| graph[i].label == "derived")
        .unwrap();
    assert!(graph.contains_edge(base, derived));
}

#[test]
fn test_vars_yml_context_is_shared_by_macros_and_models() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    let macros_dir = project_dir.join("macros");
    fs::create_dir_all(&models_dir).unwrap();
    fs::create_dir_all(&macros_dir).unwrap();

    fs::write(project_dir.join("dbt_project.yml"), "name: vars_test\n").unwrap();
    fs::write(project_dir.join("vars.yml"), "vars:\n  suffix: file\n").unwrap();
    fs::write(
        macros_dir.join("dynamic_ref.sql"),
        r#"
{% macro dynamic_ref() %}
  {{ ref('macro_' ~ var('suffix')) }}
{% endmacro %}
"#,
    )
    .unwrap();
    fs::write(models_dir.join("macro_file.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("model_file.sql"), "SELECT 1").unwrap();
    fs::write(
        models_dir.join("downstream.sql"),
        "SELECT * FROM {{ dynamic_ref() }} JOIN {{ ref('model_' ~ var('suffix')) }} USING (id)",
    )
    .unwrap();

    let project = crate::parser::project::DbtProject::load(&project_dir).unwrap();
    let paths = project.resolve_paths(&project_dir);
    let files = crate::parser::discovery::discover_files(&paths).unwrap();
    let graph = build_graph(&project_dir, &files, None, true, false, &project.vars).unwrap();

    let downstream = graph
        .node_indices()
        .find(|&i| graph[i].label == "downstream")
        .unwrap();
    let upstream_labels: std::collections::HashSet<_> = graph
        .neighbors_directed(downstream, petgraph::Direction::Incoming)
        .map(|i| graph[i].label.as_str())
        .collect();
    assert_eq!(
        upstream_labels,
        std::collections::HashSet::from(["macro_file", "model_file"])
    );
}

#[test]
fn test_var_list_expansion_resolves_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    // dbt_project.yml with vars
    fs::write(project_dir.join("dbt_project.yml"), "name: var_test\n").unwrap();

    // Model that uses var() to iterate over categories and ref dynamically
    fs::write(
        models_dir.join("combined.sql"),
        r#"
            {%- set categories = var("product_categories") -%}
            {%- for cat in categories -%}
                SELECT * FROM {{ ref('stg_' ~ cat ~ '_summary') }}
                {% if not loop.last %}UNION ALL{% endif %}
            {%- endfor -%}
            "#,
    )
    .unwrap();

    // Stub models that the refs point to
    fs::write(models_dir.join("stg_electronics_summary.sql"), "SELECT 1").unwrap();
    fs::write(models_dir.join("stg_clothing_summary.sql"), "SELECT 1").unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/combined.sql"),
            project_dir.join("models/stg_electronics_summary.sql"),
            project_dir.join("models/stg_clothing_summary.sql"),
        ],
        ..Default::default()
    };

    // Provide project-level vars
    let mut vars = HashMap::new();
    vars.insert(
        "product_categories".to_string(),
        serde_json::json!(["electronics", "clothing"]),
    );

    let graph = build_graph(&project_dir, &files, None, true, false, &vars).unwrap();

    // 3 model nodes: combined + stg_electronics_summary + stg_clothing_summary
    assert_eq!(graph.node_count(), 3);

    // 2 edges: stg_electronics_summary → combined, stg_clothing_summary → combined
    assert_eq!(graph.edge_count(), 2);

    let combined = graph
        .node_indices()
        .find(|&i| graph[i].label == "combined")
        .unwrap();
    let electronics = graph
        .node_indices()
        .find(|&i| graph[i].label == "stg_electronics_summary")
        .unwrap();
    let clothing = graph
        .node_indices()
        .find(|&i| graph[i].label == "stg_clothing_summary")
        .unwrap();
    assert!(graph.contains_edge(electronics, combined));
    assert!(graph.contains_edge(clothing, combined));
}

#[test]
fn test_build_graph_yaml_only_snapshot() {
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    // Use a simple SQL file with no source refs to keep node count predictable
    fs::write(models_dir.join("stg_orders.sql"), "SELECT 1").unwrap();
    fs::write(
        models_dir.join("snap_schema.yml"),
        r#"
version: 2
snapshots:
  - name: snap_orders
    description: Orders snapshot
    relation: ref('stg_orders')
  - name: snap_no_relation
    description: Snapshot without upstream
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
        yaml_files: vec![project_dir.join("models/snap_schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 1 model + 2 snapshots = 3 nodes
    assert_eq!(graph.node_count(), 3);

    let snap_orders_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "snap_orders")
        .unwrap();
    let snap_no_rel_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "snap_no_relation")
        .unwrap();
    let model_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "stg_orders")
        .unwrap();

    assert_eq!(graph[snap_orders_idx].node_type, NodeType::Snapshot);
    assert_eq!(
        graph[snap_orders_idx].description.as_deref(),
        Some("Orders snapshot")
    );
    assert_eq!(graph[snap_no_rel_idx].node_type, NodeType::Snapshot);

    // snap_orders gets an edge from the upstream model via relation
    assert_eq!(graph.edge_count(), 1);
    assert!(graph.contains_edge(model_idx, snap_orders_idx));
    // snap_no_relation has no edge
    assert!(!graph.contains_edge(model_idx, snap_no_rel_idx));
}

#[test]
fn test_build_graph_yaml_snapshot_sql_takes_precedence() {
    // When both a SQL file and YAML definition exist for the same snapshot name,
    // the SQL file registers the node; the YAML definition is skipped (no duplicate).
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    // Use a simple SQL file with no source refs to keep node count predictable
    fs::write(models_dir.join("stg_orders.sql"), "SELECT 1").unwrap();

    let snap_dir = project_dir.join("snapshots");
    fs::create_dir_all(&snap_dir).unwrap();
    fs::write(
        snap_dir.join("snap_orders.sql"),
        "SELECT * FROM {{ ref('stg_orders') }}",
    )
    .unwrap();
    fs::write(
        snap_dir.join("snap_schema.yml"),
        r#"
version: 2
snapshots:
  - name: snap_orders
    relation: ref('stg_orders')
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
        snapshot_sql_files: vec![project_dir.join("snapshots/snap_orders.sql")],
        yaml_files: vec![project_dir.join("snapshots/snap_schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 1 model + 1 snapshot = 2 nodes (no duplicate snapshot node)
    let snap_count = graph
        .node_indices()
        .filter(|&i| graph[i].node_type == NodeType::Snapshot)
        .count();
    assert_eq!(snap_count, 1);

    // Edge comes from SQL file's ref() call
    assert_eq!(graph.edge_count(), 1);
}

#[test]
fn test_build_graph_yaml_only_snapshot_source_relation() {
    // YAML-only snapshot with relation: source('schema', 'table') should create
    // a Source edge to the matching source node (or a phantom if undefined).
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("snap_schema.yml"),
        r#"
version: 2
sources:
  - name: raw
    tables:
      - name: orders
snapshots:
  - name: snap_raw_orders
    description: Raw orders snapshot
    relation: source('raw', 'orders')
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        yaml_files: vec![project_dir.join("models/snap_schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 1 source + 1 snapshot = 2 nodes
    assert_eq!(graph.node_count(), 2);

    let source_idx = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Source)
        .unwrap();
    let snap_idx = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Snapshot)
        .unwrap();

    assert_eq!(graph[snap_idx].label, "snap_raw_orders");
    assert_eq!(
        graph[snap_idx].description.as_deref(),
        Some("Raw orders snapshot")
    );

    // Source edge: source.raw.orders → snap_raw_orders
    assert_eq!(graph.edge_count(), 1);
    assert!(graph.contains_edge(source_idx, snap_idx));

    use petgraph::visit::IntoEdgeReferences;
    let edge = graph.edge_references().next().unwrap();
    assert_eq!(edge.weight().edge_type, EdgeType::Source);
}

#[test]
fn test_build_graph_yaml_only_snapshot_phantom_source_relation() {
    // When source('schema', 'table') references an undefined source, a phantom
    // node is created and a Source edge is still added.
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("snap_schema.yml"),
        r#"
version: 2
snapshots:
  - name: snap_unknown
    relation: source('undefined_schema', 'undefined_table')
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        yaml_files: vec![project_dir.join("models/snap_schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 1 phantom source + 1 snapshot = 2 nodes
    assert_eq!(graph.node_count(), 2);

    let phantom_idx = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Phantom)
        .unwrap();
    let snap_idx = graph
        .node_indices()
        .find(|&i| graph[i].node_type == NodeType::Snapshot)
        .unwrap();

    assert_eq!(graph[phantom_idx].label, "undefined_schema.undefined_table");
    assert_eq!(graph.edge_count(), 1);
    assert!(graph.contains_edge(phantom_idx, snap_idx));
}

#[test]
fn test_build_graph_yaml_only_snapshot_ref_forward_declaration() {
    // A YAML-only snapshot whose relation: ref(...) points to another YAML-only
    // snapshot declared *later* in the same file must resolve to snapshot.<name>,
    // not create a phantom model.<name> node.
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("snap_schema.yml"),
        r#"
version: 2
snapshots:
  - name: snap_downstream
    description: References snap_upstream which is declared after this
    relation: ref('snap_upstream')
  - name: snap_upstream
    description: The upstream snapshot
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        yaml_files: vec![project_dir.join("models/snap_schema.yml")],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // 2 snapshots, no phantom nodes
    assert_eq!(graph.node_count(), 2);
    assert!(
        graph
            .node_indices()
            .all(|i| graph[i].node_type == NodeType::Snapshot)
    );

    let upstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "snap_upstream")
        .unwrap();
    let downstream_idx = graph
        .node_indices()
        .find(|&i| graph[i].label == "snap_downstream")
        .unwrap();

    assert_eq!(graph.edge_count(), 1);
    assert!(graph.contains_edge(upstream_idx, downstream_idx));
}
