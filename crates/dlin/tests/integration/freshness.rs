use super::*;
use std::fs;
use std::process::Command;

/// Make the manifest newer than every file copied from the fixture. A
/// single model is not sufficient here: macros, YAML, seeds, and tests
/// are all inputs to freshness checks and may have a later fixture mtime.
fn set_mtime_newer_than_fixture(manifest: &Path, fixture_root: &Path) {
    fn latest_file(root: &Path, excluded: &Path) -> Option<PathBuf> {
        let mut latest = None;
        let mut paths: Vec<PathBuf> = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        for path in paths {
            if path == excluded {
                continue;
            }
            if path.is_dir() {
                if let Some(candidate) = latest_file(&path, excluded)
                    && latest.as_ref().is_none_or(|current: &PathBuf| {
                        fs::metadata(candidate.as_path())
                            .unwrap()
                            .modified()
                            .unwrap()
                            > fs::metadata(current.as_path()).unwrap().modified().unwrap()
                    })
                {
                    latest = Some(candidate);
                }
            } else if latest.as_ref().is_none_or(|current: &PathBuf| {
                fs::metadata(&path).unwrap().modified().unwrap()
                    > fs::metadata(current).unwrap().modified().unwrap()
            }) {
                latest = Some(path);
            }
        }
        latest
    }

    let latest = latest_file(fixture_root, manifest).expect("fixture should contain files");
    set_mtime_newer_than(manifest, &latest);
}

fn run_check_manifest(tmp: &tempfile::TempDir) -> serde_json::Value {
    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run check-manifest");
    serde_json::from_slice(&output.stdout).expect("check-manifest should emit JSON")
}

fn run_manifest_summary(tmp: &tempfile::TempDir) -> serde_json::Value {
    let output = Command::new(binary_path())
        .args([
            "summary",
            "--source",
            "manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run summary");
    assert!(
        output.status.success(),
        "summary should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("summary should emit JSON");
    report["manifest_status"].clone()
}

fn stale_files(report: &serde_json::Value) -> &Vec<serde_json::Value> {
    report["stale_files"]
        .as_array()
        .expect("stale_files should be an array")
}

#[test]
fn test_check_manifest_up_to_date() {
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");
    // Touch manifest to make it newer than every fixture input.
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Should be up-to-date: {}", stdout);
    assert!(stdout.contains("up-to-date"));
}

#[test]
fn test_check_manifest_detects_stale_file() {
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");
    let model_path = tmp.path().join("models/staging/stg_orders.sql");

    // Touch manifest first, after all fixture inputs.
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    // Now touch a model file to make it newer
    set_mtime_newer_than(&model_path, &manifest_path);

    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "Should be stale");
    assert!(stdout.contains("stale"));
    assert!(stdout.contains("newer"));
    assert!(stdout.contains("stg_orders.sql"));
}

#[test]
fn test_check_manifest_detects_deleted_file() {
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");
    let model_path = tmp.path().join("models/staging/stg_orders.sql");

    // Touch manifest to make it newer than every fixture input.
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    // Delete a model file that's referenced in the manifest
    fs::remove_file(&model_path).unwrap();

    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
        ])
        .output()
        .expect("Failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "Should be stale when file is deleted"
    );
    assert!(
        stdout.contains("deleted"),
        "Should mention deleted: {}",
        stdout
    );
    assert!(
        stdout.contains("stg_orders.sql"),
        "Should list the deleted file: {}",
        stdout
    );
}

#[test]
fn test_check_manifest_json_deleted_files() {
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");
    let model_path = tmp.path().join("models/staging/stg_orders.sql");

    // Touch manifest to make it newer than every fixture input.
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    // Delete a model file
    fs::remove_file(&model_path).unwrap();

    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert_eq!(parsed["is_stale"], true);
    assert!(parsed["deleted_file_count"].as_u64().unwrap() > 0);
    let deleted = parsed["deleted_files"].as_array().unwrap();
    assert!(
        deleted
            .iter()
            .any(|f| f.as_str().unwrap().contains("stg_orders.sql")),
        "deleted_files should contain stg_orders.sql: {:?}",
        deleted
    );
}

