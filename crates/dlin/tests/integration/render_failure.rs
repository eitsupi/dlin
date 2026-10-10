use super::*;
use std::process::Command;

fn issue_199_project(model_sql: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("models")).unwrap();
    std::fs::create_dir_all(tmp.path().join("macros")).unwrap();
    std::fs::write(
        tmp.path().join("dbt_project.yml"),
        "name: issue_199\nmodel-paths: [models]\nmacro-paths: [macros]\n",
    )
    .unwrap();
    // The project macro is valid but unused; scanning the whole prefix adds a
    // false self-edge in both the failed-render and runtime-macro scenarios.
    std::fs::write(tmp.path().join("models/u.sql"), model_sql).unwrap();
    std::fs::write(
        tmp.path().join("models/downstream.sql"),
        "select * from {{ ref('u') }}\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("models/base.sql"), "select 1 as id\n").unwrap();
    std::fs::write(
        tmp.path().join("macros/unused.sql"),
        "{% macro unused() %}{{ ref('u') }}{% endmacro %}\n",
    )
    .unwrap();
    tmp
}

fn run_graph(project: &std::path::Path, cache_args: &[&str]) -> std::process::Output {
    Command::new(binary_path())
        .args(["graph", "--project-dir"])
        .arg(project)
        .args(["--output", "json"])
        .args(cache_args)
        .output()
        .expect("failed to run graph")
}

#[test]
fn test_issue_199_render_failure_does_not_create_self_cycle() {
    let project = issue_199_project("select * from {{ ref('base') }}\n{{ missing_macro() }}\n");
    assert_issue_project_has_no_false_self_edge(&project);

    let impact = Command::new(binary_path())
        .args(["impact", "u", "--project-dir"])
        .arg(project.path())
        .arg("--no-cache")
        .output()
        .expect("failed to run impact");
    assert!(
        impact.status.success(),
        "impact failed: {}",
        String::from_utf8_lossy(&impact.stderr)
    );
    assert!(
        String::from_utf8_lossy(&impact.stdout).contains("downstream"),
        "impact should include downstream model: {}",
        String::from_utf8_lossy(&impact.stdout)
    );
}

#[test]
fn test_issue_199_adapter_runtime_case_does_not_create_self_cycle() {
    let project = issue_199_project(
        "{% if is_incremental() %}select {{ gmid('d') }} from {{ ref('base') }}{% else %}select 1 from {{ ref('base') }}{% endif %}\n",
    );
    std::fs::write(
        project.path().join("macros/gmid.sql"),
        r#"
            {% macro gmid(c) %}
                {% if is_incremental() %}
                    {% set r = adapter.get_relation(this.database, this.schema, this.identifier) %}
                    {% if r %}{% set q = run_query('select 1') %}{% endif %}
                {% endif %}
                current_date()
            {% endmacro %}
        "#,
    )
    .unwrap();
    assert_issue_project_has_no_false_self_edge(&project);

    let impact = Command::new(binary_path())
        .args(["impact", "u", "--project-dir"])
        .arg(project.path())
        .arg("--no-cache")
        .output()
        .expect("failed to run impact");
    assert!(
        impact.status.success(),
        "adapter runtime impact failed: {}",
        String::from_utf8_lossy(&impact.stderr)
    );
    assert!(
        String::from_utf8_lossy(&impact.stdout).contains("downstream"),
        "impact should include downstream model: {}",
        String::from_utf8_lossy(&impact.stdout)
    );
}

