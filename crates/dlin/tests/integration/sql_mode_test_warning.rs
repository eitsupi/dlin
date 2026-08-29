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
