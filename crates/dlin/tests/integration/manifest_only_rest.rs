use super::*;

#[test]
fn test_graph_manifest_mode_jinja_path_like_input_ignores_project_flag() {
    let tmp = minimal_manifest_dir(None);
    let manifest_path = tmp.path().join("target/manifest.json");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap()
        .replace("models/stg_orders.sql", "models/stg_orders.sql.jinja");
    fs::write(&manifest_path, manifest).unwrap();
    fs::write(
        tmp.path().join("dbt_project.yml"),
        "name: test_project\nflags:\n  allow_jinja_file_extensions: false\n",
    )
    .unwrap();

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
        "graph with Jinja-suffixed path-like input should ignore the project flag in manifest mode; stderr: {}",
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
    assert_eq!(node_labels, vec!["stg_orders"]);
}

#[test]
fn test_list_manifest_mode_path_like_input_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "list",
            "models/orders.sql",
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
        "list with path-like input should succeed in manifest mode without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    let node_labels: Vec<&str> = parsed
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|n| n["label"].as_str())
        .collect();
    assert!(
        node_labels.contains(&"orders"),
        "Should resolve models/orders.sql to orders node: {:?}",
        node_labels
    );
}

#[test]
fn test_impact_manifest_mode_path_like_input_without_project_yml() {
    let tmp = minimal_manifest_dir(None);
    let output = Command::new(binary_path())
        .args([
            "impact",
            "models/stg_orders.sql",
            "--source",
            "manifest",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "impact with path-like input should succeed in manifest mode without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("orders"),
        "Should show downstream model 'orders': {}",
        stdout
    );
}

/// Create a temp dir with manifest.json that includes compiled_code for column lineage.
fn minimal_manifest_dir_with_compiled_code() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();

    let manifest_json = r#"{
  "metadata": {"adapter_type": "postgres"},
  "nodes": {
    "model.test_project.stg_orders": {
      "unique_id": "model.test_project.stg_orders",
      "name": "stg_orders",
      "original_file_path": "models/stg_orders.sql",
      "resource_type": "model",
      "depends_on": {"nodes": ["source.test_project.raw.orders"]},
      "config": {"materialized": "view", "tags": []},
      "description": "Staged orders",
      "compiled_code": "SELECT order_id, amount FROM raw.orders"
    },
    "model.test_project.orders": {
      "unique_id": "model.test_project.orders",
      "name": "orders",
      "original_file_path": "models/orders.sql",
      "resource_type": "model",
      "depends_on": {"nodes": ["model.test_project.stg_orders"]},
      "config": {"materialized": "table", "tags": []},
      "description": "Orders mart",
      "compiled_code": "SELECT stg_orders.order_id, stg_orders.amount FROM stg_orders"
    }
  },
  "sources": {
    "source.test_project.raw.orders": {
      "unique_id": "source.test_project.raw.orders",
      "name": "orders",
      "source_name": "raw",
      "resource_type": "source",
      "description": "Raw orders"
    }
  },
  "exposures": {}
}"#;

    fs::write(tmp.path().join("target/manifest.json"), manifest_json).unwrap();
    tmp
}

/// A small BigQuery manifest covering row-value alias partial lineage. The
/// upstream model's STRUCT field cannot be proven by the backend, but the
/// downstream model still has an honest nearest model terminal.
fn row_value_alias_manifest_dir() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("target")).unwrap();
    let manifest_json = r#"{
  "metadata": {"adapter_type": "bigquery"},
  "nodes": {
    "model.test_project.upstream_model": {
      "unique_id": "model.test_project.upstream_model",
      "name": "upstream_model",
      "resource_type": "model",
      "depends_on": {"nodes": []},
      "config": {"materialized": "view", "tags": []},
      "columns": {"col_a": {"name": "col_a"}},
      "compiled_code": "WITH latest AS (SELECT ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t) SELECT event.col_a AS col_a FROM latest"
    },
    "model.test_project.repro_model": {
      "unique_id": "model.test_project.repro_model",
      "name": "repro_model",
      "resource_type": "model",
      "depends_on": {"nodes": ["model.test_project.upstream_model"]},
      "config": {"materialized": "view", "tags": []},
      "columns": {"col_a": {"name": "col_a"}},
      "compiled_code": "SELECT col_a FROM `p`.`d`.`upstream_model`"
    }
  },
  "sources": {}, "exposures": {}
}"#;
    fs::write(tmp.path().join("target/manifest.json"), manifest_json).unwrap();
    tmp
}

#[test]
fn test_row_value_alias_partial_lineage_is_non_fatal_and_structured() {
    let tmp = row_value_alias_manifest_dir();
    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "repro_model",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .current_dir(tmp.path())
        .output()
        .expect("failed to run dlin");

    assert!(
        output.status.success(),
        "indeterminate-only lineage should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reports: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let report = &reports[0];
    let source = &report["columns"][0]["sources"][0];
    assert_eq!(source["table"], "p.d.upstream_model");
    assert_eq!(source["column"], "col_a");
    assert!(source["model_path"].is_null() || source["model_path"].as_array().unwrap().is_empty());
    assert_eq!(report["errors"][0]["kind"], "column_indeterminate");
    assert_eq!(report["errors"][0]["column"], "col_a");

    let filtered = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "repro_model",
            "--column",
            "col_a",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .current_dir(tmp.path())
        .output()
        .expect("failed to run filtered dlin");
    assert!(
        filtered.status.success(),
        "column-scoped indeterminate should survive --column filtering without becoming fatal; stderr: {}",
        String::from_utf8_lossy(&filtered.stderr)
    );
}

#[test]
fn test_column_manifest_mode_path_like_input_without_project_yml() {
    let tmp = minimal_manifest_dir_with_compiled_code();
    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "models/stg_orders.sql",
            "--column",
            "order_id",
            "--manifest-path",
            tmp.path().join("target/manifest.json").to_str().unwrap(),
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "column with path-like input should succeed in manifest mode without dbt_project.yml; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("stg_orders"),
        "Should show stg_orders in column lineage output: {}",
        stdout
    );
}

