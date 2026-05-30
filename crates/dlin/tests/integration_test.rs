use std::path::PathBuf;

// We need to reference the library modules — use the binary crate via process for CLI tests,
// but for unit-level integration tests, we'll test the core logic inline.
// For artifact tests, we test the JSON parsing directly.

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn fixture_dir() -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("simple_project")
}

fn binary_path() -> PathBuf {
    let mut path = workspace_root();
    path.push("target");
    path.push("debug");
    path.push("dlin");
    path
}

/// Copy the fixture project into a temp directory and return the temp dir.
fn copy_fixture_to_temp() -> tempfile::TempDir {
    use std::fs;

    let tmp = tempfile::tempdir().unwrap();
    let fixture = fixture_dir();

    fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if src_path.is_dir() {
                copy_dir_recursive(&src_path, &dst_path);
            } else {
                fs::copy(&src_path, &dst_path).unwrap();
            }
        }
    }

    copy_dir_recursive(&fixture, tmp.path());
    tmp
}

mod parsing {
    use super::*;

    #[test]
    fn test_load_project() {
        let dir = fixture_dir();
        let project = dlin_core::parser::project::DbtProject::load(&dir).unwrap();
        assert_eq!(project.name, "simple_project");
    }

    #[test]
    fn test_sql_ref_extraction() {
        let sql = std::fs::read_to_string(fixture_dir().join("models/marts/orders.sql")).unwrap();

        // Check that refs are found using regex
        let ref_re =
            regex::Regex::new(r#"\{\{-?\s*ref\s*\(\s*['"]([^'"]+)['"]\s*\)\s*-?\}\}"#).unwrap();
        let refs: Vec<String> = ref_re
            .captures_iter(&sql)
            .map(|c| c[1].to_string())
            .collect();

        assert_eq!(refs.len(), 2);
        assert!(refs.contains(&"stg_orders".to_string()));
        assert!(refs.contains(&"stg_payments".to_string()));
    }

    #[test]
    fn test_sql_source_extraction() {
        let sql =
            std::fs::read_to_string(fixture_dir().join("models/staging/stg_orders.sql")).unwrap();

        let source_re = regex::Regex::new(
            r#"\{\{-?\s*source\s*\(\s*['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*\)\s*-?\}\}"#,
        )
        .unwrap();

        let sources: Vec<(String, String)> = source_re
            .captures_iter(&sql)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect();

        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0], ("raw".to_string(), "orders".to_string()));
    }

    #[test]
    fn test_yaml_sources_parsing() {
        let content =
            std::fs::read_to_string(fixture_dir().join("models/staging/schema.yml")).unwrap();

        let schema: serde_json::Value = serde_saphyr::from_str(&content).unwrap();
        let sources = schema["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1);

        let tables = sources[0]["tables"].as_array().unwrap();
        assert_eq!(tables.len(), 3);
    }

    #[test]
    fn test_yaml_exposures_parsing() {
        let content =
            std::fs::read_to_string(fixture_dir().join("models/marts/schema.yml")).unwrap();

        let schema: serde_json::Value = serde_saphyr::from_str(&content).unwrap();
        let exposures = schema["exposures"].as_array().unwrap();
        assert_eq!(exposures.len(), 1);
        assert_eq!(exposures[0]["name"].as_str().unwrap(), "weekly_report");
    }
}

mod artifacts {
    use super::*;

    #[test]
    fn test_load_run_results_fixture() {
        let dir = fixture_dir();
        let path = dir.join("target").join("run_results.json");
        let content = std::fs::read_to_string(&path).unwrap();
        let results: serde_json::Value = serde_json::from_str(&content).unwrap();

        let result_list = results["results"].as_array().unwrap();
        assert_eq!(result_list.len(), 5);

        // Check first result
        assert_eq!(
            result_list[0]["unique_id"].as_str().unwrap(),
            "model.simple_project.stg_customers"
        );
        assert_eq!(result_list[0]["status"].as_str().unwrap(), "success");

        // Check error result
        assert_eq!(
            result_list[4]["unique_id"].as_str().unwrap(),
            "model.simple_project.orders"
        );
        assert_eq!(result_list[4]["status"].as_str().unwrap(), "error");
    }

    #[test]
    fn test_run_results_timing_parsing() {
        let json = r#"{
            "results": [{
                "unique_id": "model.proj.test",
                "status": "success",
                "message": "OK",
                "timing": [{
                    "name": "execute",
                    "completed_at": "2025-01-15T10:30:00Z"
                }]
            }]
        }"#;

