use std::collections::HashSet;
use std::path::PathBuf;

use crate::cli::{DialectArg, McpArgs};
use dlin_core::graph::column_lineage::DlinDialect;
use serde_json::{Value, json};

use super::protocol::{McpState, handle_request, parse_request};
use super::tools::{
    call_tool, error_names_upstream_model, extract_column_not_found_name, find_nodes,
    get_column_lineage, get_impact, get_lineage, list_nodes, normalize_table_short_name, tools,
};

fn fixture_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("simple_project")
}

fn column_lineage_fixture_project_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("column_lineage_project")
}

fn generic_dialect() -> DialectArg {
    DialectArg {
        dialect: DlinDialect::Generic,
        requested: "generic".to_string(),
    }
}

fn state() -> McpState {
    McpState::load(McpArgs {
        project_dir: fixture_project_dir(),
        manifest_path: None,
        dialect: Some(generic_dialect()),
    })
    .unwrap()
}

fn column_lineage_state() -> McpState {
    McpState::load(McpArgs {
        project_dir: column_lineage_fixture_project_dir(),
        manifest_path: None,
        dialect: Some(generic_dialect()),
    })
    .unwrap()
}

#[test]
fn tools_list_exposes_expected_tools() {
    let names: Vec<String> = tools()
        .into_iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        names,
        vec![
            "get_project_summary",
            "list_nodes",
            "find_nodes",
            "get_lineage",
            "get_impact",
            "get_column_lineage"
        ]
    );
}

#[test]
fn mcp_auto_detects_manifest_dialect() {
    let state = McpState::load(McpArgs {
        project_dir: column_lineage_fixture_project_dir(),
        manifest_path: None,
        dialect: None,
    })
    .unwrap();

    assert_eq!(state.dialect, DlinDialect::DuckDB);
    assert!(state.dialect_warning.is_none());

    let result = call_tool(
        &json!({
            "name": "get_column_lineage",
            "arguments": {
                "model": "stg_orders",
                "column": "order_id",
                "direction": "upstream"
            }
        }),
        &state,
    )
    .unwrap();

    assert!(result["structuredContent"].get("warnings").is_none());
}

#[test]
fn mcp_explicit_supported_dialect_has_no_warning() {
    let state = column_lineage_state();

    assert_eq!(state.dialect, DlinDialect::Generic);

    let result = call_tool(
        &json!({
            "name": "get_column_lineage",
            "arguments": {
                "model": "stg_orders",
                "column": "order_id",
                "direction": "upstream"
            }
        }),
        &state,
    )
    .unwrap();

    assert!(result["structuredContent"].get("warnings").is_none());
}

#[test]
fn mcp_dialect_warning_preserves_legacy_string_shape() {
    let mut state = column_lineage_state();
    state.dialect_warning = Some("using generic instead".to_string());
    let result = call_tool(
        &json!({"name": "get_project_summary", "arguments": {}}),
        &state,
    )
    .unwrap();
    assert!(result["structuredContent"]["warnings"][0].is_string());
    assert_eq!(
        result["structuredContent"]["warnings"][0],
        "using generic instead"
    );
}

#[test]
fn mcp_unknown_resource_warning_preserves_core_identity() {
    let temp = tempfile::tempdir().unwrap();
    let manifest_path = temp.path().join("manifest.json");
    let manifest = json!({
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
            "dbt_version": "1.8.0"
        },
        "nodes": {
            "operation.proj.refresh": {
                "unique_id": "operation.proj.refresh",
                "name": "refresh",
                "resource_type": "operation",
                "depends_on": {"nodes": []},
                "config": {},
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "compiled_code": null,
                "database": null,
                "schema": null
            }
        }
    });
    std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let state = McpState::load(McpArgs {
        project_dir: temp.path().to_path_buf(),
        manifest_path: Some(manifest_path),
        dialect: Some(generic_dialect()),
    })
    .unwrap();
    let result = call_tool(
        &json!({"name": "get_project_summary", "arguments": {}}),
        &state,
    )
    .unwrap();
    assert_eq!(
        result["structuredContent"]["warnings"][0]["kind"],
        "unsupported_resource_type"
    );
    assert_eq!(
        result["structuredContent"]["warnings"][0]["raw_type"],
        "operation"
    );
    assert_eq!(
        result["structuredContent"]["warnings"][0]["hint"],
        state.manifest_warnings[0].hint.as_deref().unwrap()
    );
}

