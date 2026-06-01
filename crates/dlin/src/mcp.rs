use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use path_slash::PathExt as _;
use polyglot_sql::DialectType;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cli::McpArgs;
use crate::{check_manifest_freshness, resolve_manifest_path_or_default};
use dlin_core::graph;
use dlin_core::graph::types::{LineageGraph, NodeType};
use dlin_core::parser;
use dlin_core::render;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(skip)]
    id: Option<Value>,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

struct McpState {
    project_dir: PathBuf,
    manifest_path: PathBuf,
    dialect: DialectType,
    manifest: parser::manifest::Manifest,
    dag: LineageGraph,
    column_lineage_cache: RefCell<graph::column_lineage::ColumnLineageCache>,
}

pub fn run(args: McpArgs) -> Result<()> {
    let state = McpState::load(args)?;
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let response = match parse_request(&line) {
            Ok(req) => handle_request(req, &state),
            Err(err_response) => Some(err_response),
        };

        if let Some(response) = response {
            serde_json::to_writer(&mut out, &response)?;
            writeln!(out)?;
            out.flush()?;
        }
    }

    Ok(())
}

impl McpState {
    fn load(args: McpArgs) -> Result<Self> {
        let project_dir = args
            .project_dir
            .canonicalize()
            .unwrap_or_else(|_| args.project_dir.clone());
        let manifest_path =
            resolve_manifest_path_or_default(args.manifest_path.as_ref(), &project_dir)?;
        let manifest = parser::manifest::load_manifest(&manifest_path)?;
        let dag = parser::manifest::build_graph_from_parsed_manifest(&manifest)?;

        Ok(Self {
            project_dir,
            manifest_path,
            dialect: args.dialect,
            manifest,
            dag,
            column_lineage_cache: RefCell::new(
                graph::column_lineage::ColumnLineageCache::disabled(),
            ),
        })
    }
}

fn parse_request(line: &str) -> Result<JsonRpcRequest, JsonRpcResponse> {
    let value: Value = serde_json::from_str(line)
        .map_err(|err| error_response(Value::Null, -32700, format!("parse error: {err}")))?;
    let id = value
        .as_object()
        .and_then(|object| object.get("id").cloned());
    if let Some(ref raw_id) = id
        && !matches!(raw_id, Value::String(_) | Value::Number(_) | Value::Null)
    {
        return Err(error_response(
            Value::Null,
            -32600,
            "invalid request: id must be a string, number, or null",
        ));
    }
    let id_for_error = id.clone().unwrap_or(Value::Null);
    let mut req: JsonRpcRequest = serde_json::from_value(value)
        .map_err(|err| error_response(id_for_error, -32600, format!("invalid request: {err}")))?;
    req.id = id;
    Ok(req)
}

fn handle_request(req: JsonRpcRequest, state: &McpState) -> Option<JsonRpcResponse> {
    req.id.as_ref()?;
    let id = req.id.unwrap_or(Value::Null);
    if req.jsonrpc != "2.0" {
        return Some(error_response(id, -32600, "invalid JSON-RPC version"));
    }

    let result = match req.method.as_str() {
        "initialize" => Ok(initialize_result(&req.params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(&req.params, state),
        method => Err((-32601, format!("method not found: {method}"))),
    };

    Some(match result {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Err((code, message)) => error_response(id, code, message),
    })
}

fn error_response(id: Value, code: i32, message: impl Into<String>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.into(),
        }),
    }
}