        let results: serde_json::Value = serde_json::from_str(json).unwrap();
        let timing = results["results"][0]["timing"][0]["completed_at"]
            .as_str()
            .unwrap();
        assert_eq!(timing, "2025-01-15T10:30:00Z");
    }
}

mod freshness {
    use super::*;
    use std::fs;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_check_manifest_up_to_date() {
        let tmp = copy_fixture_to_temp();
        let manifest_path = tmp.path().join("target/manifest.json");

        // Touch manifest to make it newer than all files
        // Sleep briefly to ensure mtime difference
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();

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

        // Touch manifest first
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();

        // Now touch a model file to make it newer
        thread::sleep(Duration::from_millis(50));
        let model_path = tmp.path().join("models/staging/stg_orders.sql");
        fs::write(&model_path, fs::read(&model_path).unwrap()).unwrap();

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

        // Touch manifest to make it newer than all files
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();

        // Delete a model file that's referenced in the manifest
        let model_path = tmp.path().join("models/staging/stg_orders.sql");
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

        // Touch manifest to make it newer
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();

        // Delete a model file
        fs::remove_file(tmp.path().join("models/staging/stg_orders.sql")).unwrap();

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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
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

        // Touch manifest first
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();

        // Delete one file
        fs::remove_file(tmp.path().join("models/staging/stg_orders.sql")).unwrap();

        // Touch another file to make it newer
        thread::sleep(Duration::from_millis(50));
        let model_path = tmp.path().join("models/marts/orders.sql");
        fs::write(&model_path, fs::read(&model_path).unwrap()).unwrap();

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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
        assert_eq!(parsed["is_stale"], true);
        assert!(parsed["stale_file_count"].as_u64().unwrap() > 0);
        assert!(parsed["deleted_file_count"].as_u64().unwrap() > 0);
    }

    #[test]
    fn test_check_manifest_json_up_to_date_has_empty_arrays() {
        let tmp = copy_fixture_to_temp();
        let manifest_path = tmp.path().join("target/manifest.json");

        // Touch manifest to make it newer
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();

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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
        assert_eq!(parsed["is_stale"], false);
        assert_eq!(parsed["stale_file_count"], 0);
        assert_eq!(parsed["stale_files"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["deleted_file_count"], 0);
        assert_eq!(parsed["deleted_files"].as_array().unwrap().len(), 0);
    }
}

mod cli {
    use super::*;
    use std::process::Command;

    #[test]
    fn test_help_flag() {
        let output = Command::new(binary_path())
            .args(["graph", "--help"])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("dlin"));
        assert!(stdout.contains("--project-dir"));
    }

    #[test]
    fn test_no_subcommand_shows_help() {
        let output = Command::new(binary_path())
            .output()
            .expect("Failed to run binary");

        let stderr = String::from_utf8_lossy(&output.stderr);
        // clap prints usage/help to stderr when no subcommand is given
        assert!(
            stderr.contains("Usage") || stderr.contains("dlin"),
            "Should show usage info: {}",
            stderr
        );
    }

