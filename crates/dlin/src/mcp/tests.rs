use std::collections::HashSet;
use std::path::PathBuf;

use crate::cli::{DialectArg, McpArgs};
use dlin_core::graph::column_lineage::{ColumnLineageError, ColumnLineageErrorKind, DlinDialect};
use serde_json::{Value, json};

use super::protocol::{McpState, handle_request, parse_request};
use super::tools::{
    call_tool, error_names_upstream_model, find_nodes, get_column_lineage, get_impact, get_lineage,
    list_nodes, normalize_table_short_name, tools,
};

fn body(result: &Value) -> Value {
    assert!(result.get("structuredContent").is_none());
    assert_eq!(result["content"].as_array().unwrap().len(), 1);
    assert_eq!(result["content"][0]["type"], "text");
    let text = result["content"][0]["text"].as_str().unwrap();
    let value: Value = serde_json::from_str(text).unwrap();
    assert_eq!(text, value.to_string());
    value
}

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

fn row_value_alias_state() -> (tempfile::TempDir, McpState) {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("target")).unwrap();
    let manifest = r#"{
  "metadata": {"adapter_type": "bigquery"},
  "nodes": {
    "model.test.standalone": {
      "unique_id": "model.test.standalone", "name": "standalone",
      "resource_type": "model", "depends_on": {"nodes": []},
      "config": {"materialized": "view", "tags": []},
      "columns": {"x": {"name": "x"}},
      "compiled_code": "WITH latest AS (SELECT ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t) SELECT event.x AS x FROM latest"
    },
    "model.test.deep": {
      "unique_id": "model.test.deep", "name": "deep",
      "resource_type": "model", "depends_on": {"nodes": []},
      "config": {"materialized": "view", "tags": []},
      "columns": {"x": {"name": "x"}},
      "compiled_code": "WITH latest AS (SELECT ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t) SELECT event.x AS x FROM latest"
    },
    "model.test.middle": {
      "unique_id": "model.test.middle", "name": "middle",
      "resource_type": "model", "depends_on": {"nodes": ["model.test.deep"]},
      "config": {"materialized": "view", "tags": []},
      "columns": {"x": {"name": "x"}, "y": {"name": "y"}},
      "compiled_code": "SELECT deep.x AS x, deep.x AS y FROM `p`.`d`.`deep` AS deep"
    },
    "model.test.target": {
      "unique_id": "model.test.target", "name": "target",
      "resource_type": "model", "depends_on": {"nodes": ["model.test.middle"]},
      "config": {"materialized": "view", "tags": []},
      "columns": {"z": {"name": "z"}},
      "compiled_code": "SELECT x AS z FROM `p`.`d`.`middle`"
    }
  }, "sources": {}, "exposures": {}
}"#;
    std::fs::write(tmp.path().join("target/manifest.json"), manifest).unwrap();
    let state = McpState::load(McpArgs {
        project_dir: tmp.path().to_path_buf(),
        manifest_path: None,
        dialect: None,
    })
    .unwrap();
    (tmp, state)
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

    assert!(body(&result).get("warnings").is_none());
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

    assert!(body(&result).get("warnings").is_none());
}

#[test]
fn mcp_keeps_target_source_less_indeterminate_diagnostic() {
    let (_tmp, state) = row_value_alias_state();
    let result = get_column_lineage(
        &json!({
            "model": "standalone",
            "column": "x",
            "direction": "upstream",
            "detail": "full"
        }),
        &state,
    )
    .unwrap();
    let errors = result["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|error| { error["kind"] == "column_indeterminate" && error["column"] == "x" })
    );
    assert!(
        !errors
            .iter()
            .any(|error| error["kind"] == "column_not_found")
    );
}

#[test]
fn mcp_rebases_rename_and_suppresses_unrelated_same_named_diagnostics() {
    let (_tmp, state) = row_value_alias_state();
    let result = get_column_lineage(
        &json!({
            "model": "target",
            "column": "z",
            "direction": "upstream",
            "detail": "full"
        }),
        &state,
    )
    .unwrap();
    let errors = result["errors"].as_array().unwrap();
    assert!(errors.iter().any(|error| {
        error["kind"] == "column_indeterminate"
            && error["column"] == "z"
            && error["what"].as_str().unwrap().starts_with("column 'z':")
    }));
    assert!(
        errors
            .iter()
            .all(|error| { error["column"] != "x" && error["column"] != "y" })
    );
}

