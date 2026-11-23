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

fn long_help(args: &[&str]) -> String {
    let output = Command::new(binary_path())
        .args(args)
        .arg("--help")
        .output()
        .expect("Failed to run binary");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The section of a help text that starts at the line `heading`, up to the
/// next unindented line.
fn help_section(help: &str, heading: &str) -> String {
    let mut lines = help.lines().skip_while(|line| *line != heading);
    let mut section = vec![
        lines
            .next()
            .unwrap_or_else(|| panic!("no {heading:?} in:\n{help}")),
    ];
    section.extend(lines.take_while(|line| line.is_empty() || line.starts_with(' ')));
    section.join("\n").trim_end().to_string()
}

#[test]
fn column_help_describes_the_command_and_its_limitations() {
    let help = long_help(&["column"]);
    let description = help.split("\nUsage:").next().unwrap().trim_end();
    insta::assert_snapshot!(description, @r#"
    Column-level lineage and impact analysis.

    Unlike `dlin graph` (model-level, bidirectional, with -u/-d depth control),
    column analysis is split by direction — each subcommand covers one direction:

      upstream    — traces where each output column's data came from
      downstream  — finds which models/columns are affected by a column change

    There are no -u/-d depth flags; the full chain is always traversed in both cases.

    Unlike `dlin graph`/`dlin impact`, there is no --source flag — manifest.json is the only
    data source for column-level analysis.

    Both subcommands require manifest.json with compiled SQL (run `dbt compile` first).
    Use `dlin check-manifest` to verify freshness before querying.

    Limitations:
      - Value dependencies only: columns used only in filters and join
        conditions (row dependencies) are not collected.
      - Unknown columns behind SELECT *, unsupported SQL and ambiguous names
        leave lineage partial.
      - A STRUCT field can be traced to the inputs of the whole STRUCT, so a
        complete result can include the inputs of sibling fields. This happens
        across dbt model boundaries, and through CAST, CASE, set operations,
        array subscripts or UNNEST of an array built elsewhere.
      - A YAML column list used as a schema is treated as exhaustive. Results
        that rely on it cite it in evidence[] and show an "assumes" note.
    "#);
}

#[test]
fn column_subcommands_state_the_limitations_and_where_to_check_them() {
    // A reader of either subcommand's help alone must learn what a result
    // cannot show, and where to check it.
    let upstream = long_help(&["column", "upstream"]);
    insta::assert_snapshot!(help_section(&upstream, "Limitations:"), @r#"
    Limitations:
      - Value dependencies only: columns used only in filters and join
        conditions (row dependencies) are not collected.
      - Unknown columns behind SELECT *, unsupported SQL and ambiguous names
        leave lineage partial.
      - A STRUCT field can be traced to the inputs of the whole STRUCT, so a
        complete result can include the inputs of sibling fields. This happens
        across dbt model boundaries, and through CAST, CASE, set operations,
        array subscripts or UNNEST of an array built elsewhere.
      - A YAML column list used as a schema is treated as exhaustive. Results
        that rely on it cite it in evidence[] and show an "assumes" note.
      In JSON, check output.columns[].lineage.status and lineage.unresolved[],
      output.shape for unexpanded stars, boundaries[] for where tracing stopped,
      and errors[] for diagnostics.
    "#);
    insta::assert_snapshot!(help_section(&upstream, "Stdin/pipe support:"), @"
    Stdin/pipe support:
      Accepts model names or file paths on stdin (one per line).
      File paths (detected by extension or path separator) are resolved to
      model names using the dbt project configuration. Without dbt_project.yml,
      path inputs use the standard dbt layout; use model names with a standalone
      manifest from a project with custom paths.
    ");

    let downstream = long_help(&["column", "downstream"]);
    insta::assert_snapshot!(help_section(&downstream, "Limitations:"), @r#"
    Limitations:
      - Value dependencies only: columns used only in filters and join
        conditions (row dependencies) are not collected.
      - Unknown columns behind SELECT *, unsupported SQL and ambiguous names
        leave lineage partial.
      - A STRUCT field can be traced to the inputs of the whole STRUCT, so a
        complete result can include the inputs of sibling fields. This happens
        across dbt model boundaries, and through CAST, CASE, set operations,
        array subscripts or UNNEST of an array built elsewhere.
      - A YAML column list used as a schema is treated as exhaustive. Results
        that rely on it cite it in evidence[] and show an "assumes" note.
      In JSON, possible[] lists unconfirmed targets, exploration[] lists gaps
      that may hide further targets, and errors[] holds diagnostics.
    "#);
    insta::assert_snapshot!(help_section(&downstream, "Output format (-o/--output):"), @r#"
    Output format (-o/--output):
      json (default)  JSON array per column:
        model, column     the analysed input
        input             the model output that the column names, or absent when it
                          names none or several (the exploration then has an
                          "unidentified_input" gap; dependents that name the
                          column are still searched by that name, and their paths
                          start at an "unidentified_column" that does not stand
                          for one particular output of the model)
        scope             "value_only"
        confirmed[]       downstream columns that depend on the input through known
                          edges (target, kind, transformation, path of edges).
                          evidence[] (omitted if empty) lists the schema evidence
                          ids the path relies on
        possible[]        concrete downstream targets that may depend on the input
                          through finite unresolved paths; paths remain separate.
                          evidence[] as for confirmed[]
        exploration[]     per dependency kind, "complete" or "incomplete" with the
                          gaps (unresolved lineage, failed analysis, a dependent
                          model with unlisted columns, ...) that may hide
                          further impacts. "no impact" is shown only when there
                          are no confirmed or possible targets and every requested
                          dependency kind is complete; it relies on the
                          evidence[] of every exploration.
        evidence[]        the schema evidence the report cites, described as for
                          `dlin column upstream`
        errors[]          diagnostics of the input column and of the models the
                          search could not analyse
      plain           human-readable text with confirmed and possible counts, scope,
                      per-kind gaps and concrete paths. An "assumes" line names
                      each assumed column list and the conclusions that rely on it
      mermaid         Mermaid flowchart (LR) showing impacted columns across models,
                      with gap and assumption notes
      dot             Graphviz DOT format; models as clusters, columns as nodes
                      color-coded by transformation type (pipe to `dot -Tsvg`)
    "#);
}

#[test]
fn top_level_help_states_the_data_sources_and_their_limitations() {
    insta::assert_snapshot!(help_section(&long_help(&[]), "Data sources:"), @"
    Data sources:
      sql (default)   Interpret the Jinja in SQL files. No Python or dbt required.
                      Detects ref() and source() calls in SQL, plus exposures from YAML.
                      var() uses vars.yml or dbt_project.yml, then its default; CLI
                      --vars overrides are not supported. env_var() uses its default and
                      never reads the environment, so a different run-time value can
                      select another dependency or branch. Values known only at run time
                      (target, env_var() without a default, run_query() results) do not
                      select dependencies; execute is false, as in dbt parsing.
                      Dependencies selected through adapter.dispatch() or namespaced
                      package macros (e.g. dbt_utils.*) may be missed; the nodes
                      concerned are reported as partial (see dependency_analysis).
                      Generic tests (not_null, unique, etc.) are inferred from YAML declarations
                      with dlin-specific IDs — use manifest mode for exact dependency resolution.
      manifest        Read a pre-compiled manifest.json, the dependency graph recorded by
                      dbt. Requires `dbt compile` (or `dbt run`/`dbt build`) to have been
                      run first. Use `dlin check-manifest` to verify freshness before querying.

      Use sql mode for quick, local exploration without dbt setup. For exact dbt test
      IDs, materializations or run-time dependency choices, use a manifest compiled for
      the environment you want to inspect.
    ");
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