fn initialize_result(params: &Value) -> Value {
    let protocol_version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(MCP_PROTOCOL_VERSION);
    let protocol_version = if protocol_version <= MCP_PROTOCOL_VERSION {
        protocol_version
    } else {
        MCP_PROTOCOL_VERSION
    };

    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false }
        },
        "serverInfo": {
            "name": "dlin",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "get_project_summary",
            "description": "Return dbt project summary, node counts, edge count, and manifest freshness.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "find_nodes",
            "description": "Search DAG nodes by keyword or return details for specific names. Covers all node types (models, sources, seeds, etc.).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Case-insensitive substring to match name, description, tags, or file path." },
                    "names": { "type": "array", "items": { "type": "string" }, "description": "Specific node names or unique IDs to describe." },
                    "node_types": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["model", "source", "seed", "snapshot", "test", "exposure"] },
                        "description": "Node types to include. Defaults to all types when omitted."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_lineage",
            "description": "Return model-level lineage graph as JSON for the specified focus models. Depth defaults to one hop each way.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "models": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Focus model names (at least one required)." },
                    "upstream_depth": { "type": "integer", "minimum": 0, "default": 1 },
                    "downstream_depth": { "type": "integer", "minimum": 0, "default": 1 }
                },
                "required": ["models"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_impact",
            "description": "Return downstream impact analysis for a model with severity scoring.",
            "inputSchema": {
                "type": "object",
                "properties": { "model": { "type": "string" } },
                "required": ["model"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_column_lineage",
            "description": "Return upstream or downstream column lineage for one model column. Requires compiled SQL in manifest.json.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "model": { "type": "string" },
                    "column": { "type": "string" },
                    "direction": { "type": "string", "enum": ["upstream", "downstream"] }
                },
                "required": ["model", "column", "direction"],
                "additionalProperties": false
            }
        }),
    ]
}

fn call_tool(params: &Value, state: &McpState) -> std::result::Result<Value, (i32, String)> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        (
            -32602,
            "tools/call params.name must be a string".to_string(),
        )
    })?;
    let args = params.get("arguments").unwrap_or(&Value::Null);

    let result = match name {
        "get_project_summary" => get_project_summary(state),
        "find_nodes" => find_nodes(args, state),
        "get_lineage" => get_lineage(args, state),
        "get_impact" => get_impact(args, state),
        "get_column_lineage" => get_column_lineage(args, state),
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };

    Ok(match result {
        Ok(value) => tool_result(value, false),
        Err(err) => tool_result(json!({ "error": err.to_string() }), true),
    })
}

fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": is_error
    })
}

fn get_project_summary(state: &McpState) -> Result<Value> {
    let project_name = state
        .manifest
        .metadata
        .project_name
        .clone()
        .unwrap_or_else(|| "(unknown)".to_string());
    let manifest_status = match parser::project::DbtProject::load(&state.project_dir) {
        Ok(project) => {
            check_manifest_freshness(&state.project_dir, Some(&state.manifest_path), &project)
        }
        Err(_) => None,
    };
    let report = render::summary::SummaryReport {
        project_name,
        source_mode: "manifest".to_string(),
        node_counts: render::summary::count_nodes(&state.dag),
        edge_count: state.dag.edge_count(),
        vars_count: 0,
        manifest_status,
    };
    Ok(serde_json::to_value(report)?)
}

fn find_nodes(args: &Value, state: &McpState) -> Result<Value> {
    let names = optional_string_array(args, "names")?;
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .map(|s| s.to_lowercase());
    let type_filter: Option<HashSet<NodeType>> = match args.get("node_types") {
        None | Some(Value::Null) => None,
        Some(Value::Array(arr)) => {
            let mut set = HashSet::new();
            for v in arr {
                let s = v
                    .as_str()
                    .with_context(|| "argument 'node_types' must contain only strings")?;
                let nt = node_type_from_str(s).with_context(|| {
                    format!("argument 'node_types' contains unknown value: '{s}'")
                })?;
                set.insert(nt);
            }
            Some(set)
        }
        Some(_) => anyhow::bail!("argument 'node_types' must be an array"),
    };

    let mut nodes = Vec::new();
    let mut not_found = Vec::new();

    if let Some(names) = names {
        for name in names {
            match graph::filter::try_resolve_node_quiet(&state.dag, &name) {
                Some(idx) => {
                    let node = &state.dag[idx];
                    if type_filter
                        .as_ref()
                        .is_none_or(|f| f.contains(&node.node_type))
                    {
                        nodes.push(node_value(node, true));
                    } else {
                        not_found.push(name);
                    }
                }
                None => not_found.push(name),
            }
        }
    } else {
        for idx in state.dag.node_indices() {
            let node = &state.dag[idx];
            if node.node_type == NodeType::Phantom {
                continue;
            }
            if let Some(ref filter) = type_filter
                && !filter.contains(&node.node_type)
            {
                continue;
            }
            if let Some(query) = query.as_deref()
                && !node_matches_query(node, query)
            {
                continue;
            }
            nodes.push(node_value(node, false));
        }
        nodes.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    }

    Ok(json!({
        "nodes": nodes,
        "count": nodes.len(),
        "not_found": not_found
    }))
}

