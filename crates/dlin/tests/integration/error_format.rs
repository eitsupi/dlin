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