#[test]
fn mcp_dialect_warning_preserves_legacy_string_shape() {
    let mut state = column_lineage_state();
    state.dialect_warning = Some("using generic instead".to_string());
    let result = call_tool(
        &json!({"name": "get_column_lineage", "arguments": {"model":"stg_orders", "column":"order_id", "direction":"upstream"}}),
        &state,
    )
    .unwrap();
    assert!(body(&result)["warnings"][0].is_string());
    assert_eq!(body(&result)["warnings"][0], "using generic instead");
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
    let diag = &body(&result)["manifest_diagnostics"][0];
    assert_eq!(diag["kind"], "unsupported_resource_type");
    assert_eq!(diag["section"], "nodes");
    assert_eq!(diag["resource_type"], "operation");
    assert_eq!(diag["count"], 1);
    assert_eq!(diag["examples"][0]["id"], "operation.proj.refresh");
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
    let result = body(&result);
    let warning = result["manifest_diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|warning| warning["kind"] == "future_schema_version")
        .unwrap();
    assert_eq!(warning["count"], 1);
    assert!(warning["examples"][0]["message"].is_string());
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
            "arguments": { "names": ["stg_orders"], "include_sql":true }
        }),
        &state,
    )
    .unwrap();

    assert_eq!(result["isError"], false);
    assert_eq!(body(&result)["count"], 1);
    let node = &body(&result)["nodes"][0];
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
    assert!(result["nodes"][0].get("compiled_sql").is_none());
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

    assert!(nodes.iter().any(|node| node["name"] == "orders"));
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
            .any(|n| n["name"] == "orders")
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
            .any(|n| n["name"] == "orders")
    );
}

#[test]
fn impact_resolves_manifest_unique_id() {
    let state = state();
    let value = get_impact(&json!({ "model": "model.simple_project.orders" }), &state).unwrap();
    assert_eq!(value["source_model"], json!("orders"));
}

#[test]
fn impact_cursors_bind_to_the_effective_node_types() {
    let state = state();
    let default = get_impact(&json!({"model": "orders"}), &state).unwrap();
    assert_eq!(default["affected_tests"], json!(null));
    assert_eq!(default["page"]["total"], 2);

    let mut args =
        json!({"model": "orders", "limit": 1, "node_types": ["test", "model", "exposure"]});
    let first = get_impact(&args, &state).unwrap();
    assert_eq!(first["node_types"], json!(["model", "test", "exposure"]));
    assert_eq!(first["affected_tests"], 1);
    let cursor = &first["page"]["next_cursor"];
    assert!(cursor.is_string());
    args["cursor"] = cursor.clone();
    args["node_types"] = json!(["exposure", "model", "test", "model"]);
    assert!(get_impact(&args, &state).is_ok());
    args["node_types"] = json!(["model", "exposure"]);
    assert!(
        get_impact(&args, &state)
            .unwrap_err()
            .to_string()
            .contains("cursor")
    );
    args["node_types"] = json!([]);
    assert!(
        get_impact(&args, &state)
            .unwrap_err()
            .to_string()
            .contains("must not be empty")
    );
}

