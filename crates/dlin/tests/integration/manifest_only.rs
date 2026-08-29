use super::*;
use std::fs;
use std::process::Command;

/// Create a temp dir with only a manifest.json (no dbt_project.yml, no SQL files).
fn minimal_manifest_dir(project_name: Option<&str>) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();

    let metadata = match project_name {
        Some(name) => format!(r#"{{"project_name": "{}"}}"#, name),
        None => "{}".to_string(),
    };

    let manifest_json = format!(
        r#"{{
  "metadata": {},
  "nodes": {{
    "model.test_project.stg_orders": {{
      "unique_id": "model.test_project.stg_orders",
      "name": "stg_orders",
      "original_file_path": "models/stg_orders.sql",
      "resource_type": "model",
      "depends_on": {{"nodes": ["source.test_project.raw.orders"]}},
      "config": {{"materialized": "view", "tags": []}},
      "description": "Staged orders"
    }},
    "model.test_project.orders": {{
      "unique_id": "model.test_project.orders",
      "name": "orders",
      "original_file_path": "models/orders.sql",
      "resource_type": "model",
      "depends_on": {{"nodes": ["model.test_project.stg_orders"]}},
      "config": {{"materialized": "table", "tags": []}},
      "description": "Orders mart"
    }}
  }},
  "sources": {{
    "source.test_project.raw.orders": {{
      "unique_id": "source.test_project.raw.orders",
      "name": "orders",
      "source_name": "raw",
      "resource_type": "source",
      "description": "Raw orders"
    }}
  }},
  "exposures": {{}}
}}"#,
        metadata
    );

    fs::write(tmp.path().join("target/manifest.json"), manifest_json).unwrap();
    tmp
}

fn forward_compat_manifest_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();
    let manifest = serde_json::json!({
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v99/manifest.json",
            "dbt_version": "1.9.0"
        },
        "nodes": {
            "model.test_project.orders": {
                "unique_id": "model.test_project.orders",
                "name": "orders",
                "resource_type": "model",
                "depends_on": {"nodes": []},
                "config": {},
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "compiled_code": null,
                "database": null,
                "schema": null
            },
            "operation.test_project.refresh": {
                "unique_id": "operation.test_project.refresh",
                "name": "refresh",
                "resource_type": "operation",
                "depends_on": {"nodes": []},
                "config": {},
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "compiled_code": null,
                "database": null,
                "schema": null
            }
        },
        "future_resources": {
            "future.test_project.item": {"resource_type": "future_resource"}
        }
    });
    fs::write(
        tmp.path().join("target/manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    tmp
}

#[test]
fn test_manifest_forward_compatibility_warnings_cli_contract() {
    let tmp = forward_compat_manifest_dir();
    let manifest_path = tmp.path().join("target/manifest.json");
    let common_args = [
        "graph",
        "--source",
        "manifest",
        "--manifest-path",
        manifest_path.to_str().unwrap(),
        "--project-dir",
        tmp.path().to_str().unwrap(),
        "-o",
        "plain",
    ];

    let output = Command::new(binary_path())
        .args(common_args)
        .output()
        .expect("failed to run forward-compatible manifest command");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    insta::assert_snapshot!(stderr.as_ref(), @r###"
Warning: [future_schema_version] manifest uses a future dbt schema version: https://schemas.getdbt.com/dbt/manifest/v99/manifest.json
  Hint: Some resource types may not be understood by this version of dlin
Warning: [unknown_top_level_key] unknown top-level manifest key: future_resources
  Hint: The key is retained in Manifest::extra for forward compatibility
Warning: [unsupported_resource_type] manifest resource 'operation.test_project.refresh' in 'nodes' uses unsupported resource type 'operation'
  Hint: Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results
Warning: [unsupported_resource_type] manifest resource 'future.test_project.item' in 'future_resources' uses unsupported resource type 'future_resource'
  Hint: Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results
"###);

    let mut json_args = vec!["--error-format", "json"];
    json_args.extend(common_args);
    let output = Command::new(binary_path())
        .args(json_args)
        .output()
        .expect("failed to run JSON warning command");
    assert!(output.status.success());
    let mut warnings: Vec<serde_json::Value> = String::from_utf8_lossy(&output.stderr)
        .lines()
        .map(|line| serde_json::from_str(line).expect("warning should be JSON"))
        .collect();
    warnings.sort_by_key(|warning| {
        (
            warning["kind"].as_str().unwrap_or_default().to_string(),
            warning["raw_resource"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        )
    });
    insta::assert_snapshot!(serde_json::to_string_pretty(&warnings).unwrap(), @r###"
[
  {
    "hint": "Some resource types may not be understood by this version of dlin",
    "kind": "future_schema_version",
    "level": "warning",
    "raw_resource": "metadata.dbt_schema_version",
    "raw_type": null,
    "what": "manifest uses a future dbt schema version: https://schemas.getdbt.com/dbt/manifest/v99/manifest.json",
    "why": null
  },
  {
    "hint": "The key is retained in Manifest::extra for forward compatibility",
    "kind": "unknown_top_level_key",
    "level": "warning",
    "raw_resource": "future_resources",
    "raw_type": null,
    "what": "unknown top-level manifest key: future_resources",
    "why": null
  },
  {
    "hint": "Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results",
    "kind": "unsupported_resource_type",
    "level": "warning",
    "raw_resource": "future.test_project.item",
    "raw_type": "future_resource",
    "what": "manifest resource 'future.test_project.item' in 'future_resources' uses unsupported resource type 'future_resource'",
    "why": null
  },
  {
    "hint": "Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results",
    "kind": "unsupported_resource_type",
    "level": "warning",
    "raw_resource": "operation.test_project.refresh",
    "raw_type": "operation",
    "what": "manifest resource 'operation.test_project.refresh' in 'nodes' uses unsupported resource type 'operation'",
    "why": null
  }
]
"###);

    let output = Command::new(binary_path())
        .args(common_args.iter().copied().chain(["-q"]))
        .output()
        .expect("failed to run quiet command");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let old = copy_fixture_to_temp();
    let old_output = Command::new(binary_path())
        .args([
            "graph",
            "--source",
            "manifest",
            "--manifest-path",
            old.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            old.path().to_str().unwrap(),
            "-o",
            "plain",
        ])
        .output()
        .expect("failed to run old manifest command");
    assert!(old_output.status.success());
    insta::assert_snapshot!(String::from_utf8_lossy(&old_output.stderr).as_ref(), @r###""###);
}

#[test]
fn test_summary_manifest_mode_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "summary",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "summary --source manifest should succeed without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Source:  manifest"),
        "Should show manifest source mode: {}",
        stdout
    );
    assert!(
        stdout.contains("model        2"),
        "Should count 2 models: {}",
        stdout
    );
}

#[test]
fn test_summary_manifest_mode_project_name_from_metadata() {
    let tmp = minimal_manifest_dir(Some("my_dbt_project"));
    let output = Command::new(binary_path())
        .args([
            "summary",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "summary --source manifest should succeed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Project: my_dbt_project"),
        "Should read project_name from manifest metadata: {}",
        stdout
    );
}

#[test]
fn test_summary_manifest_mode_unknown_project_name_when_no_metadata() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "summary",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Project: (unknown)"),
        "Should show (unknown) when metadata.project_name absent: {}",
        stdout
    );
}

