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

mod cli {
    use std::process::Command;

    fn binary_path() -> std::path::PathBuf {
        // The built binary path
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("target");
        path.push("debug");
        path.push("dlin");
        path
    }

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
        assert!(stdout.contains("--interactive"));
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
                "--include-exposures",
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

        assert!(output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("not found") || stderr.contains("nonexistent_model"),
            "Expected warning on stderr, got: {}",
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
                "--include-seeds",
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
                "--include-seeds",
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

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("--manifest-path is required"),
            "Should require --manifest-path: {}",
            stderr
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
                "--include-tests",
                "--output",
                "dot",
            ])
            .output()
            .expect("Failed to run binary");

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success());
        assert!(stdout.contains("assert_orders_positive_amount"));
    }
}
