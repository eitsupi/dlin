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
    let sql = std::fs::read_to_string(fixture_dir().join("models/staging/stg_orders.sql")).unwrap();

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
    let content = std::fs::read_to_string(fixture_dir().join("models/staging/schema.yml")).unwrap();

    let schema: serde_json::Value = serde_saphyr::from_str(&content).unwrap();
    let sources = schema["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 1);

    let tables = sources[0]["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 3);
}

#[test]
fn test_yaml_exposures_parsing() {
    let content = std::fs::read_to_string(fixture_dir().join("models/marts/schema.yml")).unwrap();

    let schema: serde_json::Value = serde_saphyr::from_str(&content).unwrap();
    let exposures = schema["exposures"].as_array().unwrap();
    assert_eq!(exposures.len(), 1);
    assert_eq!(exposures[0]["name"].as_str().unwrap(), "weekly_report");
}
