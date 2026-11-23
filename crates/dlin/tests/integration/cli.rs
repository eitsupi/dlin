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

/// The long help of a command. The environment is cleared, since clap shows
/// the values of the environment variables that back options.
fn long_help(args: &[&str]) -> String {
    let output = Command::new(binary_path())
        .env_clear()
        .args(args)
        .arg("--help")
        .output()
        .expect("Failed to run binary");
    assert!(output.status.success(), "{args:?}");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The help of every command is part of the interface that users and agents
/// read, including its known limitations, so each one is a snapshot.
#[test]
fn help_of_every_command() {
    for args in [
        &[][..],
        &["graph"],
        &["list"],
        &["impact"],
        &["summary"],
        &["column"],
        &["column", "upstream"],
        &["column", "downstream"],
        &["debug"],
        &["debug", "parse-sql"],
        &["debug", "trace-column"],
        &["check-manifest"],
        &["mcp"],
    ] {
        let name = std::iter::once("dlin")
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join("_")
            .replace('-', "_");
        insta::assert_snapshot!(name, long_help(args));
    }
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

#[test]
fn debug_trace_column_preserves_exact_quoted_output_identity() {
    for (column, source, ordinal) in [("id", "a", 0), ("ID", "b", 1)] {
        let output = Command::new(binary_path())
            .args([
                "debug",
                "trace-column",
                r#"SELECT a AS id, b AS "ID" FROM t"#,
                "--column",
                column,
                "--dialect",
                "postgresql",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["column"], column);
        assert_eq!(json["mappings"].as_array().unwrap().len(), 1);
        assert_eq!(json["mappings"][0]["ordinal"], ordinal);
        assert_eq!(
            json["mappings"][0]["lineage"]["Complete"][0]["column"]["value"],
            source
        );
    }
}

#[test]
fn debug_trace_column_shows_ambiguous_candidates_and_reports_dialect_misses() {
    for (sql, column, reason) in [
        (
            r#"SELECT a AS id, b AS "id" FROM t"#,
            "id",
            Some("distinct SQL identifiers share the same display name"),
        ),
        (
            "SELECT a AS Foo, b AS foo FROM t",
            "Foo",
            Some("more than one sqllineage mapping"),
        ),
        (r#"SELECT a AS "MiXeD" FROM t"#, "mixed", None),
        ("SELECT * FROM t", "missing", None),
    ] {
        let output = Command::new(binary_path())
            .args([
                "debug",
                "trace-column",
                sql,
                "--column",
                column,
                "--dialect",
                "snowflake",
            ])
            .output()
            .unwrap();
        if let Some(reason) = reason {
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(json["column"], column);
            assert_eq!(json["ambiguous"], true);
            assert!(json["ambiguity_reason"].as_str().unwrap().contains(reason));
            let mappings = json["mappings"].as_array().unwrap();
            assert_eq!(mappings.len(), 2);
            for (ordinal, source) in ["a", "b"].into_iter().enumerate() {
                assert_eq!(mappings[ordinal]["ordinal"], ordinal);
                assert_eq!(
                    mappings[ordinal]["lineage"]["Complete"][0]["column"]["value"],
                    source
                );
            }
        } else {
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains(&format!("no mapping for column '{column}'")),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn ambiguous_name_fails_graph_list_and_impact_even_with_an_exact_input() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{
            "metadata": {},
            "nodes": {
                "model.a.orders": {"unique_id":"model.a.orders","name":"orders","resource_type":"model"},
                "model.b.orders": {"unique_id":"model.b.orders","name":"orders","resource_type":"model"}
            }
        }"#,
    )
    .unwrap();

    for command in ["graph", "list", "impact"] {
        let output = Command::new(binary_path())
            .args([
                command,
                "model.a.orders",
                "orders",
                "--source",
                "manifest",
                "--no-cache",
                "--manifest-path",
            ])
            .arg(&manifest)
            .current_dir(tmp.path())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{command}");
        assert!(output.stdout.is_empty(), "{command}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(
                "ambiguous node name 'orders': model.a.orders, model.b.orders. Specify a unique_id."
            ),
            "{command}: {:?}",
            output.stderr
        );
    }
}

#[test]
fn sql_path_selects_the_exact_node_across_cli_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = tmp.path().join("manifest.json");
    std::fs::write(
        &manifest,
        r#"{
          "metadata":{"adapter_type":"duckdb"},
          "nodes":{
            "model.b.orders":{"unique_id":"model.b.orders","name":"orders","resource_type":"model","original_file_path":"models/b/orders.sql","compiled_code":"select 2 as id"},
            "model.a.orders":{"unique_id":"model.a.orders","name":"orders","resource_type":"model","original_file_path":"models/a/orders.sql","compiled_code":"select 1 as id"},
            "model.a.customers":{"unique_id":"model.a.customers","name":"customers","resource_type":"model","original_file_path":"models/customers.sql","compiled_code":"select 1 as id"},
            "snapshot.a.history":{"unique_id":"snapshot.a.history","name":"history","resource_type":"snapshot","original_file_path":"snapshots/history.sql","compiled_code":"select 1 as id"},
            "snapshot.a.shared_b":{"unique_id":"snapshot.a.shared_b","name":"shared_b","resource_type":"snapshot","original_file_path":"snapshots/shared.sql"},
            "snapshot.a.shared_a":{"unique_id":"snapshot.a.shared_a","name":"shared_a","resource_type":"snapshot","original_file_path":"snapshots/shared.sql"}
          }
        }"#,
    )
    .unwrap();
    let run = |command: &[&str], inputs: &[&str]| {
        let mut cmd = Command::new(binary_path());
        cmd.args(command).args(inputs);
        if command[0] != "column" {
            cmd.args(["--source", "manifest"]);
        }
        cmd.args([
            "--manifest-path",
            "manifest.json",
            "--no-cache",
            "-o",
            "json",
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap()
    };
    let json = |command: &[&str], inputs: &[&str]| {
        let output = run(command, inputs);
        assert!(
            output.status.success(),
            "{command:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let path = "models/a/orders.sql";
    for (command, key, label) in [
        (&["graph"][..], "/nodes/0", "/nodes/0/label"),
        (&["list"][..], "/0", "/0/label"),
        (&["impact"][..], "/0/source_node", "/0/source_model"),
        (&["column", "upstream"][..], "/0", "/0/model"),
    ] {
        let result = json(command, &[path]);
        let node = result.pointer(key).unwrap();
        assert_eq!(node["unique_id"], "model.a.orders", "{command:?}");
        assert_eq!(result.pointer(label).unwrap(), "orders", "{command:?}");

        let output = run(command, &[path, "orders"]);
        assert!(!output.status.success(), "{command:?}");
        assert!(output.stdout.is_empty(), "{command:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("ambiguous node name 'orders': model.a.orders, model.b.orders"),
            "{command:?}"
        );
    }

    let output = run(&["graph"], &["snapshots/shared.sql"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(
        "ambiguous node path 'snapshots/shared.sql': snapshot.a.shared_a, snapshot.a.shared_b. Specify a unique_id."
    ));

    for command in [&["impact"][..], &["column", "upstream"][..]] {
        let result = json(command, &["models/customers.sql", "customers"]);
        assert_eq!(result.as_array().unwrap().len(), 1, "{command:?}");
    }

    let result = json(&["column", "upstream"], &[path, "snapshots/history.sql"]);
    assert_eq!(result.as_array().unwrap().len(), 1);
    assert_eq!(result[0]["unique_id"], "model.a.orders");

    let output = run(
        &["column", "upstream"],
        &[path, "snapshot.a.history", "missing"],
    );
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result.as_array().unwrap().len(), 3);
    assert_eq!(
        result[1]["errors"][0]["kind"],
        "unsupported_materialization"
    );
    assert_eq!(result[2]["errors"][0]["kind"], "model_not_found");
}
