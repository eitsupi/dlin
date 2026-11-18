use super::*;
use crate::parser::discovery::discover_files;
use crate::parser::project::DbtProject;
use serde_json::json;

/// Write a project named `p`, discover it as the CLI does, and build it.
fn build_project(files: &[(&str, &str)]) -> (tempfile::TempDir, LineageGraph) {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("dbt_project.yml"), "name: p\n").unwrap();
    for (path, content) in files {
        let path = tmp.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
    let project = DbtProject::load(tmp.path()).unwrap();
    let discovered = discover_files(&project.resolve_paths(tmp.path())).unwrap();
    let graph = build_graph(tmp.path(), &project.name, &discovered, &project.vars).unwrap();
    (tmp, graph)
}

fn node<'a>(graph: &'a LineageGraph, unique_id: &str) -> &'a NodeData {
    graph
        .node_indices()
        .map(|idx| &graph[idx])
        .find(|node| node.unique_id == unique_id)
        .unwrap_or_else(|| panic!("missing node {unique_id}"))
}

/// The node's dependency analysis as it is serialised.
fn analysis(graph: &LineageGraph, unique_id: &str) -> serde_json::Value {
    serde_json::to_value(&node(graph, unique_id).dependency_analysis).unwrap()
}

fn complete() -> serde_json::Value {
    json!({"status": "complete", "unresolved": []})
}

fn has_edge(graph: &LineageGraph, from: &str, to: &str) -> bool {
    let find = |id: &str| {
        graph
            .node_indices()
            .find(|&idx| graph[idx].unique_id == id)
            .unwrap_or_else(|| panic!("missing node {id}"))
    };
    graph.contains_edge(find(from), find(to))
}