    #[test]
    fn test_nonexistent_project() {
        let output = Command::new(binary_path())
            .args(["graph", "--project-dir", "/nonexistent/path"])
            .output()
            .expect("Failed to run binary");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("not found") || stderr.contains("No such file"));
    }

    #[test]
    fn test_run_on_fixture_project() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args(["graph", "--project-dir", fixture.to_str().unwrap()])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Should succeed (exit 0) and produce output
        assert!(output.status.success(), "Failed with stderr: {}", stderr);
        // Should contain some model names in the output
        assert!(
            stdout.contains("stg_orders") || stdout.contains("orders"),
            "Output should contain model names: {}",
            stdout
        );
    }

    #[test]
    fn test_dot_output() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "dot",
                "--node-type",
                "model,source,exposure",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());
        assert!(stdout.contains("digraph"));
        assert!(stdout.contains("rankdir=LR"));
    }

    #[test]
    fn test_focus_model() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "stg_orders",
                "--upstream",
                "1",
                "--downstream",
                "1",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Failed with stderr: {}", stderr);
        assert!(
            stdout.contains("stg_orders"),
            "Output should contain focused model: {}",
            stdout
        );
    }

    #[test]
    fn test_model_not_found() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "nonexistent_model",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            !output.status.success(),
            "Expected non-zero exit code for nonexistent model"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not found") || stderr.contains("nonexistent_model"),
            "Expected error on stderr, got: {}",
            stderr
        );
    }

    #[test]
    fn test_include_seeds() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--node-type",
                "model,source,seed",
                "--output",
                "dot",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());
        assert!(stdout.contains("countries"));
    }

    #[test]
    fn test_ref_resolves_to_seed() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--node-type",
                "model,source,seed",
                "--output",
                "json",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());

        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let edges = json["edges"].as_array().unwrap();

        // customers model refs the countries seed
        let has_seed_edge = edges.iter().any(|e| {
            e["source"].as_str() == Some("seed.countries")
                && e["target"].as_str() == Some("model.customers")
        });
        assert!(
            has_seed_edge,
            "Should have edge from seed 'countries' to model 'customers', edges: {:?}",
            edges
        );
    }

    #[test]
    fn test_macro_ref_tracking() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "json",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());

        let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let edges = json["edges"].as_array().unwrap();

        // order_summary uses macro order_totals() which contains ref('stg_payments')
        let has_macro_edge = edges.iter().any(|e| {
            e["source"].as_str() == Some("model.stg_payments")
                && e["target"].as_str() == Some("model.order_summary")
        });
        assert!(
            has_macro_edge,
            "Should have edge from 'stg_payments' to 'order_summary' via macro, edges: {:?}",
            edges
        );
    }

    #[test]
    fn test_source_manifest_without_path() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--source",
                "manifest",
            ])
            .output()
            .expect("Failed to run binary");

        // Should default to <project-dir>/target/manifest.json
        assert!(
            output.status.success(),
            "Should default to <project-dir>/target/manifest.json: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn test_source_sql_with_manifest_path_errors() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--source",
                "sql",
                "--manifest-path",
                "/some/path",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--manifest-path cannot be used with --source sql"),
            "Should reject --manifest-path with --source sql: {}",
            stderr
        );
    }

    #[test]
    fn test_file_path_as_positional_arg() {
        let fixture = super::fixture_dir();
        let sql_path = fixture.join("models/staging/stg_customers.sql");
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "plain",
                "--upstream",
                "0",
                "--downstream",
                "0",
                sql_path.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Failed with stderr: {}", stderr);
        assert!(
            stdout.contains("stg_customers"),
            "File path should resolve to model name: {}",
            stdout
        );
    }

    #[test]
    fn test_mixed_model_name_and_file_path() {
        let fixture = super::fixture_dir();
        let sql_path = fixture.join("models/staging/stg_customers.sql");
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "plain",
                "--upstream",
                "0",
                "--downstream",
                "0",
                "stg_orders",
                sql_path.to_str().unwrap(),
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Failed with stderr: {}", stderr);
        assert!(
            stdout.contains("stg_orders"),
            "Should contain model name arg: {}",
            stdout
        );
        assert!(
            stdout.contains("stg_customers"),
            "Should contain file-path-resolved model: {}",
            stdout
        );
    }

    #[test]
    fn test_stdin_file_path() {
        let fixture = super::fixture_dir();
        let sql_path = fixture.join("models/staging/stg_customers.sql");
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "plain",
                "--upstream",
                "0",
                "--downstream",
                "0",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        let mut child = output;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{}\n", sql_path.display()).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Failed with stderr: {}", stderr);
        assert!(
            stdout.contains("stg_customers"),
            "Stdin file path should resolve to model: {}",
            stdout
        );
    }

    #[test]
    fn test_stdin_model_name() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "plain",
                "--upstream",
                "0",
                "--downstream",
                "0",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        let mut child = output;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"stg_orders\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "Failed with stderr: {}", stderr);
        assert!(
            stdout.contains("stg_orders"),
            "Stdin model name should be used as focus: {}",
            stdout
        );
    }

    #[test]
    fn test_stdin_ignores_non_dbt_files() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--output",
                "plain",
                "--upstream",
                "0",
                "--downstream",
                "0",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        let mut child = output;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"docs/README.md\nsome/config.toml\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());
        // All inputs were ignored, so with no focus models, full graph is shown
        assert!(
            stdout.contains("stg_orders"),
            "Should show full graph when all stdin lines are ignored: {}",
            stdout
        );
    }

    #[test]
    fn test_include_tests() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--node-type",
                "model,source,test",
                "--output",
                "dot",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());
        assert!(stdout.contains("assert_orders_positive_amount"));
    }

    /// Create a temp fixture where manifest is stale (SQL files are newer).
    fn stale_fixture() -> tempfile::TempDir {
        use std::fs;
        use std::thread;
        use std::time::Duration;

        let tmp = copy_fixture_to_temp();
        let manifest_path = tmp.path().join("target/manifest.json");

        // Touch manifest first, then touch a SQL file to make it newer
        thread::sleep(Duration::from_millis(50));
        fs::write(&manifest_path, fs::read(&manifest_path).unwrap()).unwrap();
        thread::sleep(Duration::from_millis(50));
        let model_path = tmp.path().join("models/staging/stg_orders.sql");
        fs::write(&model_path, fs::read(&model_path).unwrap()).unwrap();

        tmp
    }

    #[test]
    fn test_check_manifest_text_output() {
        let tmp = stale_fixture();
        let output = Command::new(binary_path())
            .args([
                "check-manifest",
                "--project-dir",
                tmp.path().to_str().unwrap(),
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            !output.status.success(),
            "Should exit 1 when manifest is stale"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("stale"), "Should report stale: {}", stdout);
    }

    #[test]
    fn test_check_manifest_json_output() {
        let tmp = stale_fixture();
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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
        assert_eq!(parsed["is_stale"], true);
        assert!(!parsed["stale_files"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_check_manifest_quiet_output() {
        let tmp = stale_fixture();
        let output = Command::new(binary_path())
            .args([
                "check-manifest",
                "--project-dir",
                tmp.path().to_str().unwrap(),
                "-q",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(!output.status.success());
        // Quiet mode should produce no output
        assert!(
            output.stderr.is_empty(),
            "Quiet mode should produce no stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stdout.is_empty(),
            "Quiet mode should produce no stdout"
        );
    }

    #[test]
    fn test_sql_mode_test_warning_with_explicit_node_type() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--node-type",
                "model,test",
                "-o",
                "plain",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(
                "sql mode infers generic tests from YAML declarations; test IDs are dlin-specific"
            ),
            "Expected sql-mode test warning in stderr, got: {stderr}"
        );
    }

    #[test]
    fn test_sql_mode_test_warning_default_node_types() {
        // Warning should also appear when --node-type is not specified (default includes test)
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args(["list", "--project-dir", fixture.to_str().unwrap()])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(
                "sql mode infers generic tests from YAML declarations; test IDs are dlin-specific"
            ),
            "Expected sql-mode test warning even without explicit --node-type, got: {stderr}"
        );
    }

    #[test]
    fn test_sql_mode_test_warning_suppressed_by_quiet() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "list",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--quiet",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(
                "sql mode infers generic tests from YAML declarations; test IDs are dlin-specific"
            ),
            "Warning should be suppressed by --quiet, got: {stderr}"
        );
    }

    #[test]
    fn test_sql_mode_test_warning_absent_without_test_type() {
        // When --node-type excludes test, no warning should appear
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "graph",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--node-type",
                "model,source",
                "-o",
                "plain",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(
                "sql mode infers generic tests from YAML declarations; test IDs are dlin-specific"
            ),
            "Warning should not appear when test type is excluded, got: {stderr}"
        );
    }

    #[test]
    fn test_sql_mode_test_warning_absent_in_manifest_mode() {
        let fixture = super::fixture_dir();
        let output = Command::new(binary_path())
            .args([
                "list",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--source",
                "manifest",
                "--node-type",
                "test",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(
                "sql mode infers generic tests from YAML declarations; test IDs are dlin-specific"
            ),
            "Warning should not appear in manifest mode, got: {stderr}"
        );
    }
}