#[test]
fn mcp_future_schema_warning_is_visible_with_structured_identity() {
    let temp = tempfile::tempdir().unwrap();
    let manifest_path = temp.path().join("manifest.json");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&json!({
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v99/manifest.json",
                "dbt_version": "1.9.0"
            },
            "nodes": {}
        }))
        .unwrap(),
    )
    .unwrap();
    let state = McpState::load(McpArgs {
        project_dir: temp.path().to_path_buf(),
        manifest_path: Some(manifest_path),
        dialect: Some(generic_dialect()),
    })
    .unwrap();
    let result = call_tool(
        &json!({"name": "get_project_summary", "arguments": {}}),
        &state,
    )
    .unwrap();
    let warning = result["structuredContent"]["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["kind"] == "future_schema_version")
        .expect("future schema warning");
    assert_eq!(warning["level"], "warning");
    assert!(
        warning["what"]
            .as_str()
            .is_some_and(|what| !what.is_empty())
    );
    assert!(warning["why"].is_null());
    assert!(
        warning["hint"]
            .as_str()
            .is_some_and(|hint| !hint.is_empty())
    );
}

#[test]
fn mcp_requires_dialect_when_manifest_has_no_adapter_type() {
    let error = match McpState::load(McpArgs {
        project_dir: fixture_project_dir(),
        manifest_path: None,
        dialect: None,
    }) {
        Ok(_) => panic!("MCP should require --dialect when the manifest has no adapter_type"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("use --dialect"));
}

#[test]
fn find_nodes_returns_full_details_including_compiled_sql() {
    let state = column_lineage_state();
    let result = call_tool(
        &json!({
            "name": "find_nodes",
            "arguments": { "names": ["stg_orders"] }
        }),
        &state,
    )
    .unwrap();

    assert_eq!(result["isError"], false);
    assert_eq!(result["structuredContent"]["count"], 1);
    let node = &result["structuredContent"]["nodes"][0];
    assert_eq!(node["name"], Value::String("stg_orders".to_string()));
    assert!(
        node.get("compiled_sql").is_some(),
        "compiled_sql field must be present"
    );
    assert!(
        node["compiled_sql"].is_string(),
        "compiled_sql must be non-null string when manifest has compiled SQL"
    );
    assert!(
        node.get("columns").is_some(),
        "columns field must be present"
    );
}

#[test]
fn find_nodes_resolves_manifest_unique_id() {
    let state = column_lineage_state();
    let result = find_nodes(&json!({ "names": ["model.clp.stg_orders"] }), &state).unwrap();
    assert_eq!(result["count"], 1);
    assert_eq!(result["not_found"], json!([]));
    assert_eq!(result["nodes"][0]["name"], json!("stg_orders"));
    assert!(result["nodes"][0]["compiled_sql"].is_string());
}

#[test]
fn list_nodes_returns_nodes_without_compiled_sql() {
    let state = state();
    let result = list_nodes(&json!({}), &state).unwrap();
    let nodes = result["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty());
    for node in nodes {
        assert!(
            node.get("compiled_sql").is_none(),
            "list_nodes must not include compiled_sql"
        );
        assert!(
            node.get("columns").is_none(),
            "list_nodes must not include columns"
        );
    }
}

#[test]
fn list_nodes_rejects_non_string_query() {
    let state = state();
    let err = list_nodes(&json!({ "query": 123 }), &state).unwrap_err();
    assert!(
        err.to_string().contains("'query' must be a string"),
        "expected type error for query: {err}"
    );
}

#[test]
fn lineage_rejects_empty_models_array() {
    let state = state();
    let err = get_lineage(&json!({ "models": [] }), &state).unwrap_err();
    assert!(
        err.to_string().contains("at least one"),
        "expected 'at least one' in error: {err}"
    );
}

#[test]
fn find_nodes_rejects_unknown_node_type() {
    let state = state();
    let err = find_nodes(
        &json!({ "names": ["orders"], "node_types": ["models"] }),
        &state,
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("unknown value"),
        "expected 'unknown value' in error: {err}"
    );
}

#[test]
fn find_nodes_requires_names() {
    let state = state();
    let err = find_nodes(&json!({}), &state).unwrap_err();
    assert!(
        err.to_string().contains("'names' is required"),
        "expected required error for names: {err}"
    );
}

#[test]
fn lineage_defaults_to_one_hop_for_focused_models() {
    let state = state();
    let value = get_lineage(&json!({ "models": ["orders"] }), &state).unwrap();
    let nodes = value["nodes"].as_array().unwrap();
    let edges = value["edges"].as_array().unwrap();

    assert!(nodes.iter().any(|node| node["label"] == "orders"));
    assert!(!edges.is_empty());
}

#[test]
fn lineage_reports_not_found_for_unknown_models() {
    let state = state();
    let value = get_lineage(&json!({ "models": ["orders", "no_such_model"] }), &state).unwrap();
    let not_found = value["not_found"].as_array().unwrap();
    assert_eq!(not_found, &[json!("no_such_model")]);
    // known model is still returned
    assert!(
        value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["label"] == "orders")
    );
}

#[test]
fn lineage_returns_empty_when_all_models_unresolved() {
    let state = state();
    let value = get_lineage(&json!({ "models": ["no_such_model"] }), &state).unwrap();
    assert_eq!(value["nodes"], json!([]));
    assert_eq!(value["edges"], json!([]));
    assert_eq!(value["not_found"], json!(["no_such_model"]));
}

#[test]
fn lineage_resolves_manifest_unique_id() {
    let state = state();
    let value = get_lineage(
        &json!({ "models": ["model.simple_project.orders"] }),
        &state,
    )
    .unwrap();
    assert_eq!(value["not_found"], json!([]));
    assert!(
        value["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["label"] == "orders")
    );
}

#[test]
fn impact_resolves_manifest_unique_id() {
    let state = state();
    let value = get_impact(&json!({ "model": "model.simple_project.orders" }), &state).unwrap();
    assert_eq!(value["source_model"], json!("orders"));
}

#[test]
fn explicit_null_id_gets_response() {
    let state = state();
    let req = parse_request(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
    let response = handle_request(req, &state).unwrap();

    assert_eq!(response.id, Value::Null);
    assert_eq!(response.result, Some(json!({})));
}

#[test]
fn missing_id_is_notification() {
    let state = state();
    let req = parse_request(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();

    assert!(handle_request(req, &state).is_none());
}

#[test]
fn invalid_id_type_returns_invalid_request_error() {
    // JSON-RPC 2.0 requires id to be a string, number, or null.
    // Requests with any other id type (e.g. object, array) must be rejected
    // with -32600 without invoking the method handler.
    let response = parse_request(r#"{"jsonrpc":"2.0","method":"ping","id":{}}"#).unwrap_err();

    assert_eq!(response.id, Value::Null);
    let err = response.error.as_ref().unwrap();
    assert_eq!(err.code, -32600);
}

#[test]
fn column_lineage_parse_failure_not_replaced_when_model_given_as_unique_id() {
    // When the model is specified as a unique ID (e.g. "model.clp.stg_bad_sql"),
    // report.model resolves to the short display name "stg_bad_sql". The global-error
    // marker must use the resolved name, not the raw unique ID, to match the error's
    // what field ("failed to parse SQL for 'stg_bad_sql'").
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "model.clp.stg_bad_sql",
            "column": "some_col",
            "direction": "upstream"
        }),
        &state,
    )
    .unwrap();

    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("parse_failure")),
        "expected a parse_failure error; got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("column_not_found")),
        "must not synthesize column_not_found when a parse_failure is present (unique ID path); got: {errors:?}"
    );
}

