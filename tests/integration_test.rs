use std::path::PathBuf;

// We need to reference the library modules — use the binary crate via process for CLI tests,
// but for unit-level integration tests, we'll test the core logic inline.
// For artifact tests, we test the JSON parsing directly.

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("simple_project")
}

fn binary_path() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
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
        let project = dlin::parser::project::DbtProject::load(&dir).unwrap();
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

mod impact_warning {
    use super::*;

    #[test]
    fn test_impact_shows_sql_mode_test_warning() {
        // simple_project has a singular test, so impact on stg_orders should
        // emit the sql-mode test limitation warning on stderr.
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
            stderr.contains("sql mode infers generic tests"),
            "Expected sql-mode test warning in stderr, got: {stderr}"
        );
    }

    #[test]
    fn test_impact_no_warning_when_no_tests_affected() {
        // Impact on a leaf model with no downstream tests should not warn.
        // `customers` is a mart model; its only downstream is the exposure
        // `weekly_report`, so no tests are affected.
        let output = std::process::Command::new(binary_path())
            .args([
                "impact",
                "customers",
                "-p",
                fixture_dir().to_str().unwrap(),
            ])
            .output()
            .expect("Failed to run binary");

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("sql mode infers generic tests"),
            "Unexpected sql-mode test warning when no tests affected: {stderr}"
        );
    }
}