mod error_format {
    use super::*;

    #[test]
    fn test_error_format_json_on_error() {
        // Run impact on a nonexistent project to trigger an error
        let output = std::process::Command::new(binary_path())
            .args([
                "--error-format",
                "json",
                "impact",
                "nonexistent",
                "-p",
                "/nonexistent_project_dir",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        let parsed: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap_or_else(|e| {
            panic!("stderr is not valid JSON: {e}\nstderr: {stderr}");
        });
        assert_eq!(parsed["level"], "error");
        assert!(!parsed["what"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_error_format_text_on_error() {
        let output = std::process::Command::new(binary_path())
            .args([
                "--error-format",
                "text",
                "impact",
                "nonexistent",
                "-p",
                "/nonexistent_project_dir",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with("Error: "),
            "Expected 'Error: ' prefix, got: {stderr}"
        );
    }

    #[test]
    fn test_error_format_json_warning() {
        // Use a valid project with an unknown node type to trigger a warning
        let output = std::process::Command::new(binary_path())
            .args([
                "--error-format",
                "json",
                "graph",
                "-p",
                fixture_dir().to_str().unwrap(),
                "--node-type",
                "bogus_type",
                "-o",
                "plain",
            ])
            .output()
            .expect("Failed to run binary");

        let stderr = String::from_utf8_lossy(&output.stderr);
        // stderr may contain multiple lines; find the JSON warning
        let warning_line = stderr.lines().find(|l| l.contains("\"warning\""));
        assert!(
            warning_line.is_some(),
            "Expected JSON warning in stderr, got: {stderr}"
        );
        let parsed: serde_json::Value = serde_json::from_str(warning_line.unwrap()).unwrap();
        assert_eq!(parsed["level"], "warning");
        assert!(parsed["what"].as_str().unwrap().contains("bogus_type"));
    }

    #[test]
    fn test_error_format_default_is_text() {
        // Without --error-format, should use text format
        let output = std::process::Command::new(binary_path())
            .args(["impact", "nonexistent", "-p", "/nonexistent_project_dir"])
            .output()
            .expect("Failed to run binary");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.starts_with("Error: "),
            "Expected text error, got: {stderr}"
        );
    }
}

mod sql_mode_test_warning {
    use super::*;

    const WARNING_NEEDLE: &str = "sql mode infers generic tests";

    #[test]
    fn test_impact_warns_when_tests_affected() {
        let output = std::process::Command::new(binary_path())
            .args([
                "impact",
                "stg_orders",
                "-p",
                fixture_dir().to_str().unwrap(),
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(WARNING_NEEDLE),
            "Expected sql-mode test warning in stderr, got: {stderr}"
        );
    }

    #[test]
    fn test_impact_no_warning_when_no_tests_affected() {
        // `customers` has only the exposure downstream, no tests.
        let output = std::process::Command::new(binary_path())
            .args(["impact", "customers", "-p", fixture_dir().to_str().unwrap()])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(WARNING_NEEDLE),
            "Unexpected warning when no tests affected: {stderr}"
        );
    }

    #[test]
    fn test_impact_deduplicates_repeated_model_names() {
        // When the same model name appears multiple times, the output must contain
        // exactly one impact report per unique model name.
        let output = std::process::Command::new(binary_path())
            .args([
                "impact",
                "customers",
                "customers",
                "-p",
                fixture_dir().to_str().unwrap(),
                "-o",
                "json",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            output.status.success(),
            "impact with duplicate model names should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            reports.len(),
            1,
            "duplicate model name should produce exactly one impact report; got: {:?}",
            reports
        );
        assert_eq!(reports[0]["source_model"], "customers");
    }

    #[test]
    fn test_graph_warns_when_output_contains_tests() {
        // Default node types include test, so the warning should appear.
        let output = std::process::Command::new(binary_path())
            .args([
                "graph",
                "-p",
                fixture_dir().to_str().unwrap(),
                "-o",
                "plain",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(WARNING_NEEDLE),
            "Expected sql-mode test warning for graph default output, got: {stderr}"
        );
    }

    #[test]
    fn test_graph_no_warning_when_tests_excluded() {
        // Explicitly request only model nodes — no tests in output.
        let output = std::process::Command::new(binary_path())
            .args([
                "graph",
                "-p",
                fixture_dir().to_str().unwrap(),
                "-o",
                "plain",
                "--node-type",
                "model",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(WARNING_NEEDLE),
            "Unexpected warning when tests excluded via --node-type: {stderr}"
        );
    }

    #[test]
    fn test_list_warns_when_output_contains_tests() {
        let output = std::process::Command::new(binary_path())
            .args(["list", "-p", fixture_dir().to_str().unwrap()])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(WARNING_NEEDLE),
            "Expected sql-mode test warning for list default output, got: {stderr}"
        );
    }

    #[test]
    fn test_list_no_warning_when_tests_excluded() {
        let output = std::process::Command::new(binary_path())
            .args([
                "list",
                "-p",
                fixture_dir().to_str().unwrap(),
                "--node-type",
                "model",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains(WARNING_NEEDLE),
            "Unexpected warning when tests excluded via --node-type: {stderr}"
        );
    }
}

mod generic_test_metadata {
    use super::*;

    /// Create a temp project with a model and generic YAML tests.
    fn setup_generic_test_project() -> tempfile::TempDir {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        fs::create_dir_all(dir.join("models")).unwrap();
        fs::write(dir.join("dbt_project.yml"), "name: test_proj\n").unwrap();
        fs::write(dir.join("models/orders.sql"), "SELECT 1 AS order_id").unwrap();
        fs::write(
            dir.join("models/schema.yml"),
            r#"
models:
  - name: orders
    columns:
      - name: order_id
        data_tests:
          - not_null
          - unique
"#,
        )
        .unwrap();

        tmp
    }

    #[test]
    fn test_generic_test_has_yaml_file_path() {
        let tmp = setup_generic_test_project();
        let output = std::process::Command::new(binary_path())
            .args([
                "list",
                "-p",
                tmp.path().to_str().unwrap(),
                "-o",
                "json",
                "--node-type",
                "test",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let nodes: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(nodes.len(), 2);

        for node in &nodes {
            assert_eq!(
                node["file_path"].as_str(),
                Some("models/schema.yml"),
                "Generic test should have YAML file_path, got: {}",
                node
            );
        }
    }

    #[test]
    fn test_generic_test_sql_content_is_null() {
        let tmp = setup_generic_test_project();
        let output = std::process::Command::new(binary_path())
            .args([
                "list",
                "-p",
                tmp.path().to_str().unwrap(),
                "-o",
                "json",
                "--json-full",
                "--node-type",
                "test",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        let nodes: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(nodes.len(), 2);

        for node in &nodes {
            assert!(
                node["sql_content"].is_null(),
                "Generic test sql_content should be null (not YAML), got: {}",
                node["sql_content"]
            );
        }
    }

    fn column_lineage_fixture_dir() -> std::path::PathBuf {
        workspace_root()
            .join("tests")
            .join("fixtures")
            .join("column_lineage_project")
    }

    #[test]
    fn test_column_lineage_column_filter_updates_counts() {
        // When --column filter is applied, traced_columns and total_columns must
        // reflect the filtered set, not the full model's counts.
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_orders",
                "--column",
                "order_id",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            output.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(reports.len(), 1);

        let report = &reports[0];
        // Only the requested column should be present
        let columns = report["columns"].as_array().unwrap();
        assert_eq!(columns.len(), 1, "only order_id should be in columns[]");
        assert_eq!(columns[0]["column"], "order_id");
        // Counts must reflect the filtered set
        assert_eq!(
            report["traced_columns"], 1,
            "traced_columns should be 1 (filtered to 1 column)"
        );
        assert_eq!(
            report["total_columns"], 1,
            "total_columns should be 1 (requested 1 column)"
        );
    }

    #[test]
    fn test_column_lineage_column_filter_preserves_zero_counts_on_error() {
        // When the model cannot be loaded (e.g. not found), total_columns is 0.
        // Applying --column should NOT overwrite it with the filter size.
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "nonexistent_model",
                "--column",
                "some_col",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        // Exit code is 1 (error) but JSON should still be emitted
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(reports.len(), 1);

        let report = &reports[0];
        // Model not found → no analysis ran → counts must stay 0
        assert_eq!(
            report["traced_columns"], 0,
            "traced_columns should remain 0 for a missing model"
        );
        assert_eq!(
            report["total_columns"], 0,
            "total_columns should remain 0 for a missing model, not overwritten by filter size"
        );
        // columns[] must be empty
        assert!(report["columns"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_column_lineage_column_filter_suppresses_unrelated_errors() {
        // stg_partial_fail declares "ghost_col" in YAML but the SQL only outputs
        // "order_id". Without a filter both columns are attempted; ghost_col fails.
        // With --column order_id, ghost_col's error must be filtered out so that
        // the exit code is 0 and errors[] is empty.
        let fixture = column_lineage_fixture_dir();

        // First confirm that without a filter, ghost_col causes a non-zero exit.
        let unfiltered = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_partial_fail",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");
        assert!(
            !unfiltered.status.success(),
            "expected non-zero exit when ghost_col fails"
        );
        let unfiltered_json: Vec<serde_json::Value> =
            serde_json::from_str(&String::from_utf8_lossy(&unfiltered.stdout)).unwrap();
        assert!(
            !unfiltered_json[0]["errors"].as_array().unwrap().is_empty(),
            "expected errors[] to be non-empty without filter"
        );

        // Now with --column order_id: ghost_col's error must be suppressed.
        let filtered = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_partial_fail",
                "--column",
                "order_id",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");
        assert!(
            filtered.status.success(),
            "expected zero exit when only the successful column is requested; stderr: {}",
            String::from_utf8_lossy(&filtered.stderr)
        );
        let reports: Vec<serde_json::Value> =
            serde_json::from_str(&String::from_utf8_lossy(&filtered.stdout)).unwrap();
        let report = &reports[0];
        assert_eq!(report["traced_columns"], 1);
        assert_eq!(report["total_columns"], 1);
        assert_eq!(report["columns"].as_array().unwrap().len(), 1);
        assert!(
            report["errors"].as_array().unwrap().is_empty(),
            "ghost_col's error must not appear after --column order_id filter; errors: {:?}",
            report["errors"]
        );
    }

    #[test]
    fn test_column_lineage_column_filter_errors_on_missing_column() {
        // When --column requests a column that does not exist in any of the model's
        // output columns, the result should be non-zero exit with an error message.
        // Previously this yielded empty columns[], empty errors[], and exit 0.
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_orders",
                "--column",
                "this_column_does_not_exist",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            !output.status.success(),
            "expected non-zero exit when requested column is absent from the model"
        );
        let reports: Vec<serde_json::Value> =
            serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        let report = &reports[0];
        assert!(
            !report["errors"].as_array().unwrap().is_empty(),
            "errors[] must be non-empty when the requested column is missing"
        );
        assert_eq!(
            report["traced_columns"], 0,
            "traced_columns must be 0 for a missing column"
        );
        assert_eq!(
            report["total_columns"], 1,
            "total_columns must equal the filter size (1 requested column)"
        );
    }

    #[test]
    fn test_column_lineage_column_filter_preserves_global_parse_error() {
        // stg_bad_sql has valid YAML columns but invalid SQL, so total_columns > 0
        // and analysis returns a "failed to parse SQL" global error. Applying
        // --column must NOT drop that error — it is not a per-column error.
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_bad_sql",
                "--column",
                "some_col",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        // Exit code must be non-zero because the parse error propagates.
        assert!(
            !output.status.success(),
            "expected non-zero exit for a model with invalid SQL"
        );
        let reports: Vec<serde_json::Value> =
            serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
        let report = &reports[0];
        let errors = report["errors"].as_array().unwrap();
        assert!(
            !errors.is_empty(),
            "global parse error must not be dropped by --column filter"
        );
        assert!(
            errors
                .iter()
                .any(|e| e["kind"].as_str() == Some("parse_failure")),
            "expected a parse_failure error; got: {:?}",
            errors
        );
    }

    #[test]
    fn test_column_upstream_stdin_model_name() {
        // Model name provided via stdin (no positional args) should work the same as
        // providing it as a positional argument.
        let fixture = column_lineage_fixture_dir();
        let mut child = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"stg_orders\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(
            output.status.success(),
            "column upstream via stdin model name should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["model"], "stg_orders");
    }

    #[test]
    fn test_column_upstream_stdin_file_path() {
        // SQL file path provided via stdin should be resolved to the model name.
        let fixture = column_lineage_fixture_dir();
        let sql_path = fixture.join("models/staging/stg_orders.sql");
        let mut child = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{}\n", sql_path.display()).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();

        assert!(
            output.status.success(),
            "column upstream via stdin file path should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0]["model"], "stg_orders");
    }

    #[test]
    fn test_column_upstream_stdin_yaml_sources_filtered() {
        // When a YAML file containing sources: is piped (e.g. from `git diff`), the
        // source names must be silently dropped; only model names should be analyzed.
        let fixture = column_lineage_fixture_dir();

        // Build a temp project mirroring the fixture so the YAML file sits under the
        // default model-paths (models/) and is recognised by classify_line.
        let tmp_dir = tempfile::tempdir().unwrap();
        std::fs::copy(
            fixture.join("dbt_project.yml"),
            tmp_dir.path().join("dbt_project.yml"),
        )
        .unwrap();
        std::fs::create_dir_all(tmp_dir.path().join("target")).unwrap();
        std::fs::copy(
            fixture.join("target/manifest.json"),
            tmp_dir.path().join("target/manifest.json"),
        )
        .unwrap();
        std::fs::create_dir_all(tmp_dir.path().join("models/staging")).unwrap();
        let yaml_path = tmp_dir.path().join("models/staging/schema.yml");
        std::fs::write(
            &yaml_path,
            "sources:\n  - name: raw\n    tables:\n      - name: orders\nmodels:\n  - name: stg_orders\n",
        )
        .unwrap();

        let mut child = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "--project-dir",
                tmp_dir.path().to_str().unwrap(),
                "--no-cache",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{}\n", yaml_path.display()).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();

        // Should succeed: raw.orders (source) is filtered out, stg_orders (model) is kept.
        assert!(
            output.status.success(),
            "column upstream with YAML containing sources should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            reports.len(),
            1,
            "only stg_orders (model) should be analyzed"
        );
        assert_eq!(reports[0]["model"], "stg_orders");
    }

    #[test]
    fn test_column_upstream_no_args_no_stdin_fails() {
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            !output.status.success(),
            "column upstream with no models should fail"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("no model names provided"),
            "error message should mention 'no model names provided'; got: {}",
            stderr
        );
    }

    #[test]
    fn test_column_upstream_stdin_mixed_path_and_bare_source_name() {
        // When a file path AND a bare non-model name (source) are piped together, the bare
        // name must NOT be silently dropped by the model-only filter.  Before the raw_input_set
        // fix, raw.orders would be dropped and the command would succeed with only stg_orders;
        // after the fix it should pass through and produce an analysis error, causing exit 1.
        let fixture = column_lineage_fixture_dir();
        let sql_path = fixture.join("models/staging/stg_orders.sql");
        let mut child = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
                "-o",
                "json",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("Failed to spawn binary");

        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{}\nraw.orders\n", sql_path.display()).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();

        // raw.orders is a source, not a model — analysis should fail (exit 1).
        assert!(
            !output.status.success(),
            "column upstream should fail when a bare source name is piped; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        // stg_orders must still be present (the SQL path was resolved correctly).
        assert!(
            reports.iter().any(|r| r["model"] == "stg_orders"),
            "stg_orders report should be present; got: {}",
            stdout
        );
        // raw.orders must appear in the output rather than being silently dropped.
        assert!(
            reports.iter().any(|r| r["model"] == "raw.orders"),
            "raw.orders report should be present (not silently dropped); got: {}",
            stdout
        );
    }

    #[test]
    fn test_column_lineage_source_physical_schema_differs_from_source_name() {
        // Regression test: when source_name != physical schema (e.g. source_name="salesforce",
        // schema="raw"), dbt compiled SQL uses the physical schema ("raw"."accounts"), not
        // "salesforce"."accounts". The schema registration must use the physical schema so
        // that SELECT * expansion works correctly.
        //
        // The fixture has:
        //   source salesforce.accounts with schema: raw (physical)
        //   stg_accounts model with compiled SQL: SELECT ... FROM "raw"."accounts"
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_accounts",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            output.status.success(),
            "column upstream for stg_accounts should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(reports.len(), 1);

        let report = &reports[0];
        assert_eq!(report["model"], "stg_accounts");

        // All 3 YAML columns should be traced successfully
        let columns = report["columns"].as_array().unwrap();
        assert_eq!(
            columns.len(),
            3,
            "stg_accounts should have 3 traced columns, got: {:?}; errors: {:?}",
            columns.iter().map(|c| &c["column"]).collect::<Vec<_>>(),
            report["errors"]
        );

        // account_id traces from raw.accounts.id (via SELECT * expansion)
        let account_id = columns
            .iter()
            .find(|c| c["column"] == "account_id")
            .expect("account_id column should be present");
        let sources = account_id["sources"].as_array().unwrap();
        assert!(
            !sources.is_empty(),
            "account_id should have sources after physical schema registration"
        );
        assert!(
            sources.iter().any(|s| s["column"] == "id"),
            "account_id should trace to source column 'id'; got: {:?}",
            sources
        );

        // No errors — the physical schema registration enables SELECT * expansion
        assert!(
            report["errors"].as_array().unwrap().is_empty(),
            "should have no errors for stg_accounts; got: {:?}",
            report["errors"]
        );
    }

    #[test]
    fn test_column_upstream_deduplicates_repeated_model_names() {
        // When the same model name appears multiple times (CLI args or stdin),
        // the output must contain exactly one result object per unique model name.
        let fixture = column_lineage_fixture_dir();
        let output = std::process::Command::new(binary_path())
            .args([
                "column",
                "upstream",
                "stg_orders",
                "stg_orders",
                "--project-dir",
                fixture.to_str().unwrap(),
                "--no-cache",
            ])
            .output()
            .expect("Failed to run binary");

        assert!(
            output.status.success(),
            "column upstream with duplicate model names should succeed; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            reports.len(),
            1,
            "duplicate model name should produce exactly one result; got: {:?}",
            reports
        );
        assert_eq!(reports[0]["model"], "stg_orders");
    }
}

mod manifest_only_mode {
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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
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
            !stderr.is_empty(),
            "stderr should contain a parse error message"
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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
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
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).expect("Should be valid JSON");
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
  "metadata": {},
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
}