#[test]
fn test_summary_manifest_mode_json_output() {
    let tmp = minimal_manifest_dir(Some("json_test_project"));
    let output = Command::new(binary_path())
        .args([
            "summary",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert_eq!(parsed["project_name"], "json_test_project");
    assert_eq!(parsed["source_mode"], "manifest");
    assert_eq!(parsed["node_counts"]["model"], 2);
    assert_eq!(parsed["node_counts"]["source"], 1);
    assert_eq!(parsed["vars_count"], 0);
    assert!(
        parsed["manifest_status"].is_null(),
        "manifest_status should be null when dbt_project.yml is absent"
    );
}

#[test]
fn test_summary_manifest_mode_with_malformed_project_yml() {
    let tmp = minimal_manifest_dir(Some("malformed_test"));
    fs::write(tmp.path().join("dbt_project.yml"), "name: [\ninvalid yaml").unwrap();

    let output = Command::new(binary_path())
        .args([
            "summary",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        !output.status.success(),
        "summary should fail when dbt_project.yml is malformed; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Failed to parse") && stderr.contains("dbt_project.yml"),
        "stderr should contain a parse error referencing dbt_project.yml; got: {}",
        stderr
    );
}

#[test]
fn test_graph_manifest_mode_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "graph",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "graph --source manifest should succeed without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(
        parsed["nodes"].as_array().unwrap().len() >= 3,
        "Should have at least 3 nodes (2 models + 1 source): {:?}",
        parsed["nodes"]
    );
}

#[test]
fn test_list_manifest_mode_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "list",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "list --source manifest should succeed without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert!(
        parsed.as_array().unwrap().len() >= 3,
        "Should list at least 3 nodes (2 models + 1 source): {:?}",
        parsed
    );
}

#[test]
fn test_impact_manifest_mode_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "impact",
            "stg_orders",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "impact --source manifest should succeed without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("orders"),
        "Should show downstream model 'orders': {}",
        stdout
    );
}

#[test]
fn test_graph_manifest_mode_path_like_input_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "graph",
            "models/stg_orders.sql",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "graph with path-like input should succeed in manifest mode without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    let node_labels: Vec<&str> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["label"].as_str())
        .collect();
    assert!(
        node_labels.contains(&"stg_orders"),
        "Should resolve models/stg_orders.sql to stg_orders node: {:?}",
        node_labels
    );
}

#[test]
fn test_graph_manifest_mode_jinja_path_like_input_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let manifest_path = tmp.path().join("target/manifest.json");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("models/stg_orders.sql", "models/stg_orders.sql.jinja");
    fs::write(&manifest_path, manifest).unwrap();

    let output = Command::new(binary_path())
        .args([
            "graph",
            "models/stg_orders.sql.jinja",
            "--source",
            "manifest",
            "--manifest-path",
            manifest_path.to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-u",
            "0",
            "-d",
            "0",
            "-o",
            "json",
        ])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "graph with Jinja-suffixed path-like input should succeed in manifest mode without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    let node_labels: Vec<&str> = parsed["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["label"].as_str())
        .collect();
    assert!(
        node_labels.contains(&"stg_orders"),
        "Should resolve models/stg_orders.sql.jinja to stg_orders node: {:?}",
        node_labels
    );
    assert!(
        !node_labels.contains(&"orders"),
        "Should not return unrelated orders node when resolving a single path: {:?}",
        node_labels
    );
}

#[path = "manifest_only_rest.rs"]
mod rest;
