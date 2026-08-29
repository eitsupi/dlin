use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use dlin_core::graph::column_lineage::DlinDialect;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tools::{call_tool, tools};
use crate::cli::McpArgs;
use crate::commands::{resolve_dialect, resolve_manifest_path_or_default};
use dlin_core::graph;
use dlin_core::graph::types::LineageGraph;
use dlin_core::parser;
use dlin_core::render;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Deserialize)]
pub(super) struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Value,
    #[serde(skip)]
    id: Option<Value>,
}

#[derive(Debug, Serialize)]
pub(super) struct JsonRpcResponse {
    pub(super) jsonrpc: &'static str,
    pub(super) id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub(super) struct JsonRpcError {
    pub(super) code: i32,
    pub(super) message: String,
}

pub(super) struct McpState {
    pub(super) project_dir: PathBuf,
    pub(super) manifest_path: PathBuf,
    pub(super) dialect: DlinDialect,
    pub(super) dialect_warning: Option<String>,
    pub(super) manifest_warnings: Vec<parser::manifest::ManifestDiagnostic>,
    pub(super) manifest: parser::manifest::Manifest,
    pub(super) dag: LineageGraph,
    pub(super) column_lineage_cache: RefCell<graph::column_lineage::ColumnLineageCache>,
}

pub fn run(args: McpArgs) -> Result<()> {
    let state = McpState::load(args)?;

    let counts = render::summary::count_nodes(&state.dag);
    let project_name = state
        .manifest
        .metadata
        .project_name
        .as_deref()
        .unwrap_or("(unknown)");
    eprintln!("dlin MCP server ready");
    eprintln!("  project:  {project_name}");
    eprintln!("  manifest: {}", state.manifest_path.display());
    eprintln!("  dialect:  {}", state.dialect);
    let mut parts = vec![format!("{} models", counts.model)];
    if counts.source > 0 {
        parts.push(format!("{} sources", counts.source));
    }
    if counts.seed > 0 {
        parts.push(format!("{} seeds", counts.seed));
    }
    if counts.snapshot > 0 {
        parts.push(format!("{} snapshots", counts.snapshot));
    }
    if counts.exposure > 0 {
        parts.push(format!("{} exposures", counts.exposure));
    }
    eprintln!("  nodes:    {}", parts.join(", "));

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
    pub(super) fn load(args: McpArgs) -> Result<Self> {
        let project_dir = args
            .project_dir
            .canonicalize()
            .unwrap_or_else(|_| args.project_dir.clone());
        let manifest_path =
            resolve_manifest_path_or_default(args.manifest_path.as_ref(), &project_dir)?;
        let graph_report = parser::manifest::build_graph_from_manifest_report(&manifest_path)?;
        let parser::manifest::ManifestGraphReport {
            graph: dag,
            diagnostics,
            manifest,
            ..
        } = graph_report;
        let resolved_dialect = resolve_dialect(args.dialect.as_ref(), &manifest)?;

        Ok(Self {
            project_dir,
            manifest_path,
            dialect: resolved_dialect.dialect,
            dialect_warning: resolved_dialect.warning,
            manifest_warnings: diagnostics
                .into_iter()
                .filter(|diagnostic| diagnostic.kind.is_user_visible_warning())
                .collect(),
            manifest,
            dag,
            column_lineage_cache: RefCell::new(
                graph::column_lineage::ColumnLineageCache::disabled(),
            ),
        })
    }
}

pub(super) fn parse_request(line: &str) -> Result<JsonRpcRequest, JsonRpcResponse> {
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

pub(super) fn handle_request(req: JsonRpcRequest, state: &McpState) -> Option<JsonRpcResponse> {
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

pub(super) fn error_response(id: Value, code: i32, message: impl Into<String>) -> JsonRpcResponse {
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

pub(super) fn initialize_result(params: &Value) -> Value {
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
