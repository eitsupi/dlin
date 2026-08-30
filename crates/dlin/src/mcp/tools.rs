use std::collections::HashSet;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use path_slash::PathExt as _;
use serde_json::{Value, json};

use super::protocol::McpState;
use crate::commands::check_manifest_freshness;
use dlin_core::graph;
use dlin_core::graph::types::NodeType;
use dlin_core::parser;
use dlin_core::render;

pub(super) fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "get_project_summary",
            "description": "Return a summary of the loaded dbt project: node counts by type, total edge count, and whether manifest.json reflects the latest project source files (SQL models, YAML schemas, seeds, and macros).",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false }
        }),
        json!({
            "name": "list_nodes",
            "description": "Search and list nodes in the dbt DAG. Returns lightweight metadata (name, type, description, tags, file path) without SQL content. Use find_nodes to retrieve full details for specific nodes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Case-insensitive substring filter applied to node name, description, tags, and file path. Returns all nodes when omitted." },
                    "node_types": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["model", "source", "seed", "snapshot", "test", "exposure", "semantic_model", "metric", "saved_query"] },
                        "description": "Node types to include. Defaults to all types when omitted."
                    }
                },
                "additionalProperties": false
            }
        }),
        json!({
            "name": "find_nodes",
            "description": "Look up one or more dbt nodes by name or unique ID. Returns full details including compiled SQL, column list, description, tags, file path, and materialization.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "names": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Node names (e.g. 'orders') or dbt unique IDs (e.g. 'model.my_project.orders', 'source.my_project.raw.orders') to look up." },
                    "node_types": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["model", "source", "seed", "snapshot", "test", "exposure", "semantic_model", "metric", "saved_query"] },
                        "description": "Restrict results to these node types. Nodes whose type does not match are reported in not_found."
                    }
                },
                "required": ["names"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_lineage",
            "description": "Return the lineage subgraph around a set of models as nodes and directed edges. Use upstream_depth and downstream_depth to control traversal depth.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "models": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Names or unique IDs of the models to centre the lineage on. At least one is required." },
                    "upstream_depth": { "type": "integer", "minimum": 0, "default": 1, "description": "Number of hops to traverse toward sources. Default: 1." },
                    "downstream_depth": { "type": "integer", "minimum": 0, "default": 1, "description": "Number of hops to traverse toward exposures and consumers. Default: 1." }
                },
                "required": ["models"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_impact",
            "description": "Analyse the downstream impact of changing a model: returns all downstream nodes with their distance and a severity score based on node type and materialization.",
            "inputSchema": {
                "type": "object",
                "properties": { "model": { "type": "string", "description": "Name or unique ID of the model whose downstream impact to analyse." } },
                "required": ["model"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_column_lineage",
            "description": "Trace the lineage of a single column across upstream or downstream models, following transformations. Requires compiled SQL in manifest.json.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "model": { "type": "string", "description": "Name or unique ID of the model containing the column." },
                    "column": { "type": "string", "description": "Column name to trace." },
                    "direction": { "type": "string", "enum": ["upstream", "downstream"], "description": "'upstream' traces where the column's value originates; 'downstream' traces models and columns that derive from it." }
                },
                "required": ["model", "column", "direction"],
                "additionalProperties": false
            }
        }),
    ]
}

pub(super) fn call_tool(
    params: &Value,
    state: &McpState,
) -> std::result::Result<Value, (i32, String)> {
    let name = params.get("name").and_then(Value::as_str).ok_or_else(|| {
        (
            -32602,
            "tools/call params.name must be a string".to_string(),
        )
    })?;
    let args = params.get("arguments").unwrap_or(&Value::Null);

    let result = match name {
        "get_project_summary" => get_project_summary(state),
        "list_nodes" => list_nodes(args, state),
        "find_nodes" => find_nodes(args, state),
        "get_lineage" => get_lineage(args, state),
        "get_impact" => get_impact(args, state),
        "get_column_lineage" => get_column_lineage(args, state),
        _ => return Err((-32602, format!("unknown tool: {name}"))),
    };

    Ok(match result {
        Ok(value) => tool_result(value, false, state),
        Err(err) => tool_result(json!({ "error": err.to_string() }), true, state),
    })
}

fn tool_result(mut value: Value, is_error: bool, state: &McpState) -> Value {
    if let Some(object) = value.as_object_mut() {
        let mut warnings = state
            .manifest_warnings
            .iter()
            .map(parser::manifest::ManifestDiagnostic::to_warning_json)
            .collect::<Vec<_>>();
        if let Some(warning) = &state.dialect_warning {
            // Dialect warnings predate structured manifest diagnostics and
            // are part of the existing MCP JSON contract.
            warnings.push(json!(warning));
        }
        if !warnings.is_empty() {
            object.insert("warnings".to_string(), Value::Array(warnings));
        }
    }
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": value,
        "isError": is_error
    })
}