#[test]
fn test_runtime_macro_branches_recover_only_reachable_dependencies() {
    let project = runtime_macro_branch_project();
    let no_cache = run_graph(project.path(), &["--no-cache"]);
    assert!(
        no_cache.status.success(),
        "uncached graph failed: {}",
        String::from_utf8_lossy(&no_cache.stderr)
    );
    let no_cache_graph: serde_json::Value = serde_json::from_slice(&no_cache.stdout).unwrap();
    assert_runtime_macro_edges(&no_cache_graph);

    let cache_dir = project.path().join("dlin-cache");
    let cache_dir_arg = cache_dir.to_str().unwrap();
    let cold = run_graph(project.path(), &["--cache-dir", cache_dir_arg]);
    assert!(
        cold.status.success(),
        "cold cached graph failed: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    let warm = run_graph(project.path(), &["--cache-dir", cache_dir_arg]);
    assert!(
        warm.status.success(),
        "warm cached graph failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let cold_graph: serde_json::Value = serde_json::from_slice(&cold.stdout).unwrap();
    let warm_graph: serde_json::Value = serde_json::from_slice(&warm.stdout).unwrap();
    assert_runtime_macro_edges(&cold_graph);
    assert_runtime_macro_edges(&warm_graph);
    assert_eq!(cold_graph, warm_graph, "cold and warm graph output differs");
    assert_eq!(
        no_cache_graph, cold_graph,
        "cached and uncached graph output differs"
    );
}

fn runtime_macro_branch_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("models")).unwrap();
    std::fs::create_dir_all(tmp.path().join("macros")).unwrap();
    std::fs::write(
        tmp.path().join("dbt_project.yml"),
        "name: runtime_macro_repro\nmodel-paths: [models]\nmacro-paths: [macros]\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("models/downstream.sql"),
        "select {{ choose() }} as chosen\n",
    )
    .unwrap();
    std::fs::write(tmp.path().join("models/yes_branch.sql"), "select 1 as id\n").unwrap();
    std::fs::write(tmp.path().join("models/no_branch.sql"), "select 0 as id\n").unwrap();
    std::fs::write(
        tmp.path().join("models/schema.yml"),
        "version: 2\nsources:\n  - name: unused\n    tables:\n      - name: tbl\n",
    )
    .unwrap();
    std::fs::write(
        tmp.path().join("macros/choose.sql"),
        r#"
            {% macro choose() %}
                {% if adapter.get_relation(identifier='x') %}
                    {{ ref('yes_branch') }}
                {% else %}
                    {{ ref('no_branch') }}
                {% endif %}
            {% endmacro %}

            {% macro unused() %}{{ ref('downstream') }}{{ source('unused', 'tbl') }}{% endmacro %}
        "#,
    )
    .unwrap();
    tmp
}

fn assert_runtime_macro_edges(graph: &serde_json::Value) {
    let edges = graph["edges"].as_array().unwrap();
    for source in ["model.yes_branch", "model.no_branch"] {
        assert!(
            edges.iter().any(|edge| {
                edge["source"].as_str() == Some(source)
                    && edge["target"].as_str() == Some("model.downstream")
                    && edge["edge_type"].as_str() == Some("ref")
            }),
            "missing {source} edge: {edges:?}"
        );
    }
    assert!(
        !edges.iter().any(|edge| {
            edge["source"].as_str() == Some("model.downstream")
                && edge["target"].as_str() == Some("model.downstream")
        }),
        "unused macro added downstream self-edge: {edges:?}"
    );
    assert!(
        !edges.iter().any(|edge| {
            edge["source"].as_str() == Some("source.unused.tbl")
                && edge["target"].as_str() == Some("model.downstream")
        }),
        "unused macro source edge leaked into graph: {edges:?}"
    );
}

fn assert_issue_project_has_no_false_self_edge(project: &tempfile::TempDir) {
    let no_cache = run_graph(project.path(), &["--no-cache"]);
    assert!(
        no_cache.status.success(),
        "uncached graph failed: {}",
        String::from_utf8_lossy(&no_cache.stderr)
    );
    let no_cache_graph: serde_json::Value = serde_json::from_slice(&no_cache.stdout).unwrap();
    assert_no_u_self_edge(&no_cache_graph);

    let cache_dir = project.path().join("dlin-cache");
    let cache_dir_arg = cache_dir.to_str().unwrap();
    let cold = run_graph(project.path(), &["--cache-dir", cache_dir_arg]);
    assert!(
        cold.status.success(),
        "cold cached graph failed: {}",
        String::from_utf8_lossy(&cold.stderr)
    );
    let warm = run_graph(project.path(), &["--cache-dir", cache_dir_arg]);
    assert!(
        warm.status.success(),
        "warm cached graph failed: {}",
        String::from_utf8_lossy(&warm.stderr)
    );
    let cold_graph: serde_json::Value = serde_json::from_slice(&cold.stdout).unwrap();
    let warm_graph: serde_json::Value = serde_json::from_slice(&warm.stdout).unwrap();
    assert_no_u_self_edge(&cold_graph);
    assert_no_u_self_edge(&warm_graph);
    assert_eq!(cold_graph, warm_graph, "cold and warm graph output differs");
    assert_eq!(
        no_cache_graph, cold_graph,
        "cached and uncached graph output differs"
    );

    for graph in [&no_cache_graph, &cold_graph, &warm_graph] {
        assert!(graph["edges"].as_array().unwrap().iter().any(|edge| {
            edge["source"].as_str() == Some("model.base")
                && edge["target"].as_str() == Some("model.u")
        }));
    }
}

fn assert_no_u_self_edge(graph: &serde_json::Value) {
    let edges = graph["edges"].as_array().unwrap();
    assert!(
        !edges.iter().any(|edge| {
            edge["source"].as_str() == Some("model.u") && edge["target"].as_str() == Some("model.u")
        }),
        "unexpected model.u self-edge: {edges:?}"
    );
}