#[test]
fn test_check_manifest_stale_and_deleted_combined() {
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");
    let deleted_model_path = tmp.path().join("models/staging/stg_orders.sql");

    // Touch manifest first, after all fixture inputs.
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    // Delete one file
    fs::remove_file(&deleted_model_path).unwrap();

    // Touch another file to make it newer
    let model_path = tmp.path().join("models/marts/orders.sql");
    set_mtime_newer_than(&model_path, &manifest_path);

    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert_eq!(parsed["is_stale"], true);
    assert!(parsed["stale_file_count"].as_u64().unwrap() > 0);
    assert!(parsed["deleted_file_count"].as_u64().unwrap() > 0);
}

#[test]
fn test_check_manifest_json_up_to_date_has_empty_arrays() {
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");

    // Touch manifest to make it newer than every fixture input.
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    let output = Command::new(binary_path())
        .args([
            "check-manifest",
            "--project-dir",
            tmp.path().to_str().unwrap(),
            "-o",
            "json",
        ])
        .output()
        .expect("Failed to run binary");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
    assert_eq!(parsed["is_stale"], false);
    assert_eq!(parsed["stale_file_count"], 0);
    assert_eq!(parsed["stale_files"].as_array().unwrap().len(), 0);
    assert_eq!(parsed["deleted_file_count"], 0);
    assert_eq!(parsed["deleted_files"].as_array().unwrap().len(), 0);
}

#[test]
fn test_root_project_inputs_make_both_freshness_paths_stale_when_newer() {
    for root_file in ["dbt_project.yml", "vars.yml"] {
        let tmp = copy_fixture_to_temp();
        if root_file == "vars.yml" {
            fs::write(tmp.path().join(root_file), "vars: {}\n").unwrap();
        }

        let manifest_path = tmp.path().join("target/manifest.json");
        set_mtime_newer_than_fixture(&manifest_path, tmp.path());
        set_mtime_newer_than(&tmp.path().join(root_file), &manifest_path);

        let check_report = run_check_manifest(&tmp);
        assert_eq!(check_report["is_stale"], true);
        assert!(
            stale_files(&check_report)
                .iter()
                .any(|file| file.as_str() == Some(root_file)),
            "check-manifest should report newer {root_file}: {check_report}"
        );

        let summary_status = run_manifest_summary(&tmp);
        assert_eq!(summary_status["is_stale"], true);
        assert!(
            stale_files(&summary_status)
                .iter()
                .any(|file| file.as_str() == Some(root_file)),
            "summary should report newer {root_file}: {summary_status}"
        );
    }
}

#[test]
fn test_optional_vars_file_older_than_manifest_does_not_make_manifest_stale() {
    let tmp = copy_fixture_to_temp();
    let vars_path = tmp.path().join("vars.yml");
    fs::write(&vars_path, "vars: {}\n").unwrap();
    let manifest_path = tmp.path().join("target/manifest.json");
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    let check_report = run_check_manifest(&tmp);
    assert_eq!(check_report["is_stale"], false);
    assert!(stale_files(&check_report).is_empty());

    let summary_status = run_manifest_summary(&tmp);
    assert_eq!(summary_status["is_stale"], false);
    assert!(stale_files(&summary_status).is_empty());
}

#[test]
fn test_missing_optional_vars_file_does_not_make_manifest_stale() {
    let tmp = copy_fixture_to_temp();
    assert!(!tmp.path().join("vars.yml").exists());
    let manifest_path = tmp.path().join("target/manifest.json");
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());

    let check_report = run_check_manifest(&tmp);
    assert_eq!(check_report["is_stale"], false);
    assert!(stale_files(&check_report).is_empty());

    let summary_status = run_manifest_summary(&tmp);
    assert_eq!(summary_status["is_stale"], false);
    assert!(stale_files(&summary_status).is_empty());
}

#[test]
fn test_root_freshness_input_is_not_duplicated_when_discovered() {
    let tmp = copy_fixture_to_temp();
    fs::write(
        tmp.path().join("dbt_project.yml"),
        "name: simple_project\nmodel-paths: [.]\n",
    )
    .unwrap();
    let manifest_path = tmp.path().join("target/manifest.json");
    set_mtime_newer_than_fixture(&manifest_path, tmp.path());
    set_mtime_newer_than(&tmp.path().join("dbt_project.yml"), &manifest_path);

    let check_report = run_check_manifest(&tmp);
    assert_eq!(check_report["stale_file_count"], 1);
    assert_eq!(stale_files(&check_report)[0], "dbt_project.yml");

    let summary_status = run_manifest_summary(&tmp);
    assert_eq!(summary_status["stale_file_count"], 1);
    assert_eq!(stale_files(&summary_status)[0], "dbt_project.yml");
}