#[test]
fn column_lineage_parse_failure_not_replaced_by_column_not_found() {
    // stg_bad_sql has valid YAML columns (total_columns > 0) but unparseable SQL,
    // so analysis returns a global ParseFailure error. A ColumnNotFound error must
    // NOT be synthesized on top of it, because the column may well exist — we just
    // cannot confirm it due to the SQL failure.
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "stg_bad_sql",
            "column": "some_col",
            "direction": "upstream"
        }),
        &state,
    )
    .unwrap();

    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("parse_failure")),
        "expected a parse_failure error; got: {errors:?}"
    );
    assert!(
        !errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("column_not_found")),
        "must not synthesize column_not_found when a parse_failure is present; got: {errors:?}"
    );
}

#[test]
fn column_lineage_unrelated_upstream_parse_failure_does_not_suppress_column_not_found() {
    // mart_unrelated_parse_fail references both stg_orders (parses fine) and
    // stg_bad_sql (ParseFailure). When we query a column that does not exist in
    // the target model, ColumnNotFound must still be synthesized — the upstream
    // ParseFailure belongs to an unrelated column and must not suppress the
    // diagnostic for the missing column.
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "mart_unrelated_parse_fail",
            "column": "nonexistent_col",
            "direction": "upstream"
        }),
        &state,
    )
    .unwrap();

    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("column_not_found")),
        "must synthesize column_not_found when the column is absent and the parse failure is unrelated; got: {errors:?}"
    );
}

#[test]
fn column_lineage_upstream_parse_failure_shown_when_column_depends_on_failing_model() {
    // mart_unrelated_parse_fail.bad_col comes from stg_bad_sql (ParseFailure).
    // The error must be included in the response because stg_bad_sql is on the
    // lineage path of bad_col.
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "mart_unrelated_parse_fail",
            "column": "bad_col",
            "direction": "upstream"
        }),
        &state,
    )
    .unwrap();

    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("parse_failure")),
        "expected parse_failure for bad_col whose source model fails to parse; got: {errors:?}"
    );
}

