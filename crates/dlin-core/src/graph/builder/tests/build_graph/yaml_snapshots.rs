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

    let graph = build_graph(&project_dir, "test_project", &files, &HashMap::new()).unwrap();
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
    let graph = build_graph(&project_dir, "test_project", &files, &project.vars).unwrap();

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

    let graph = build_graph(&project_dir, "test_project", &files, &vars).unwrap();

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

    let graph = build_graph(&project_dir, "test_project", &files, &HashMap::new()).unwrap();

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

    let graph = build_graph(&project_dir, "test_project", &files, &HashMap::new()).unwrap();

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

    let graph = build_graph(&project_dir, "test_project", &files, &HashMap::new()).unwrap();

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

    let graph = build_graph(&project_dir, "test_project", &files, &HashMap::new()).unwrap();

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

    let graph = build_graph(&project_dir, "test_project", &files, &HashMap::new()).unwrap();

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
fn var_templates_in_sql_record_dependencies_when_called() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();
    fs::write(
        project_dir.join("dbt_project.yml"),
        r#"
name: var_templates
vars:
  relation: "{{ source('raw', 'fallback') }}"
  var_templates:
    relation: "{{ source('raw', 'accounts') }}"
  other_package:
    relation: "{{ source('raw', 'fallback') }}"
  chain: "{{ var('relation') }}"
  cycle_a: "{{ var('cycle_b') }}"
  cycle_b: "{{ var('cycle_a') }}"
  invalid: "{% if %}"
  opaque: "{{ missing_macro() }}"
  filter: "{{ 'x' | missing_filter }}"
"#,
    )
    .unwrap();
    fs::write(
        models_dir.join("sources.yml"),
        r#"
version: 2
sources:
  - name: raw
    tables:
      - name: accounts
      - name: fallback
"#,
    )
    .unwrap();

    let cases = [
        (
            "direct",
            r#"select * from {{ var('relation') }}"#,
            &["raw.accounts"][..],
            &[][..],
        ),
        (
            "unused_result",
            r#"{% set ignored = var('relation') %} select 1"#,
            &["raw.accounts"][..],
            &[][..],
        ),
        ("uncalled", r#"select 1"#, &[][..], &[][..]),
        (
            "configured",
            r#"select * from {{ var('relation', "{{ source('raw', 'fallback') }}") }}"#,
            &["raw.accounts"][..],
            &[][..],
        ),
        (
            "selected_default",
            r#"select * from {{ var('missing', "{{ source('raw', 'fallback') }}") }}"#,
            &["raw.fallback"][..],
            &[][..],
        ),
        (
            "chain",
            r#"select * from {{ var('chain') }}"#,
            &["raw.accounts"][..],
            &[][..],
        ),
        (
            "partial",
            r#"select * from {{ var('chain') }}
{% set ignored = var('cycle_a') %}
{% set ignored = var('invalid') %}
{% set ignored = var('opaque') %}
{% set ignored = var('filter') %}"#,
            &["raw.accounts"][..],
            &[
                "cyclic_var_template",
                "jinja_render_failed",
                "unresolved_call",
                "unsupported_jinja_construct",
            ][..],
        ),
    ];
    for (name, sql, _, _) in cases {
        fs::write(models_dir.join(format!("{name}.sql")), sql).unwrap();
    }
    let project = crate::parser::project::DbtProject::load(project_dir).unwrap();
    let paths = project.resolve_paths(project_dir);
    let files = crate::parser::discovery::discover_files(&paths).unwrap();
    let graph = build_graph(project_dir, "var_templates", &files, &project.vars).unwrap();

    for (name, _, expected_upstream, expected_reasons) in cases {
        let consumer = graph
            .node_indices()
            .find(|&i| graph[i].label == name)
            .unwrap();
        let mut upstream: Vec<_> = graph
            .neighbors_directed(consumer, petgraph::Direction::Incoming)
            .map(|i| graph[i].label.as_str())
            .collect();
        upstream.sort_unstable();
        assert_eq!(upstream, expected_upstream, "{name}");

        let json = serde_json::to_value(&graph[consumer].dependency_analysis).unwrap();
        let status = if expected_reasons.is_empty() {
            "complete"
        } else {
            "partial"
        };
        assert_eq!(json["status"], status, "{name}");
        if !expected_reasons.is_empty() {
            let mut reasons: Vec<_> = json["unresolved"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["type"].as_str().unwrap())
                .collect();
            reasons.sort_unstable();
            assert_eq!(reasons, expected_reasons);
            let cycle = json["unresolved"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["type"] == "cyclic_var_template")
                .unwrap();
            assert_eq!(cycle["at"]["path"], "models/partial.sql");
            assert_eq!(cycle["at"]["line"], 2);
            assert_eq!(
                cycle["at"]["var_template"]["names"],
                serde_json::json!(["cycle_a", "cycle_b", "cycle_a"])
            );
            assert!(cycle["at"]["expr"].is_null());
        }
    }
}

#[test]
fn root_dispatch_respects_project_dispatch_entries() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::create_dir(root.join("models")).unwrap();
    fs::create_dir(root.join("macros")).unwrap();
    fs::write(root.join("models/upstream.sql"), r#"select 1"#).unwrap();
    fs::write(
        root.join("models/consumer.sql"),
        r#"{{ root_project.wrapper('upstream') }}"#,
    )
    .unwrap();
    fs::write(
        root.join("macros/wrapper.sql"),
        r#"
{% macro wrapper(name) %}
  {{ return(adapter.dispatch('choose', 'root_project')(name)) }}
{% endmacro %}
{% macro default__choose(name) %}
  select * from {{ ref(name) }}
{% endmacro %}
"#,
    )
    .unwrap();
    for (config, expected) in [
        ("", true),
        (
            r#"
dispatch:
  - macro_namespace: unrelated
    search_order: [unrelated, root_project]
"#,
            true,
        ),
        (
            r#"
dispatch:
  - macro_namespace: root_project
    search_order: []
"#,
            false,
        ),
        (
            r#"
dispatch:
  - macro_namespace: root_project
    search_order: [other, root_project]
"#,
            false,
        ),
        (
            r#"
dispatch:
  - macro_namespace: "{{ var('namespace') }}"
    search_order: [other]
"#,
            false,
        ),
    ] {
        fs::write(
            root.join("dbt_project.yml"),
            format!("name: root_project\n{config}"),
        )
        .unwrap();
        let project = crate::parser::project::DbtProject::load(root).unwrap();
        let files = crate::parser::discovery::discover_files(&project.resolve_paths(root)).unwrap();
        let graph = super::super::super::build_graph_report_with_macro_resolution(
            root,
            &files,
            &project.vars,
            &project.macro_resolution(),
        )
        .unwrap()
        .into_graph();
        assert_eq!(graph.edge_count(), usize::from(expected), "{config}");
        let consumer = graph
            .node_weights()
            .find(|node| node.label == "consumer")
            .unwrap();
        let report = serde_json::to_value(&consumer.dependency_analysis).unwrap();
        if expected {
            assert_eq!(report["status"], "complete");
        } else {
            assert_eq!(report["status"], "partial");
            assert_eq!(report["unresolved"][0]["type"], "unsupported_dispatch");
            assert_eq!(
                report["unresolved"][0]["at"]["expr"],
                r#"root_project.wrapper('upstream')"#
            );
        }
    }
}