pub(super) fn get_project_summary(state: &McpState) -> Result<Value> {
    let project_name = state
        .manifest
        .metadata
        .project_name
        .clone()
        .unwrap_or_else(|| "(unknown)".to_string());
    let manifest_status = match parser::project::DbtProject::load(&state.project_dir) {
        Ok(project) => check_manifest_freshness(
            &state.project_dir,
            Some(&state.manifest_path),
            &project,
            Some(&state.manifest),
        ),
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

pub(super) fn list_nodes(args: &Value, state: &McpState) -> Result<Value> {
    let query = match args.get("query") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.to_lowercase()),
        Some(_) => anyhow::bail!("argument 'query' must be a string"),
    };
    let type_filter = parse_node_type_filter(args)?;

    let mut nodes = Vec::new();
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
        nodes.push(node_summary(node));
    }
    nodes.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));

    Ok(json!({
        "nodes": nodes,
        "count": nodes.len()
    }))
}

pub(super) fn find_nodes(args: &Value, state: &McpState) -> Result<Value> {
    let names = required_string_array(args, "names")?;
    let type_filter = parse_node_type_filter(args)?;
    let sql_contents = state.manifest.collect_sql_contents();

    let mut nodes = Vec::new();
    let mut not_found = Vec::new();

    for name in names {
        match resolve_node_unique_id(state, &name)
            .and_then(|id| graph::filter::try_resolve_node_quiet(&state.dag, &id))
        {
            Some(idx) => {
                let node = &state.dag[idx];
                if type_filter
                    .as_ref()
                    .is_none_or(|f| f.contains(&node.node_type))
                {
                    let compiled_sql = sql_contents.get(&node.unique_id).map(String::as_str);
                    nodes.push(node_detail(node, compiled_sql));
                } else {
                    not_found.push(name);
                }
            }
            None => not_found.push(name),
        }
    }

    Ok(json!({
        "nodes": nodes,
        "count": nodes.len(),
        "not_found": not_found
    }))
}

fn parse_node_type_filter(args: &Value) -> Result<Option<HashSet<NodeType>>> {
    match args.get("node_types") {
        None | Some(Value::Null) => Ok(None),
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
            Ok(Some(set))
        }
        Some(_) => anyhow::bail!("argument 'node_types' must be an array"),
    }
}

fn node_type_from_str(s: &str) -> Option<NodeType> {
    match s {
        "model" => Some(NodeType::Model),
        "source" => Some(NodeType::Source),
        "seed" => Some(NodeType::Seed),
        "snapshot" => Some(NodeType::Snapshot),
        "test" => Some(NodeType::Test),
        "exposure" => Some(NodeType::Exposure),
        "semantic_model" => Some(NodeType::SemanticModel),
        "metric" => Some(NodeType::Metric),
        "saved_query" => Some(NodeType::SavedQuery),
        _ => None,
    }
}

/// Extract the first single-quoted token from an error `what` string and check
/// whether it exactly matches one of the given model names.
///
/// Global error messages use the format "... for 'model_name'" or
/// "'model_name': ...". Exact matching avoids false positives that substring
/// matching produces when one model name is a suffix of another (e.g. `orders`
/// matching `stg_orders`).
pub(super) fn error_names_upstream_model(what: &str, upstream_models: &HashSet<String>) -> bool {
    let start = match what.find('\'') {
        Some(i) => i + 1,
        None => return false,
    };
    let end = match what[start..].find('\'') {
        Some(i) => start + i,
        None => return false,
    };
    upstream_models.contains(&what[start..end])
}