#[test]
fn column_lineage_upstream_parse_failure_hidden_when_column_unrelated_to_failing_model() {
    // mart_unrelated_parse_fail.order_id comes from stg_orders (parses fine).
    // stg_bad_sql is a sibling dependency used only by bad_col, so its ParseFailure
    // must NOT appear when querying order_id's lineage.
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "mart_unrelated_parse_fail",
            "column": "order_id",
            "direction": "upstream"
        }),
        &state,
    )
    .unwrap();

    let errors = result["errors"].as_array().unwrap();
    assert!(
        !errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("parse_failure")),
        "must not include stg_bad_sql ParseFailure when order_id does not depend on it; got: {errors:?}"
    );
}

#[test]
fn column_lineage_unrelated_column_not_found_suppressed_when_name_collides_with_lineage_path() {
    // mart_id_name_collision.order_id traces through stg_mixed_id.order_id →
    // stg_orders.order_id → raw.orders.id, so "id" appears as a column name on
    // the retained path.  stg_mixed_id also selects an output column named "id"
    // directly from stg_orders (which does not expose "id" — it renames it to
    // order_id), producing ColumnNotFound("id") in per-model analysis.
    // Because "id" is not in lineage_model_columns for stg_mixed_id (only
    // "order_id" is on the retained path), that error must be suppressed.
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "mart_id_name_collision",
            "column": "order_id",
            "direction": "upstream"
        }),
        &state,
    )
    .unwrap();

    let errors = result["errors"].as_array().unwrap();
    assert!(
        !errors
            .iter()
            .any(|e| e["kind"].as_str() == Some("column_not_found")),
        "ColumnNotFound(\"id\") from stg_mixed_id must be suppressed when \"id\" is not on stg_mixed_id's lineage path for order_id; got: {errors:?}"
    );
}

#[test]
fn error_name_matching_avoids_orders_stg_orders_overlap() {
    let upstream_models = HashSet::from(["orders".to_string()]);
    assert!(error_names_upstream_model(
        "failed to parse SQL for 'orders'",
        &upstream_models
    ));
    assert!(!error_names_upstream_model(
        "failed to parse SQL for 'stg_orders'",
        &upstream_models
    ));
}

#[test]
fn find_nodes_resolves_semantic_layer_full_unique_ids() {
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("refs")
        .join("jaffle-shop")
        .join("target")
        .join("manifest.json");
    if !manifest_path.exists() {
        eprintln!(
            "SKIP: jaffle-shop fixture not found at {manifest_path:?}; run `make fixtures` to enable this test"
        );
        return;
    }
    let state = McpState::load(McpArgs {
        project_dir: manifest_path
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf(),
        manifest_path: Some(manifest_path),
        dialect: Some(generic_dialect()),
    })
    .unwrap();

    let result = find_nodes(
        &json!({
            "names": [
                "semantic_model.jaffle_shop.supplies",
                "metric.jaffle_shop.revenue",
                "saved_query.jaffle_shop.revenue_metrics"
            ]
        }),
        &state,
    )
    .unwrap();

    assert_eq!(
        result["not_found"],
        json!([]),
        "all full unique IDs should resolve"
    );
    assert_eq!(result["count"], 3);
    let unique_ids: Vec<&str> = result["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["unique_id"].as_str().unwrap())
        .collect();
    assert!(
        unique_ids.contains(&"semantic_model.jaffle_shop.supplies"),
        "semantic_model node should resolve to correct type"
    );
    assert!(
        unique_ids.contains(&"metric.jaffle_shop.revenue"),
        "metric node should resolve to correct type"
    );
    assert!(
        unique_ids.contains(&"saved_query.jaffle_shop.revenue_metrics"),
        "saved_query node should resolve to correct type"
    );
}

#[test]
fn extract_column_not_found_name_parses_column_prefix() {
    assert_eq!(
        extract_column_not_found_name("column 'order_id': not found in model output"),
        Some("order_id")
    );
    assert_eq!(extract_column_not_found_name("failed to parse SQL"), None);
}

#[test]
fn normalize_table_short_name_extracts_leaf_name() {
    assert_eq!(normalize_table_short_name("db.schema.orders"), "orders");
    assert_eq!(
        normalize_table_short_name("\"db\".\"schema\".\"orders\""),
        "orders"
    );
    assert_eq!(normalize_table_short_name("orders"), "orders");
}
