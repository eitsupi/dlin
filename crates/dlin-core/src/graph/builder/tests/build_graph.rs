use super::*;
use std::collections::HashMap;

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

#[test]
fn test_build_graph_semantic_layer_full() {
    let (_tmp, project_dir) = setup_temp_project();

    // Add a semantic layer YAML alongside the existing schema.yml
    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("semantic.yml"),
        r#"
semantic_models:
  - name: orders
    description: Order semantic model
    model: ref('orders')
    measures:
      - name: order_count
      - name: order_total

metrics:
  - name: orders
    type: simple
    type_params:
      measure: order_count
  - name: order_total
    type: simple
    type_params:
      measure: order_total
  - name: revenue_per_order
    type: ratio
    type_params:
      numerator: order_total
      denominator: orders

saved_queries:
  - name: order_kpis
    description: Key order KPIs
    query_params:
      metrics:
        - orders
        - order_total
        - revenue_per_order
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![
            project_dir.join("models/schema.yml"),
            project_dir.join("models/semantic.yml"),
        ],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    // Node type counts
    let counts: HashMap<NodeType, usize> =
        graph.node_indices().fold(HashMap::new(), |mut acc, i| {
            *acc.entry(graph[i].node_type).or_insert(0) += 1;
            acc
        });

    // 1 source + 2 models + 1 semantic_model + 3 metrics + 1 saved_query = 8
    assert_eq!(*counts.get(&NodeType::Source).unwrap_or(&0), 1, "sources");
    assert_eq!(*counts.get(&NodeType::Model).unwrap_or(&0), 2, "models");
    assert_eq!(
        *counts.get(&NodeType::SemanticModel).unwrap_or(&0),
        1,
        "semantic_models"
    );
    assert_eq!(*counts.get(&NodeType::Metric).unwrap_or(&0), 3, "metrics");
    assert_eq!(
        *counts.get(&NodeType::SavedQuery).unwrap_or(&0),
        1,
        "saved_queries"
    );

    // Verify semantic_model.orders exists and is linked to model.orders
    let sem_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "semantic_model.orders")
        .expect("semantic_model.orders not found");
    let model_orders_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "model.orders")
        .expect("model.orders not found");
    assert!(
        graph.contains_edge(model_orders_idx, sem_idx),
        "model.orders → semantic_model.orders edge missing"
    );

    // Verify metric.orders is linked to semantic_model.orders
    let metric_orders_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.orders")
        .expect("metric.orders not found");
    assert!(
        graph.contains_edge(sem_idx, metric_orders_idx),
        "semantic_model.orders → metric.orders edge missing"
    );

    // Verify revenue_per_order depends on order_total and orders metrics
    let ratio_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.revenue_per_order")
        .expect("metric.revenue_per_order not found");
    let metric_total_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.order_total")
        .expect("metric.order_total not found");
    assert!(
        graph.contains_edge(metric_total_idx, ratio_idx),
        "metric.order_total → metric.revenue_per_order edge missing"
    );
    assert!(
        graph.contains_edge(metric_orders_idx, ratio_idx),
        "metric.orders → metric.revenue_per_order edge missing"
    );

    // Verify saved_query.order_kpis is linked to all 3 metrics
    let sq_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "saved_query.order_kpis")
        .expect("saved_query.order_kpis not found");
    assert!(
        graph.contains_edge(metric_orders_idx, sq_idx),
        "metric.orders → saved_query.order_kpis edge missing"
    );
    assert!(
        graph.contains_edge(metric_total_idx, sq_idx),
        "metric.order_total → saved_query.order_kpis edge missing"
    );
    assert!(
        graph.contains_edge(ratio_idx, sq_idx),
        "metric.revenue_per_order → saved_query.order_kpis edge missing"
    );
}

#[test]
fn test_build_graph_semantic_layer_metric_reference_shapes() {
    let (_tmp, project_dir) = setup_temp_project();

    let models_dir = project_dir.join("models");
    fs::write(
        models_dir.join("semantic_refs.yml"),
        r#"
semantic_models:
  - name: orders
    model: ref('orders')
    measures:
      - name: order_total
      - name: order_count
      - name: customer_count

metrics:
  - name: order_total
    type: simple
    type_params:
      measure:
        name: order_total
        fill_nulls_with: 0
  - name: orders
    type: simple
    type_params:
      measure: order_count
  - name: customers
    type: simple
    type_params:
      measure: customer_count
  - name: derived_kpi
    type: derived
    type_params:
      metrics:
        - name: order_total
        - orders
        - customers
"#,
    )
    .unwrap();

    let files = DiscoveredFiles {
        model_sql_files: vec![
            project_dir.join("models/stg_orders.sql"),
            project_dir.join("models/orders.sql"),
        ],
        yaml_files: vec![
            project_dir.join("models/schema.yml"),
            project_dir.join("models/semantic_refs.yml"),
        ],
        ..Default::default()
    };

    let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

    let sem_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "semantic_model.orders")
        .expect("semantic_model.orders not found");
    let metric_total_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.order_total")
        .expect("metric.order_total not found");
    assert!(
        graph.contains_edge(sem_idx, metric_total_idx),
        "semantic_model.orders → metric.order_total edge missing"
    );

    let derived_idx = graph
        .node_indices()
        .find(|&i| graph[i].unique_id == "metric.derived_kpi")
        .expect("metric.derived_kpi not found");
    for metric_id in ["metric.order_total", "metric.orders", "metric.customers"] {
        let metric_idx = graph
            .node_indices()
            .find(|&i| graph[i].unique_id == metric_id)
            .unwrap_or_else(|| panic!("{metric_id} not found"));
        assert!(
            graph.contains_edge(metric_idx, derived_idx),
            "{metric_id} → metric.derived_kpi edge missing"
        );
    }
}