pub(super) fn normalize_table_short_name(table: &str) -> String {
    let stripped: String = table.chars().filter(|c| *c != '"' && *c != '`').collect();
    stripped.rsplit('.').next().unwrap_or(&stripped).to_string()
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

fn node_summary(node: &graph::types::NodeData) -> Value {
    json!({
        "unique_id": node.unique_id,
        "name": node.label,
        "node_type": node.node_type.label(),
        "file_path": node.file_path.as_ref().map(|p| p.to_slash_lossy().into_owned()),
        "description": node.description,
        "materialization": node.materialization,
        "tags": node.tags
    })
}

fn node_detail(node: &graph::types::NodeData, compiled_sql: Option<&str>) -> Value {
    let mut value = node_summary(node);
    value["columns"] = json!(node.columns);
    value["compiled_sql"] = json!(compiled_sql);
    value
}

static GRAPH_NODE_FIELDS_SET: LazyLock<HashSet<String>> = LazyLock::new(|| {
    render::json::GRAPH_NODE_FIELDS
        .iter()
        .filter(|&&f| f != "sql_content")
        .map(|field| (*field).to_string())
        .collect()
});

pub(super) fn get_lineage(args: &Value, state: &McpState) -> Result<Value> {
    let models = required_string_array(args, "models")?;
    let upstream = optional_usize(args, "upstream_depth")?.or(Some(1));
    let downstream = optional_usize(args, "downstream_depth")?.or(Some(1));

    let normalized_models: Vec<Option<String>> = models
        .iter()
        .map(|model| resolve_node_unique_id(state, model))
        .collect();
    let not_found: Vec<String> = models
        .iter()
        .zip(normalized_models.iter())
        .filter(|(_, normalized)| normalized.is_none())
        .map(|(original, _)| original.clone())
        .collect();
    let resolved_models: Vec<String> = normalized_models.into_iter().flatten().collect();
    if resolved_models.is_empty() {
        return Ok(json!({
            "nodes": [],
            "edges": [],
            "not_found": not_found
        }));
    }

    let filtered = graph::filter::filter_graph(
        &state.dag,
        &resolved_models,
        upstream,
        downstream,
        &[],
        true,
    )?;
    let mut buf = Vec::new();
    render::json::render_json_to_writer(&filtered, None, &GRAPH_NODE_FIELDS_SET, &mut buf, false)?;
    let mut result: Value = serde_json::from_slice(&buf)?;
    result["not_found"] = json!(not_found);
    Ok(result)
}

pub(super) fn get_impact(args: &Value, state: &McpState) -> Result<Value> {
    let model = required_string(args, "model")?;
    let normalized = resolve_node_unique_id(state, model).unwrap_or_else(|| model.to_string());
    let idx = graph::filter::try_resolve_node_quiet(&state.dag, &normalized)
        .with_context(|| format!("model not found: {model}"))?;
    let report = graph::impact::compute_impact(&state.dag, idx);
    Ok(serde_json::to_value(report)?)
}

pub(super) fn get_column_lineage(args: &Value, state: &McpState) -> Result<Value> {
    let model = required_string(args, "model")?;
    let column = required_string(args, "column")?;
    let direction = required_string(args, "direction")?;
    let mut cache = state.column_lineage_cache.borrow_mut();
    let mut analysis = graph::column_lineage::ColumnLineageAnalysis::new(
        &state.manifest,
        state.dialect,
        &mut cache,
    );

    match direction {
        "upstream" => {
            let mut report = analysis.compute_cross_model_column_lineage(model);
            report.columns.retain(|entry| entry.column == column);
            if report.total_columns > 0 {
                // Collect the short names of models on the target column's lineage path.
                // Table names from SQL may be fully qualified (e.g. "db.schema.model"); take
                // the last dot-separated segment and strip quotes to get the model short name.
                let upstream_models: HashSet<String> = report
                    .columns
                    .iter()
                    .flat_map(|entry| {
                        entry.sources.iter().flat_map(|src| {
                            std::iter::once(normalize_table_short_name(&src.table))
                                .chain(src.model_path.iter().map(|(m, _, _)| m.clone()))
                        })
                    })
                    .collect();
                // Partition cross-model errors:
                // - Global errors (ParseFailure, NoCompiledCode, etc.) are kept only when
                //   their message references a model on the target column's lineage path.
                //   This prevents errors from models used by *other* columns from leaking in.
                // - Cross-model reports already rebase column-scoped diagnostics to the
                //   requested output column, so retain only that exact column identity.
                let mut cross_global_errors = Vec::new();
                let mut cross_column_errors = Vec::new();
                for err in report.errors.drain(..) {
                    if err.is_column_scoped() {
                        if err.column_name() == Some(column) && !cross_column_errors.contains(&err)
                        {
                            cross_column_errors.push(err);
                        }
                    } else if !upstream_models.is_empty()
                        && error_names_upstream_model(&err.what, &upstream_models)
                    {
                        cross_global_errors.push(err);
                    }
                }
                cross_column_errors.sort_by(|a, b| a.what.cmp(&b.what));

                // Query single-model lineage for global errors specific to the target model
                // itself (e.g. the target model's own ParseFailure or NoCompiledCode). The
                // cache populated during cross-model computation makes this a cheap hit.
                let target_lineage = analysis.compute_column_lineage(model);
                let target_global_errors: Vec<_> = target_lineage
                    .errors
                    .into_iter()
                    .filter(|err| !err.is_column_scoped())
                    .collect();

                let has_column_error = !cross_column_errors.is_empty();
                let has_global_errors =
                    !target_global_errors.is_empty() || !cross_global_errors.is_empty();

                report.errors = target_global_errors;
                report.errors.extend(cross_global_errors);
                report.errors.extend(cross_column_errors);

                report.traced_columns = report.columns.len();
                report.total_columns = 1;
                if report.columns.is_empty() && !has_column_error && !has_global_errors {
                    report
                        .errors
                        .push(graph::column_lineage::ColumnLineageError {
                            kind: graph::column_lineage::ColumnLineageErrorKind::ColumnNotFound,
                            column: Some(column.to_string()),
                            what: format!("column '{column}': not found in model output"),
                            why: None,
                            hint: None,
                        });
                }
            }
            Ok(serde_json::to_value(&report)?)
        }
        "downstream" => {
            let report = analysis.compute_column_impact(model, column);
            Ok(serde_json::to_value(&report)?)
        }
        _ => anyhow::bail!("direction must be 'upstream' or 'downstream'"),
    }
}

pub(super) fn resolve_node_unique_id(state: &McpState, name: &str) -> Option<String> {
    graph::filter::resolve_node_by_name(&state.dag, name)
        .ok()
        .map(|idx| state.dag[idx].unique_id.clone())
}

pub(super) fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("argument '{key}' is required and must be a string"))
}

pub(super) fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>> {
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

pub(super) fn required_string_array(args: &Value, key: &str) -> Result<Vec<String>> {
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