fn node_type_from_str(s: &str) -> Option<NodeType> {
    match s {
        "model" => Some(NodeType::Model),
        "source" => Some(NodeType::Source),
        "seed" => Some(NodeType::Seed),
        "snapshot" => Some(NodeType::Snapshot),
        "test" => Some(NodeType::Test),
        "exposure" => Some(NodeType::Exposure),
        _ => None,
    }
}

fn node_matches_query(node: &graph::types::NodeData, query: &str) -> bool {
    node.label.to_lowercase().contains(query)
        || node
            .description
            .as_deref()
            .is_some_and(|s| s.to_lowercase().contains(query))
        || node
            .tags
            .iter()
            .any(|tag| tag.to_lowercase().contains(query))
        || node
            .file_path
            .as_ref()
            .is_some_and(|p| p.to_slash_lossy().to_lowercase().contains(query))
}

fn node_value(node: &graph::types::NodeData, detailed: bool) -> Value {
    let mut value = json!({
        "unique_id": node.unique_id,
        "name": node.label,
        "node_type": node.node_type.label(),
        "file_path": node.file_path.as_ref().map(|p| p.to_slash_lossy().into_owned()),
        "description": node.description,
        "materialization": node.materialization,
        "tags": node.tags
    });
    if detailed {
        value["columns"] = json!(node.columns);
    }
    value
}

static GRAPH_NODE_FIELDS_SET: LazyLock<HashSet<String>> = LazyLock::new(|| {
    render::json::GRAPH_NODE_FIELDS
        .iter()
        .map(|field| (*field).to_string())
        .collect()
});

fn get_lineage(args: &Value, state: &McpState) -> Result<Value> {
    let models = required_string_array(args, "models")?;
    let upstream = optional_usize(args, "upstream_depth")?.or(Some(1));
    let downstream = optional_usize(args, "downstream_depth")?.or(Some(1));

    let filtered =
        graph::filter::filter_graph(&state.dag, &models, upstream, downstream, &[], true)?;
    let mut buf = Vec::new();
    render::json::render_json_to_writer(&filtered, None, &GRAPH_NODE_FIELDS_SET, &mut buf, false)?;
    Ok(serde_json::from_slice(&buf)?)
}

fn get_impact(args: &Value, state: &McpState) -> Result<Value> {
    let model = required_string(args, "model")?;
    let idx = graph::filter::try_resolve_node_quiet(&state.dag, model)
        .with_context(|| format!("model not found: {model}"))?;
    let report = graph::impact::compute_impact(&state.dag, idx);
    Ok(serde_json::to_value(report)?)
}