/// Build a manifest JSON string with the given adapter_type value (or none).
fn manifest_json_with_adapter(adapter_type: Option<&str>) -> String {
    let metadata = match adapter_type {
        Some(a) => format!(r#"{{"adapter_type": "{}"}}"#, a),
        None => "{}".to_string(),
    };
    format!(
        r#"{{
  "metadata": {},
  "nodes": {{
    "model.test_project.stg_orders": {{
      "unique_id": "model.test_project.stg_orders",
      "name": "stg_orders",
      "original_file_path": "models/stg_orders.sql",
      "resource_type": "model",
      "depends_on": {{"nodes": []}},
      "config": {{"materialized": "view", "tags": []}},
      "description": "",
      "compiled_code": "SELECT order_id FROM raw.orders",
      "columns": {{"order_id": {{"name": "order_id", "description": ""}}}}
    }}
  }},
  "sources": {{}},
  "exposures": {{}}
}}"#,
        metadata
    )
}

fn write_manifest(dir: &std::path::Path, json: &str) {
    fs::create_dir_all(dir.join("target")).unwrap();
    fs::write(dir.join("target/manifest.json"), json).unwrap();
}

#[test]
fn test_column_upstream_auto_detects_dialect_from_manifest_adapter_type() {
    // When --dialect is omitted but manifest has adapter_type, dialect is auto-detected.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(Some("postgres")));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "should succeed with auto-detected dialect from adapter_type; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_column_upstream_supported_warehouse_dialect_has_no_warning() {
    // A dialect with a native sqllineage mapping is used directly, without
    // the compatibility downgrade that applies to removed dialects.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(Some("duckdb")));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "should succeed; stderr: {stderr}");
    assert!(
        !stderr.contains("no longer supported"),
        "unexpected warning: {stderr}"
    );
}

#[test]
fn test_column_upstream_explicit_removed_dialect_warns_once() {
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(Some("postgres")));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--dialect",
            "presto",
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "should succeed; stderr: {stderr}");
    assert_eq!(
        stderr
            .matches("no longer supported by the column-lineage backend")
            .count(),
        1,
        "expected exactly one dialect warning: {stderr}"
    );
}

#[test]
fn test_column_upstream_errors_when_no_adapter_type_and_no_dialect_flag() {
    // When both --dialect and manifest adapter_type are absent, the command must fail
    // with an actionable error rather than silently using Generic.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(None));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        !output.status.success(),
        "should fail when adapter_type is absent and --dialect is not given"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not specify an adapter_type"),
        "error message should say 'does not specify an adapter_type': {}",
        stderr
    );
}

#[test]
fn test_column_upstream_errors_on_unknown_adapter_type() {
    // An unrecognised adapter_type in the manifest must produce a clear error
    // telling the user to pass --dialect explicitly.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(
        tmp.path(),
        &manifest_json_with_adapter(Some("unknown_warehouse")),
    );

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        !output.status.success(),
        "should fail when adapter_type is not a known dialect"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown_warehouse") && stderr.contains("--dialect"),
        "error should name the unknown adapter and suggest --dialect: {}",
        stderr
    );
}

#[test]
fn test_column_upstream_dialect_flag_overrides_manifest_adapter_type() {
    // --dialect takes precedence over whatever adapter_type the manifest declares.
    let tmp = tempfile::tempdir().unwrap();
    // adapter_type is deliberately wrong (trino); --dialect postgres should win.
    write_manifest(tmp.path(), &manifest_json_with_adapter(Some("trino")));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--dialect",
            "postgres",
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        output.status.success(),
        "--dialect flag should override adapter_type in manifest; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_column_downstream_errors_when_no_adapter_type_and_no_dialect_flag() {
    // Same error behaviour for the impact (downstream) subcommand.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(None));

    let output = Command::new(binary_path())
        .args([
            "column",
            "downstream",
            "stg_orders",
            "--column",
            "order_id",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        !output.status.success(),
        "column downstream should also fail without adapter_type and --dialect"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not specify an adapter_type"),
        "error message should say 'does not specify an adapter_type': {}",
        stderr
    );
}

#[test]
fn test_column_upstream_errors_on_empty_adapter_type() {
    // An empty string adapter_type must not silently fall back to Generic.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(Some("")));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        !output.status.success(),
        "empty adapter_type should fail rather than silently using Generic"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("adapter_type is empty"),
        "error message should say 'adapter_type is empty': {}",
        stderr
    );
}

#[test]
fn test_column_upstream_errors_on_whitespace_only_adapter_type() {
    // A whitespace-only adapter_type must not silently fall back to Generic.
    let tmp = tempfile::tempdir().unwrap();
    write_manifest(tmp.path(), &manifest_json_with_adapter(Some("   ")));

    let output = Command::new(binary_path())
        .args([
            "column",
            "upstream",
            "stg_orders",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "--no-cache",
        ])
        .output()
        .expect("Failed to run binary");

    assert!(
        !output.status.success(),
        "whitespace-only adapter_type should fail rather than silently using Generic"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("adapter_type is empty"),
        "error message should say 'adapter_type is empty': {}",
        stderr
    );
}
