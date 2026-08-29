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