/// Manifest dependencies are not assessed, so no detail level or page of an
/// impact claims that nothing is affected. The resolved input is reported
/// outside the page too.
#[test]
fn impact_reports_unassessed_manifest_exploration() {
    let state = state();
    for detail in ["summary", "full"] {
        let value = get_impact(
            &json!({ "model": "model.simple_project.orders", "detail": detail }),
            &state,
        )
        .unwrap();
        assert_eq!(value["proves_no_impact"], json!(false), "{detail}");
        assert_eq!(
            value["source_node"]["unique_id"],
            json!("model.simple_project.orders"),
            "{detail}"
        );
        assert_eq!(
            value["source_node"]["dependency_analysis"]["unresolved"][0]["scope"],
            json!("manifest_dependencies"),
            "{detail}"
        );
        assert_eq!(
            value["exploration"],
            json!([{
                "kind": "model_dependency",
                "status": "incomplete",
                "gaps": [{"type": "not_assessed", "scope": "manifest_dependencies"}]
            }]),
            "{detail}"
        );
    }
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
            "direction": "upstream",
            "detail": "full"
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
    // stg_bad_sql has valid YAML columns but unparseable SQL (so it has no output),
    // so analysis returns a global ParseFailure error. A ColumnNotFound error must
    // NOT be synthesized on top of it, because the column may well exist — we just
    // cannot confirm it due to the SQL failure.
    let state = column_lineage_state();
    let result = get_column_lineage(
        &json!({
            "model": "stg_bad_sql",
            "column": "some_col",
            "direction": "upstream",
            "detail": "full"
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
            "direction": "upstream",
            "detail": "full"
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
            "direction": "upstream",
            "detail": "full"
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
            "direction": "upstream",
            "detail": "full"
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
            "direction": "upstream",
            "detail": "full"
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
fn column_error_name_uses_structured_identity_only() {
    let error = ColumnLineageError {
        kind: ColumnLineageErrorKind::ColumnNotFound,
        column: Some("order_id".to_string()),
        slot: None,
        what: "column 'order_id': not found in model output".to_string(),
        why: None,
        hint: None,
    };
    assert_eq!(error.column_name(), Some("order_id"));
    assert_eq!(
        ColumnLineageError {
            column: None,
            ..error.clone()
        }
        .column_name(),
        None
    );
    assert_eq!(
        ColumnLineageError {
            kind: ColumnLineageErrorKind::ParseFailure,
            column: None,
            slot: None,
            what: "failed to parse SQL".to_string(),
            why: None,
            hint: None,
        }
        .column_name(),
        None
    );
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

#[test]
fn manifest_diagnostics_are_grouped_in_summary_and_unit_tests_do_not_expand_other_tools() {
    let temp = tempfile::tempdir().unwrap();
    let manifest_path = temp.path().join("manifest.json");
    let mut artifact = json!({
        "metadata":{"project_name":"proj","adapter_type":"duckdb"},
        "nodes":{},
        "unit_tests":{}
    });
    for i in (0..5).rev() {
        let id = format!("operation.proj.run_{i}");
        artifact["nodes"][&id] =
            json!({"name":format!("run_{i}"),"unique_id":id,"resource_type":"operation"});
    }
    let load = |artifact: &Value| {
        std::fs::write(&manifest_path, artifact.to_string()).unwrap();
        McpState::load(McpArgs {
            project_dir: temp.path().to_path_buf(),
            manifest_path: Some(manifest_path.clone()),
            dialect: None,
        })
        .unwrap()
    };
    let request = json!({"name":"list_nodes","arguments":{}});
    let before = call_tool(&request, &load(&artifact)).unwrap();
    for i in 0..73 {
        let id = format!("unit_test.proj.check_{i}");
        artifact["unit_tests"][&id] = json!({"unique_id":id,"resource_type":"unit_test",
            "model":"orders","given":[{"input":"ref('raw_orders')","rows":[]}],"expect":{"rows":[]}});
    }
    let state = load(&artifact);
    assert_eq!(call_tool(&request, &state).unwrap(), before);
    let summary = body(&call_tool(&json!({"name":"get_project_summary"}), &state).unwrap());
    assert_eq!(summary["skipped_unit_tests"], 73);
    assert_eq!(summary["manifest_status"]["verification"], "unverified");
    assert!(summary.get("vars_count").is_none());
    let diag = &summary["manifest_diagnostics"][0];
    assert_eq!(diag["kind"], "unsupported_resource_type");
    assert_eq!(diag["count"], 5);
    assert_eq!(diag["examples"].as_array().unwrap().len(), 3);
    assert_eq!(diag["examples"][0]["id"], "operation.proj.run_0");
}

#[test]
fn compact_wire_size_and_detail_agree_on_column_lineage_facts() {
    let temp = tempfile::tempdir().unwrap();
    let state = McpState::load(McpArgs {
        project_dir: temp.path().to_path_buf(),
        manifest_path: Some(column_lineage_fixture_project_dir().join("target/manifest.json")),
        dialect: None,
    })
    .unwrap();
    for (model, column, direction, wire_max, text_before) in [
        ("orders", "total_amount", "upstream", 2736, 1833),
        (
            "mart_unrelated_parse_fail",
            "bad_col",
            "upstream",
            3300,
            2213,
        ),
        ("stg_orders", "order_id", "downstream", 7025, 4902),
    ] {
        let args = json!({"model":model,"column":column,"direction":direction});
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
            "params":{"name":"get_column_lineage","arguments":args}});
        let response =
            handle_request(parse_request(&request.to_string()).unwrap(), &state).unwrap();
        let wire = serde_json::to_vec(&response).unwrap().len();
        let result = response.result.unwrap();
        let text_length = result["content"][0]["text"].as_str().unwrap().len();
        assert!(
            wire <= wire_max && text_length < text_before,
            "{model}.{column}: wire {wire}, text {text_length}"
        );
        let summary = body(&result);
        let mut full_args = args;
        full_args["detail"] = json!("full");
        let full = get_column_lineage(&full_args, &state).unwrap();
        for key in [
            "status",
            "known_count",
            "unresolved_count",
            "confirmed_count",
            "possible_count",
            "no_impact",
        ] {
            assert_eq!(summary.get(key), full.get(key), "{model}.{column} {key}");
        }
        match (model, direction) {
            ("orders", _) => {
                assert_eq!(summary["status"], "complete");
                assert!(summary["known_count"].as_u64().unwrap() > 0);
            }
            (_, "upstream") => {
                assert_eq!(summary["status"], "partial");
                assert_eq!(summary["known_count"], 0);
                assert_eq!(summary["incomplete_boundary"], true);
            }
            _ => {
                assert_eq!(summary["no_impact"], false);
                assert!(summary["confirmed_count"].as_u64().unwrap() > 0);
            }
        }
    }
}

#[test]
fn pages_preserve_edges_and_endpoints_and_reject_mismatched_cursors() {
    let state = state();
    let args = json!({"models":["orders"], "upstream_depth":3,"downstream_depth":3});
    let complete = get_lineage(&args, &state).unwrap();
    let mut paged_args = args.clone();
    paged_args["limit"] = json!(1);
    let mut collected = Vec::new();
    loop {
        let page = get_lineage(&paged_args, &state).unwrap();
        let id = |value: &Value| value.as_str().unwrap().to_owned();
        let nodes: HashSet<_> = page["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| id(&node["unique_id"]))
            .collect();
        assert!(nodes.contains("model.simple_project.orders"));
        for edge in page["edges"].as_array().unwrap() {
            assert!(nodes.contains(&id(&edge["source"])));
            assert!(nodes.contains(&id(&edge["target"])));
            collected.push(edge.clone());
        }
        let cursor = &page["page"]["next_cursor"];
        if cursor.is_null() {
            break;
        }
        let mut mismatch = paged_args.clone();
        mismatch["cursor"] = cursor.clone();
        mismatch["upstream_depth"] = json!(2);
        assert!(get_lineage(&mismatch, &state).is_err());
        paged_args["cursor"] = cursor.clone();
    }
    assert_eq!(collected, *complete["edges"].as_array().unwrap());
}

#[test]
fn invalid_tool_arguments_use_one_compact_text_item() {
    let result = call_tool(
        &json!({"name":"find_nodes","arguments":{"names":vec!["orders";21]}}),
        &state(),
    )
    .unwrap();
    assert_eq!(result["isError"], true);
    assert!(
        body(&result)["error"]
            .as_str()
            .unwrap()
            .contains("at most 20")
    );
}

#[test]
fn column_pages_preserve_each_impact_and_gap_at_both_detail_levels() {
    let state = column_lineage_state();
    for detail in ["summary", "full"] {
        let args = json!({"model":"stg_orders","column":"order_id","direction":"downstream","detail":detail});
        let all = get_column_lineage(&args, &state).unwrap();
        let mut request = args.clone();
        request["limit"] = json!(1);
        let mut confirmed = Vec::new();
        let mut possible = Vec::new();
        let mut gaps = Vec::new();
        loop {
            let result = get_column_lineage(&request, &state).unwrap();
            assert_eq!(result["confirmed_count"], all["confirmed_count"]);
            assert_eq!(result["possible_count"], all["possible_count"]);
            confirmed.extend_from_slice(result["confirmed"].as_array().unwrap());
            possible.extend_from_slice(result["possible"].as_array().unwrap());
            for entry in result["exploration"].as_array().unwrap() {
                if let Some(rows) = entry["gaps"].as_array() {
                    gaps.extend_from_slice(rows);
                }
            }
            if result["page"]["next_cursor"].is_null() {
                break;
            }
            request["cursor"] = result["page"]["next_cursor"].clone();
            let mut mismatch = request.clone();
            mismatch["detail"] = json!(if detail == "summary" {
                "full"
            } else {
                "summary"
            });
            assert!(get_column_lineage(&mismatch, &state).is_err());
        }
        assert_eq!(confirmed, *all["confirmed"].as_array().unwrap());
        assert_eq!(possible, *all["possible"].as_array().unwrap());
        let all_gaps: Vec<_> = all["exploration"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|entry| entry["gaps"].as_array())
            .flatten()
            .cloned()
            .collect();
        assert_eq!(gaps, all_gaps);
    }
}