fn get_column_lineage(args: &Value, state: &McpState) -> Result<Value> {
    let model = required_string(args, "model")?;
    let column = required_string(args, "column")?;
    let direction = required_string(args, "direction")?;
    let mut cache = state.column_lineage_cache.borrow_mut();

    match direction {
        "upstream" => {
            let mut report =
                graph::column_lineage::compute_cross_model_column_lineage_with_manifest_path(
                    &state.manifest,
                    model,
                    state.dialect,
                    Some(&state.manifest_path),
                    &mut cache,
                );
            report.columns.retain(|entry| entry.column == column);
            if report.total_columns > 0 {
                let column_error_prefix = format!("column '{column}': ");
                report.errors.retain(|err| {
                    !matches!(
                        err.kind,
                        graph::column_lineage::ColumnLineageErrorKind::ColumnNotFound
                    ) || err.what.starts_with(&column_error_prefix)
                });
                let has_column_error = report
                    .errors
                    .iter()
                    .any(|err| err.what.starts_with(&column_error_prefix));
                // Determine whether the *target* model itself has a global error (e.g.
                // ParseFailure, NoCompiledCode) by querying single-model lineage. The
                // cross-model report accumulates errors from all upstream models, including
                // those that may share the same display name in other packages, so
                // string-matching on the display name is not reliable. The single-model
                // result contains only errors for the target model, and the cache populated
                // during cross-model computation makes this a cheap cache hit.
                let target_lineage =
                    graph::column_lineage::compute_column_lineage_with_manifest_path(
                        &state.manifest,
                        model,
                        state.dialect,
                        Some(&state.manifest_path),
                        &mut cache,
                    );
                let has_global_errors = target_lineage.errors.iter().any(|err| {
                    !matches!(
                        err.kind,
                        graph::column_lineage::ColumnLineageErrorKind::ColumnNotFound
                    )
                });
                report.traced_columns = report.columns.len();
                report.total_columns = 1;
                if report.columns.is_empty() && !has_column_error && !has_global_errors {
                    report
                        .errors
                        .push(graph::column_lineage::ColumnLineageError {
                            kind: graph::column_lineage::ColumnLineageErrorKind::ColumnNotFound,
                            what: format!("column '{column}': not found in model output"),
                            why: None,
                            hint: None,
                        });
                }
            }
            Ok(serde_json::to_value(report)?)
        }
        "downstream" => {
            let report = graph::column_lineage::compute_column_impact_with_manifest_path(
                &state.manifest,
                model,
                column,
                state.dialect,
                Some(&state.manifest_path),
                &mut cache,
            );
            Ok(serde_json::to_value(report)?)
        }
        _ => anyhow::bail!("direction must be 'upstream' or 'downstream'"),
    }
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("argument '{key}' is required and must be a string"))
}

fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let n = value
                .as_u64()
                .with_context(|| format!("argument '{key}' must be a non-negative integer"))?;
            let n = usize::try_from(n)
                .with_context(|| format!("argument '{key}' value is out of range"))?;
            Ok(Some(n))
        }
    }
}

fn required_string_array(args: &Value, key: &str) -> Result<Vec<String>> {
    match args.get(key) {
        Some(Value::Array(values)) if !values.is_empty() => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .with_context(|| format!("argument '{key}' must contain only strings"))
            })
            .collect(),
        Some(Value::Array(_)) => {
            anyhow::bail!("argument '{key}' must contain at least one element")
        }
        _ => anyhow::bail!("argument '{key}' is required and must be a non-empty array of strings"),
    }
}

fn optional_string_array(args: &Value, key: &str) -> Result<Option<Vec<String>>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .with_context(|| format!("argument '{key}' must contain only strings"))
            })
            .collect::<Result<Vec<_>>>()
            .map(Some),
        Some(_) => anyhow::bail!("argument '{key}' must be an array of strings"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn state() -> McpState {
        McpState::load(McpArgs {
            project_dir: fixture_project_dir(),
            manifest_path: None,
            dialect: DialectType::Generic,
        })
        .unwrap()
    }

    fn column_lineage_state() -> McpState {
        McpState::load(McpArgs {
            project_dir: column_lineage_fixture_project_dir(),
            manifest_path: None,
            dialect: DialectType::Generic,
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
                "find_nodes",
                "get_lineage",
                "get_impact",
                "get_column_lineage"
            ]
        );
    }

    #[test]
    fn find_nodes_returns_structured_tool_result() {
        let state = state();
        let result = call_tool(
            &json!({
                "name": "find_nodes",
                "arguments": { "names": ["orders"] }
            }),
            &state,
        )
        .unwrap();

        assert_eq!(result["isError"], false);
        assert_eq!(result["structuredContent"]["count"], 1);
        assert_eq!(
            result["structuredContent"]["nodes"][0]["name"],
            Value::String("orders".to_string())
        );
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("orders")
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
        let err = find_nodes(&json!({ "node_types": ["models"] }), &state).unwrap_err();
        assert!(
            err.to_string().contains("unknown value"),
            "expected 'unknown value' in error: {err}"
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
}
