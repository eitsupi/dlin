//! How the CLI reports the model dependency analysis: the stderr summary
//! and `--verbose` diagnostics, and the impact exploration.

use super::*;
use std::process::{Command, Output};

/// A project with the reasons the summary groups, a reference to a missing
/// model shared by two models, and a configured path that does not exist.
fn project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let files: &[(&str, &str)] = &[
        (
            "dbt_project.yml",
            "name: my_project\nmodel-paths: [\"models\", \"more_models\"]\n",
        ),
        ("models/upstream_a.sql", "select 1 as id\n"),
        ("models/stg_kind_x.sql", "select 1 as id\n"),
        (
            "models/downstream_static.sql",
            "select * from {{ ref('upstream_a') }}\n",
        ),
        (
            "models/downstream_var_nodefault.sql",
            "select * from {{ ref('stg_kind_' ~ var('kind')) }}\n",
        ),
        (
            "models/downstream_env.sql",
            "select * from {{ ref(env_var('DLIN_REVIEW_UPSTREAM')) }}\n",
        ),
        (
            "models/downstream_dispatch.sql",
            "select * from {{ adapter.dispatch('kind_x_relation')() }}\n",
        ),
        (
            "models/downstream_types.sql",
            "select cast(id as {{ dbt.type_string() }}) from {{ ref('upstream_a') }}\n",
        ),
        (
            "models/missing_user_1.sql",
            "select * from {{ ref('gone') }}\n",
        ),
        (
            "models/missing_user_2.sql",
            "select * from {{ ref('gone') }}\n",
        ),
    ];
    for (path, content) in files {
        let path = tmp.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    tmp
}

