use super::*;
use std::process::Command;

/// The fixture declares its sources without a database, schema, identifier or
/// relation name, so the schema of `raw.orders` is only the source name.
fn fixture() -> PathBuf {
    workspace_root()
        .join("tests")
        .join("fixtures")
        .join("column_lineage_project")
}

fn run_column(args: &[&str]) -> String {
    let output = Command::new(binary_path())
        .arg("column")
        .args(args)
        .args(["--project-dir", fixture().to_str().unwrap(), "--no-cache"])
        .output()
        .expect("Failed to run binary");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn upstream_plain_shows_the_source_schema_defaulted_to_the_source_name() {
    let stdout = run_column(&[
        "upstream",
        "customers",
        "--column",
        "order_count",
        "--column",
        "lifetime_value",
        "-o",
        "plain",
    ]);
    assert!(
        stdout.contains("raw.orders.id → stg_orders.order_id → orders.order_id"),
        "{stdout}"
    );
    assert!(
        stdout.contains("raw.payments.amount → stg_payments.amount → orders.total_amount"),
        "{stdout}"
    );
    assert!(!stdout.contains("  →  orders.id"), "{stdout}");
}

#[test]
fn upstream_json_keeps_the_source_schema_in_known_tables() {
    let stdout = run_column(&["upstream", "customers"]);
    let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let mut tables: Vec<String> = reports[0]["output"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|column| column["lineage"]["known"].as_array().unwrap())
        .map(|source| source["table"].as_str().unwrap().to_string())
        .collect();
    tables.sort();
    tables.dedup();
    assert_eq!(tables, ["raw.customers", "raw.orders", "raw.payments"]);
}

#[test]
fn upstream_mermaid_separates_the_source_from_the_model_of_the_same_name() {
    let stdout = run_column(&["upstream", "customers", "-o", "mermaid"]);
    assert!(stdout.contains(r#"subgraph sg1["orders"]"#), "{stdout}");
    for source in ["raw.customers", "raw.orders", "raw.payments"] {
        assert!(
            stdout.contains(&format!(r#"["{source}"]"#)),
            "missing {source} subgraph: {stdout}"
        );
    }
}

#[test]
fn downstream_impact_of_a_model_over_defaulted_sources_still_finds_its_path() {
    let stdout = run_column(&["downstream", "stg_orders", "--column", "order_id"]);
    let reports: Vec<serde_json::Value> = serde_json::from_str(&stdout).unwrap();
    let confirmed = reports[0]["confirmed"].as_array().unwrap();
    assert!(
        confirmed
            .iter()
            .any(|entry| entry["target"]["model"] == "orders"
                && entry["target"]["column"] == "order_id"),
        "{confirmed:?}"
    );
}

#[test]
fn upstream_dot_separates_the_source_from_the_model_of_the_same_name() {
    let stdout = run_column(&["upstream", "customers", "-o", "dot"]);
    for cluster in ["orders", "raw.customers", "raw.orders", "raw.payments"] {
        assert!(
            stdout.contains(&format!(r#"label="{cluster}";"#)),
            "missing {cluster} cluster: {stdout}"
        );
    }
}
