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
    // The CLI canonicalises the project directory, as discovery does with
    // the paths it finds.
    let project_dir = tmp.path().canonicalize().unwrap();
    let project = DbtProject::load(&project_dir).unwrap();
    let discovered = discover_files(&project.resolve_paths(&project_dir)).unwrap();
    let graph = build_graph(&project_dir, &project.name, &discovered, &project.vars).unwrap();
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
      - ref('a') | upper
      - orders
"#,
        ),
    ]);
    // `ref('a') | upper` is evaluated like the others, as dbt does.
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
                "type": "unresolved_call",
                "callee": "metric",
                "at": at("metric('revenue')")
            },
            {
                "type": "unsupported_dependency_expression",
                "function": null,
                "at": at("orders")
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
fn generic_test_arguments_are_dependencies_as_dbt_evaluates_them() {
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
                to: ref ('a')
                field: id
          - accepted_values:
              arguments:
                values: ["ref(", "see ref('a')", "x"]
      - name: id
        tests:
          - accepted_values:
              arguments:
                values: ["ref('a')"]
          - relationships:
              arguments:
                to: ref(var('target_model'))
                field: id
          - dbt_utils.equality:
              arguments:
                compare_model: ref(var('compare', 'a'))
                fallback: var('x', ref('a'))
          - custom_check:
              name: "ref('not_an_argument')"
              description: "ref('not_an_argument')"
              arguments:
                options:
                  name: "ref('a')"
                  description: "ref('a')"
                relation: "{{ choose_relation() }}"
                skipped: "{% if false %}{{ ref('never') }}{% endif %}"
          - dbt_utils.expression_is_true:
              arguments:
                expression: "{{ ref('a') }} {{ ref(var('other')) }}"
          - unique:
              config:
                where: "{{ ref('a') }} {# ref(var('x')) #} 'ref(x)'"
"#,
        ),
    ]);
    // A string that is a single call is evaluated, wherever it is in the
    // arguments, as dbt does; whitespace before the parenthesis included.
    for unique_id in ["test.relationships.b.a_id", "test.accepted_values.b.id"] {
        assert!(has_edge(&graph, "model.b", unique_id), "{unique_id}");
        assert!(has_edge(&graph, "model.a", unique_id), "{unique_id}");
        assert_eq!(analysis(&graph, unique_id), complete(), "{unique_id}");
    }
    // Other strings are data, even when they mention `ref(`.
    let data = "test.accepted_values.b.a_id";
    assert!(!has_edge(&graph, "model.a", data));
    assert_eq!(analysis(&graph, data), complete());
    // Expressions are interpreted as in SQL: vars resolve from the project
    // or their defaults, and arguments of other functions are evaluated.
    assert_eq!(
        analysis(&graph, "test.relationships.b.id")["unresolved"],
        json!([{
            "type": "unresolved_var",
            "name": "target_model",
            "at": {
                "unique_id": "test.relationships.b.id",
                "path": "models/schema.yml",
                "line": null,
                "expr": "var('target_model')"
            }
        }])
    );
    assert!(has_edge(&graph, "model.a", "test.dbt_utils.equality.b.id"));
    assert_eq!(analysis(&graph, "test.dbt_utils.equality.b.id"), complete());
    // A macro that is not defined may hide a dependency; a branch known to
    // be false is not followed.
    // The test's name and description are not arguments; keys with these
    // names inside the arguments are.
    let custom = "test.custom_check.b.id";
    assert!(has_edge(&graph, "model.a", custom));
    assert!(
        graph
            .node_indices()
            .all(|idx| graph[idx].unique_id != "model.not_an_argument")
    );
    assert_eq!(
        analysis(&graph, custom)["unresolved"][0]["type"],
        "unresolved_call"
    );
    assert!(
        graph
            .node_indices()
            .all(|idx| graph[idx].unique_id != "model.never")
    );
    // A template is interpreted too; calls in comments and string literals
    // are not calls.
    let template = "test.dbt_utils.expression_is_true.b.id";
    assert!(has_edge(&graph, "model.a", template));
    assert_eq!(
        analysis(&graph, template)["unresolved"][0]["type"],
        "unresolved_var"
    );
    assert!(has_edge(&graph, "model.a", "test.unique.b.id"));
    assert_eq!(analysis(&graph, "test.unique.b.id"), complete());
}

#[test]
fn yaml_snapshot_relation_is_interpreted_as_dbt_does() {
    let (_tmp, graph) = build_project(&[
        ("models/orders.sql", "select 1"),
        (
            "snapshots/snapshots.yml",
            r#"
version: 2
snapshots:
  - name: from_ref
    relation: ref('orders')
  - name: physical
    relation: "{{ target.schema }}.orders"
  - name: from_macro
    relation: "{{ pick_relation() }}"
"#,
        ),
    ]);
    assert!(has_edge(&graph, "model.orders", "snapshot.from_ref"));
    assert_eq!(analysis(&graph, "snapshot.from_ref"), complete());
    // A relation named in SQL has no dbt dependency.
    assert_eq!(analysis(&graph, "snapshot.physical"), complete());
    assert_eq!(
        analysis(&graph, "snapshot.from_macro")["unresolved"][0]["type"],
        "unresolved_call"
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

#[test]
fn reason_paths_are_relative_when_the_directory_is_not_canonical() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("dbt_project.yml"), "name: p\n").unwrap();
    fs::create_dir_all(tmp.path().join("models")).unwrap();
    fs::write(
        tmp.path().join("models/a.sql"),
        r#"select * from {{ ref('missing') }}"#,
    )
    .unwrap();
    let project = DbtProject::load(tmp.path()).unwrap();
    let discovered = discover_files(&project.resolve_paths(tmp.path())).unwrap();
    let graph = build_graph(tmp.path(), &project.name, &discovered, &project.vars).unwrap();
    assert_eq!(
        analysis(&graph, "model.a")["unresolved"][0]["at"]["path"],
        "models/a.sql"
    );
}
