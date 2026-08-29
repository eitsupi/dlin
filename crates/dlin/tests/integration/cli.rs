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
    let tmp = copy_fixture_to_temp();
    let manifest_path = tmp.path().join("target/manifest.json");
    let model_path = tmp.path().join("models/staging/stg_orders.sql");

    // Touch manifest first, then touch a SQL file to make it newer
    set_mtime_newer_than(&manifest_path, &model_path);
    set_mtime_newer_than(&model_path, &manifest_path);

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
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("Should be valid JSON");
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