#[test]
fn resolved_dependencies_are_complete() {
    let (_tmp, graph) = build_project(&[
        ("models/a.sql", "select 1"),
        (
            "models/b.sql",
            r#"select * from {{ ref('a') }} join {{ ref('p', 'a') }}"#,
        ),
        ("seeds/s.csv", "id\n1\n"),
        (
            "models/sources.yml",
            r#"
version: 2
sources:
  - name: raw
    tables:
      - name: t
"#,
        ),
        ("models/c.sql", r#"select * from {{ source('raw', 't') }}"#),
    ]);
    for unique_id in ["model.a", "model.b", "model.c", "seed.s", "source.raw.t"] {
        assert_eq!(analysis(&graph, unique_id), complete(), "{unique_id}");
    }
}

#[test]
fn extraction_reasons_are_located_in_the_node() {
    let (_tmp, graph) = build_project(&[
        ("models/stg_kind_x.sql", "select 1"),
        (
            "models/b.sql",
            r#"select 1
{{ ref('stg_kind_' ~ var('kind')) }}
"#,
        ),
    ]);
    assert_eq!(
        analysis(&graph, "model.b"),
        json!({
            "status": "partial",
            "unresolved": [{
                "type": "unresolved_var",
                "name": "kind",
                "at": {
                    "unique_id": "model.b",
                    "path": "models/b.sql",
                    "line": 2,
                    "expr": "var('kind')"
                }
            }]
        })
    );
    // A name computed from an unknown value is not a phantom.
    assert!(
        graph
            .node_indices()
            .all(|idx| graph[idx].node_type != NodeType::Phantom)
    );
}

#[test]
fn every_node_referencing_a_missing_target_has_a_reason() {
    let (_tmp, graph) = build_project(&[
        ("models/a.sql", r#"select * from {{ ref('missing') }}"#),
        (
            "models/b.sql",
            r#"select 1
union all select * from {{ ref('missing') }}
union all select * from {{ source('raw', 'missing') }}"#,
        ),
    ]);
    assert_eq!(
        analysis(&graph, "model.a")["unresolved"],
        json!([{
            "type": "unresolved_ref",
            "name": "missing",
            "package": null,
            "version": null,
            "at": {
                "unique_id": "model.a",
                "path": "models/a.sql",
                "line": 1,
                "expr": "ref('missing')"
            }
        }])
    );
    assert_eq!(
        analysis(&graph, "model.b")["unresolved"],
        json!([
            {
                "type": "unresolved_ref",
                "name": "missing",
                "package": null,
                "version": null,
                "at": {
                    "unique_id": "model.b",
                    "path": "models/b.sql",
                    "line": 2,
                    "expr": "ref('missing')"
                }
            },
            {
                "type": "unresolved_source",
                "source": "raw",
                "table": "missing",
                "at": {
                    "unique_id": "model.b",
                    "path": "models/b.sql",
                    "line": 3,
                    "expr": "source('raw', 'missing')"
                }
            }
        ])
    );
    let phantom = json!({
        "status": "partial",
        "unresolved": [{"type": "not_assessed", "scope": "phantom_dependencies"}]
    });
    assert_eq!(analysis(&graph, "model.missing"), phantom);
    assert_eq!(analysis(&graph, "source.raw.missing"), phantom);
    // The edges to the phantoms are kept.
    assert!(has_edge(&graph, "model.missing", "model.a"));
    assert!(has_edge(&graph, "model.missing", "model.b"));
}

#[test]
fn ref_naming_another_package_keeps_its_edge_with_a_reason() {
    let (_tmp, graph) = build_project(&[
        ("models/a.sql", "select 1"),
        (
            "models/b.sql",
            r#"select * from {{ ref('other_package', 'a') }}"#,
        ),
    ]);
    assert!(has_edge(&graph, "model.a", "model.b"));
    assert_eq!(
        analysis(&graph, "model.b")["unresolved"],
        json!([{
            "type": "unsupported_ref_package",
            "package": "other_package",
            "name": "a",
            "at": {
                "unique_id": "model.b",
                "path": "models/b.sql",
                "line": 1,
                "expr": "ref('other_package', 'a')"
            }
        }])
    );
}

#[test]
fn ref_matching_several_resources_is_ambiguous() {
    let (_tmp, graph) = build_project(&[
        ("models/a.sql", "select 1"),
        ("seeds/a.csv", "id\n1\n"),
        ("models/b.sql", r#"select * from {{ ref('a') }}"#),
    ]);
    // The edge follows the same preference as before: the model.
    assert!(has_edge(&graph, "model.a", "model.b"));
    let unresolved = &analysis(&graph, "model.b")["unresolved"];
    assert_eq!(unresolved[0]["type"], "ambiguous_ref");
    assert_eq!(unresolved[0]["candidates"], json!(["model.a", "seed.a"]));
}

#[test]
fn exposure_dependencies_are_resolved_or_reported() {
    let (_tmp, graph) = build_project(&[
        ("models/a.sql", "select 1"),
        (
            "models/exposures.yml",
            r#"
version: 2
sources:
  - name: raw
    tables:
      - name: t
exposures:
  - name: dashboard
    type: dashboard
    owner:
      name: someone
    depends_on:
      - ref('a')
      - source('raw', 't')
      - ref('missing')
      - metric('revenue')
"#,
        ),
    ]);
    assert!(has_edge(&graph, "model.a", "exposure.dashboard"));
    assert!(has_edge(&graph, "source.raw.t", "exposure.dashboard"));
    let exposure = node(&graph, "exposure.dashboard");
    assert_eq!(
        exposure.file_path.as_deref(),
        Some(std::path::Path::new("models/exposures.yml"))
    );
    let at = |expr: &str| {
        json!({
            "unique_id": "exposure.dashboard",
            "path": "models/exposures.yml",
            "line": null,
            "expr": expr
        })
    };
    assert_eq!(
        analysis(&graph, "exposure.dashboard")["unresolved"],
        json!([
            {
                "type": "unsupported_dependency_expression",
                "function": "metric",
                "at": at("metric('revenue')")
            },
            {
                "type": "dependency_target_missing",
                "name": "missing",
                "at": at("ref('missing')")
            }
        ])
    );
}

#[test]
fn generic_test_arguments_are_dependencies() {
    let (_tmp, graph) = build_project(&[
        ("models/a.sql", "select 1 as id"),
        ("models/b.sql", "select 1 as a_id"),
        (
            "models/schema.yml",
            r#"
version: 2
models:
  - name: b
    columns:
      - name: a_id
        tests:
          - relationships:
              arguments:
                to: ref('a')
                field: id
          - accepted_values:
              arguments:
                values: ["ref(", "x"]
"#,
        ),
    ]);
    let relationships = "test.relationships.b.a_id";
    assert!(has_edge(&graph, "model.b", relationships));
    assert!(has_edge(&graph, "model.a", relationships));
    assert_eq!(analysis(&graph, relationships), complete());
    // Text that mentions a dependency function without a literal call.
    assert_eq!(
        analysis(&graph, "test.accepted_values.b.a_id")["unresolved"][0]["type"],
        "unsupported_dependency_expression"
    );
}

#[test]
fn yaml_snapshot_relation_that_is_not_a_dependency_call_is_reported() {
    let (_tmp, graph) = build_project(&[(
        "snapshots/snapshots.yml",
        r#"
version: 2
snapshots:
  - name: snap
    relation: "{{ target.schema }}.orders"
"#,
    )]);
    assert_eq!(
        analysis(&graph, "snapshot.snap")["unresolved"][0]["type"],
        "unsupported_dependency_expression"
    );
}

#[test]
fn semantic_nodes_are_not_assessed() {
    let (_tmp, graph) = build_project(&[
        ("models/orders.sql", "select 1 as id"),
        (
            "models/semantic.yml",
            r#"
version: 2
semantic_models:
  - name: orders_sm
    model: ref('orders')
    entities:
      - name: order
        type: primary
        expr: id
"#,
        ),
    ]);
    assert_eq!(
        analysis(&graph, "semantic_model.orders_sm"),
        json!({
            "status": "partial",
            "unresolved": [{"type": "not_assessed", "scope": "semantic_dependencies"}]
        })
    );
}