#[test]
fn package_paths_are_verified_independently_and_missing_packages_are_unverified() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("models")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    fs::create_dir_all(root.join("vendor/elementary/models")).unwrap();
    fs::write(
        root.join("dbt_project.yml"),
        "name: sample\npackages-install-path: vendor\n",
    )
    .unwrap();
    fs::write(root.join("models/shared.sql"), "select 1").unwrap();
    let model = |package: &str, name: &str| {
        serde_json::json!({
            "unique_id":format!("model.{package}.{name}"), "package_name":package,
            "name":name,"resource_type":"model","original_file_path":format!("models/{name}.sql")
        })
    };
    let mut nodes = serde_json::Map::new();
    nodes.insert("model.sample.shared".into(), model("sample", "shared"));
    nodes.insert(
        "model.elementary.shared".into(),
        model("elementary", "shared"),
    );
    nodes.insert("model.missing.shared".into(), model("missing", "shared"));
    let artifact = root.join("target/manifest.json");
    for i in 0..29 {
        let name = format!("model_{i}");
        nodes.insert(
            format!("model.elementary.{name}"),
            model("elementary", &name),
        );
        fs::write(
            root.join(format!("vendor/elementary/models/{name}.sql")),
            "select 1",
        )
        .unwrap();
    }
    let shared = root.join("vendor/elementary/models/shared.sql");
    fs::write(&shared, "select 1").unwrap();
    fs::write(
        &artifact,
        serde_json::json!({
            "metadata":{"project_name":"sample","adapter_type":"duckdb"},"nodes":nodes,
        })
        .to_string(),
    )
    .unwrap();
    set_mtime_newer_than_fixture(&artifact, root);
    let check = run_check_manifest(&tmp);
    assert_eq!(check["verification"], "partially_verified");
    assert_eq!(check["is_stale"], false);
    assert_eq!(check["deleted_file_count"], 0);
    assert_eq!(check["unverified"][0]["package"], "missing");
    let summary = run_manifest_summary(&tmp);
    assert_eq!(summary["unverified"], check["unverified"]);
    assert_eq!(
        run_manifest_summary(&tmp),
        summary,
        "warm cache retains package ownership"
    );

    fs::remove_file(shared).unwrap();
    let check = run_check_manifest(&tmp);
    assert_eq!(check["is_stale"], true);
    assert_eq!(
        check["deleted_files"],
        serde_json::json!(["vendor/elementary/models/shared.sql"])
    );
    assert_eq!(check["unverified"][0]["package"], "missing");
    assert_eq!(
        run_manifest_summary(&tmp)["deleted_files"],
        check["deleted_files"]
    );
}

#[test]
fn check_manifest_and_mcp_agree_about_unresolved_package_installation_and_deletions() {
    use std::io::Write;
    use std::process::Stdio;

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(
        root.join("dbt_project.yml"),
        "name: sample\npackages-install-path: \"{{ env_var('PACKAGE_PATH') }}\"\n",
    )
    .unwrap();
    let artifact = root.join("target/manifest.json");
    fs::write(&artifact, r#"{
        "metadata":{"project_name":"sample","adapter_type":"duckdb"},
        "nodes":{
            "model.sample.gone":{"unique_id":"model.sample.gone","name":"gone","resource_type":"model","original_file_path":"models/gone.sql"},
            "model.elementary.model":{"unique_id":"model.elementary.model","name":"model","resource_type":"model","original_file_path":"models/model.sql"}
        }
    }"#).unwrap();
    set_mtime_newer_than_fixture(&artifact, root);
    let check = run_check_manifest(&tmp);
    assert_eq!(check["verification"], "partially_verified");
    assert_eq!(
        check["unverified"],
        serde_json::json!([{"package":"elementary","reason":"unresolved_install_path"}])
    );
    assert_eq!(
        check["deleted_files"],
        serde_json::json!(["models/gone.sql"])
    );

    let mut server = Command::new(binary_path())
        .arg("mcp")
        .args(["--project-dir", root.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        server.stdin.take().unwrap(),
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"get_project_summary"}
        })
    )
    .unwrap();
    let output = server.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let summary: serde_json::Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    for field in ["verification", "is_stale", "deleted_files", "unverified"] {
        assert_eq!(summary["manifest_status"][field], check[field], "{field}");
    }
}
