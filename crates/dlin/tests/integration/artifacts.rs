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