fn run(project: &Path, args: &[&str]) -> Output {
    let output = Command::new(binary_path())
        .args(args)
        .arg("-p")
        .arg(project)
        .env_remove("DLIN_ERROR_FORMAT")
        .env_remove("DLIN_REVIEW_UPSTREAM")
        .output()
        .expect("failed to run dlin");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn json_lines(output: &Output) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

fn codes(lines: &[serde_json::Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["code"].as_str().unwrap_or("").to_string())
        .collect()
}

#[test]
fn default_stderr_has_the_config_warning_and_one_summary() {
    let tmp = project();
    // Two inputs still give one summary.
    let output = run(
        tmp.path(),
        &[
            "--error-format",
            "json",
            "impact",
            "upstream_a",
            "stg_kind_x",
            "-o",
            "json",
        ],
    );
    let lines = json_lines(&output);
    assert_eq!(
        codes(&lines),
        [
            "configured_input_path_missing",
            "dependency_analysis_summary"
        ]
    );
    let summary = &lines[1];
    assert_eq!(summary["level"], "warning");
    assert_eq!(summary["partial_nodes"], 6);
    assert_eq!(summary["not_assessed_nodes"], 0);
    assert_eq!(
        summary["reason_counts"],
        serde_json::json!({
            "runtime_value_unknown": 1,
            "unresolved_ref": 2,
            "unresolved_var": 1,
            "unsupported_dispatch": 1,
            "unsupported_macro_namespace": 1
        })
    );
    assert_eq!(
        summary["namespace_counts"],
        serde_json::json!({"unsupported_macro_namespace": {"dbt": 1}})
    );
    assert_eq!(lines[0]["config"]["paths"][0]["path"], "more_models");
}

#[test]
fn text_summary_replaces_the_unresolved_ref_warnings() {
    let tmp = project();
    let output = run(tmp.path(), &["list"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stderr.matches("[dependency_analysis_summary]").count(),
        1,
        "{stderr}"
    );
    assert!(!stderr.contains("unresolved ref '"), "{stderr}");
    assert!(stderr.contains("unresolved_ref 2"), "{stderr}");
}

#[test]
fn verbose_lists_each_reason_without_changing_stdout() {
    let tmp = project();
    let args = [
        "--error-format",
        "json",
        "impact",
        "upstream_a",
        "-o",
        "json",
    ];
    let quiet_default = run(tmp.path(), &args);
    let mut verbose_args = vec!["--verbose"];
    verbose_args.extend(args);
    let verbose = run(tmp.path(), &verbose_args);
    assert_eq!(quiet_default.stdout, verbose.stdout);

    let lines = json_lines(&verbose);
    let unresolved: Vec<&str> = lines
        .iter()
        .filter(|line| line["code"] == "unresolved_ref")
        .map(|line| line["unique_id"].as_str().unwrap())
        .collect();
    // Every model referencing the missing target has the reason.
    assert_eq!(unresolved, ["model.missing_user_1", "model.missing_user_2"]);
    let var = lines
        .iter()
        .find(|line| line["code"] == "unresolved_var")
        .unwrap();
    assert_eq!(
        var["reason"]["at"]["path"],
        "models/downstream_var_nodefault.sql"
    );
    assert_eq!(var["reason"]["name"], "kind");
    // The JSON format alone does not add the details.
    assert!(!codes(&json_lines(&quiet_default)).contains(&"unresolved_var".to_string()));
}

#[test]
fn quiet_wins_over_verbose() {
    let tmp = project();
    let output = run(tmp.path(), &["--verbose", "list", "-q"]);
    assert!(output.stderr.is_empty());
}

/// A model whose ref is named by an unset environment variable has no edge;
/// the impact of its intended target reaches nothing but is not proved
/// empty, and the command still succeeds.
#[test]
fn impact_with_no_reached_node_reports_the_gap() {
    let tmp = project();
    let output = run(tmp.path(), &["impact", "stg_kind_x", "-o", "json", "-q"]);
    let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let report = &reports[0];
    assert_eq!(report["impacted_nodes"], serde_json::json!([]));
    assert_eq!(report["overall_severity"], "low");
    assert_eq!(report["source_node"]["unique_id"], "model.stg_kind_x");
    let exploration = &report["exploration"][0];
    assert_eq!(exploration["kind"], "model_dependency");
    assert_eq!(exploration["status"], "incomplete");
    let gap_ids: Vec<&str> = exploration["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|gap| gap["unique_id"].as_str())
        .collect();
    assert!(gap_ids.contains(&"model.downstream_env"), "{gap_ids:?}");
    assert!(
        gap_ids.contains(&"model.downstream_var_nodefault"),
        "{gap_ids:?}"
    );

    let text = run(tmp.path(), &["impact", "stg_kind_x", "-q"]);
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("Exploration: incomplete"), "{stdout}");
    assert!(stdout.contains("model.downstream_env"), "{stdout}");
}

#[test]
fn list_json_shows_the_dependency_analysis_of_each_node() {
    let tmp = project();
    let output = run(tmp.path(), &["list", "downstream_env", "-o", "json", "-q"]);
    let nodes: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let analysis = &nodes[0]["dependency_analysis"];
    assert_eq!(analysis["status"], "partial");
    assert_eq!(analysis["unresolved"][0]["type"], "runtime_value_unknown");
    assert_eq!(analysis["unresolved"][0]["source"], "env_var");
    assert_eq!(analysis["unresolved"][0]["name"], "DLIN_REVIEW_UPSTREAM");
}

fn small_project(extra: &[(&str, &str)]) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let mut files = vec![
        ("dbt_project.yml", "name: shop\n"),
        ("models/orders.sql", "select 1 as id\n"),
        (
            "models/customers.sql",
            "select * from {{ ref('orders') }}\n",
        ),
    ];
    files.extend_from_slice(extra);
    for (path, content) in files {
        let path = tmp.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    tmp
}

fn exploration(output: &Output) -> serde_json::Value {
    let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    reports[0]["exploration"][0].clone()
}

/// A YAML file that cannot be parsed is skipped; the gap reaches both the
/// impact report and stderr.
#[test]
fn skipped_yaml_reaches_the_report_and_stderr() {
    let tmp = small_project(&[("models/broken.yml", "version: 2\nmodels: [\n")]);
    let output = run(
        tmp.path(),
        &[
            "--error-format",
            "json",
            "impact",
            "customers",
            "-o",
            "json",
        ],
    );
    assert_eq!(
        exploration(&output)["gaps"],
        serde_json::json!([{
            "type": "input_skipped",
            "location": {"path": "models/broken.yml", "line": null},
            "kind": "yaml",
            "stage": "parse",
            "failure": "parse_error"
        }])
    );
    let lines = json_lines(&output);
    let summary = lines
        .iter()
        .find(|line| line["code"] == "dependency_analysis_summary")
        .unwrap();
    assert_eq!(summary["level"], "warning");
    assert_eq!(
        summary["project_gap_counts"],
        serde_json::json!({"input_skipped": 1})
    );
}

/// A configured path that does not exist holds no input to miss: it is a
/// warning, and the report stays complete.
#[test]
fn missing_configured_path_keeps_the_report_complete() {
    let tmp = small_project(&[(
        "dbt_project.yml",
        "name: shop\nmodel-paths: [\"models\", \"gone\"]\n",
    )]);
    let output = run(
        tmp.path(),
        &[
            "--error-format",
            "json",
            "impact",
            "customers",
            "-o",
            "json",
        ],
    );
    let exploration = exploration(&output);
    assert_eq!(exploration["status"], "complete");
    assert_eq!(exploration["gaps"], serde_json::json!([]));
    assert_eq!(
        codes(&json_lines(&output)),
        ["configured_input_path_missing"]
    );
}
