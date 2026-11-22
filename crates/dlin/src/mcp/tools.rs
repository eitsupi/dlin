use std::collections::{BTreeMap, HashSet};

use anyhow::{Context, Result};
use path_slash::PathExt as _;
use serde_json::{Value, json};

use super::{column_result, page::Page, protocol::McpState};
use crate::commands::{check_loaded_manifest_freshness, read_seed_schemas};
use dlin_core::graph;
use dlin_core::graph::types::NodeType;
use dlin_core::render;

pub(super) fn tools() -> Vec<Value> {
    let mut tools = vec![
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
            "description": "Look up one or more dbt nodes by name or unique ID. Returns node details and columns. Use include_sql to request compiled SQL.",
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
            "description": "Analyse the downstream impact of changing a model: returns the downstream nodes reached through the lineage edges, with their distance and a DAG severity based on node type and materialization. `exploration` says whether the reached set can miss nodes; dependencies read from a manifest are not assessed, so it is incomplete. An empty page of impacted nodes does not show that there is no impact; `proves_no_impact` does.",
            "inputSchema": {
                "type": "object",
                "properties": { "model": { "type": "string", "description": "Name or unique ID of the model whose downstream impact to analyse." } },
                "required": ["model"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "get_column_lineage",
            "description": "Trace a column upstream or downstream using compiled SQL. Partial results may omit sources. Possible impacts are unconfirmed, and incomplete exploration may miss targets.",
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
    ];
    for tool in &mut tools {
        let name = tool["name"].as_str().unwrap().to_owned();
        let default_limit = if name == "get_lineage" { 50 } else { 20 };
        let impact = matches!(name.as_str(), "get_impact" | "get_column_lineage");
        let properties = &mut tool["inputSchema"]["properties"];
        if name == "find_nodes" {
            properties["include_sql"] = json!({"type":"boolean","default":false,"description":"Include compiled SQL when available."});
            properties["names"]["maxItems"] = json!(20);
        } else if name != "get_project_summary" {
            properties["limit"] = json!({"type":"integer","minimum":1,"maximum":100,"default":default_limit,"description":"Maximum rows on this page."});
            properties["cursor"] = json!({"type":"string","description":"Use next_cursor from the previous page with the same query."});
        }
        if impact {
            properties["detail"] = json!({"type":"string","enum":["summary","full"],"default":"summary","description":"Full includes paths and supporting details. Both modes can be paginated."});
        }
    }
    tools
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
        Ok(value) => tool_result(
            value,
            false,
            (name == "get_column_lineage")
                .then_some(state.dialect_warning.as_deref())
                .flatten(),
        ),
        Err(err) => tool_result(json!({ "error": err.to_string() }), true, None),
    })
}

fn tool_result(mut value: Value, is_error: bool, warning: Option<&str>) -> Value {
    if let Some(warning) = warning {
        value["warnings"] = json!([warning]);
    }
    json!({
        "content": [{ "type": "text", "text": value.to_string() }],
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
    let freshness =
        check_loaded_manifest_freshness(&state.project_dir, &state.manifest_path, &state.manifest);
    let manifest_status = json!({
        "verification": freshness.verification(),
        "found": freshness.found,
        "is_stale": freshness.is_stale,
        "stale_file_count": freshness.stale_file_count,
        "deleted_file_count": freshness.deleted_file_count,
        "stale_files": freshness.stale_files.iter().take(3).collect::<Vec<_>>(),
        "deleted_files": freshness.deleted_files.iter().take(3).collect::<Vec<_>>(),
        "unverified_count": freshness.unverified.len(),
        "unverified": freshness.unverified.iter().take(3).collect::<Vec<_>>(),
    });
    let mut value = json!({
        "project_name": project_name,
        "source_mode": "manifest",
        "node_counts": render::summary::count_nodes(state.dag()),
        "edge_count": state.dag().edge_count(),
        "manifest_status": manifest_status,
    });
    value["skipped_unit_tests"] = json!(state.manifest.skipped_unit_test_count());
    let mut groups = BTreeMap::new();
    for diagnostic in &state.manifest_warnings {
        let key = (
            diagnostic.kind.as_str(),
            diagnostic.section.as_deref(),
            diagnostic.raw_type.as_deref(),
        );
        groups.entry(key).or_insert_with(Vec::new).push((
            diagnostic.raw_resource.as_deref(),
            diagnostic.message.as_str(),
        ));
    }
    value["manifest_diagnostics"] = json!(groups.into_iter().map(
        |((kind, section, resource_type), mut entries)| {
            entries.sort_unstable();
            json!({
                "kind": kind, "section": section, "resource_type": resource_type,
                "count": entries.len(), "examples": entries.iter().take(3)
                    .map(|(id, message)| json!({"id": id, "message": message})).collect::<Vec<_>>(),
            })
        }
    ).collect::<Vec<_>>());
    Ok(value)
}

pub(super) fn list_nodes(args: &Value, state: &McpState) -> Result<Value> {
    let query = match args.get("query") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.to_lowercase()),
        Some(_) => anyhow::bail!("argument 'query' must be a string"),
    };
    let type_filter = parse_node_type_filter(args)?;

    let mut nodes: Vec<_> = state
        .dag()
        .node_weights()
        .filter(|node| {
            node.node_type != NodeType::Phantom
                && type_filter
                    .as_ref()
                    .is_none_or(|filter| filter.contains(&node.node_type))
                && query
                    .as_deref()
                    .is_none_or(|query| node_matches_query(node, query))
        })
        .collect();
    nodes.sort_unstable_by_key(|node| (&node.label, &node.unique_id));
    let mut types: Vec<_> = type_filter
        .iter()
        .flatten()
        .map(|kind| kind.label())
        .collect();
    types.sort_unstable();
    let page = Page::new(
        args,
        20,
        nodes.len(),
        state.manifest_hash,
        ("list_nodes", &query, type_filter.is_some(), types),
    )?;
    let nodes: Vec<_> = nodes[page.range()].iter().map(|node| {
        let description = node.description.as_deref().map(|text| excerpt(text, 160));
        json!({
            "unique_id": node.unique_id, "name": node.label, "node_type": node.node_type.label(),
            "description": description, "file_path": node.file_path.as_ref().map(|path| path.to_slash_lossy()),
            "materialization": node.materialization, "tags": node.tags,
        })
    }).collect();
    Ok(json!({"nodes": nodes, "count": nodes.len(), "page": page.metadata()}))
}

/// Bound a text field without splitting a character.
pub(super) fn excerpt(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

pub(super) fn find_nodes(args: &Value, state: &McpState) -> Result<Value> {
    let names = required_string_array(args, "names")?;
    anyhow::ensure!(
        names.len() <= 20,
        "find_nodes accepts at most 20 names; split the request"
    );
    let type_filter = parse_node_type_filter(args)?;
    let include_sql = match args.get("include_sql") {
        None | Some(Value::Null) => false,
        Some(value) => value.as_bool().context("include_sql must be a boolean")?,
    };
    let sql_contents = include_sql.then(|| state.manifest.collect_sql_contents());

    let mut nodes = Vec::new();
    let mut not_found = Vec::new();

    for name in names {
        match resolve_node_unique_id(state, &name)
            .and_then(|id| graph::filter::try_resolve_node_quiet(state.dag(), &id))
        {
            Some(idx) => {
                let node = &state.dag()[idx];
                if type_filter
                    .as_ref()
                    .is_none_or(|f| f.contains(&node.node_type))
                {
                    let mut detail = node_summary(node);
                    detail["columns"] = json!(node.columns);
                    if let Some(sql) = &sql_contents {
                        detail["compiled_sql"] = json!(sql.get(&node.unique_id));
                    }
                    nodes.push(detail);
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
    let filtered = if resolved_models.is_empty() {
        Default::default()
    } else {
        graph::filter::filter_graph(
            state.dag(),
            &resolved_models,
            upstream,
            downstream,
            &[],
            true,
        )?
    };
    let mut edges: Vec<_> = filtered
        .edge_indices()
        .filter_map(|edge| {
            let (from, to) = filtered.edge_endpoints(edge)?;
            Some((from, to, &filtered[edge]))
        })
        .collect();
    edges.sort_by_key(|(from, to, edge)| {
        (
            &filtered[*from].unique_id,
            &filtered[*to].unique_id,
            edge.edge_type.label(),
            edge.collapsed_through,
        )
    });
    let page = Page::new(
        args,
        50,
        edges.len(),
        state.manifest_hash,
        (
            "get_lineage",
            &resolved_models,
            &not_found,
            upstream,
            downstream,
        ),
    )?;
    let mut included: HashSet<_> = filtered
        .node_indices()
        .filter(|idx| resolved_models.contains(&filtered[*idx].unique_id))
        .collect();
    let edges: Vec<_> = edges[page.range()]
        .iter()
        .map(|(from, to, edge)| {
            included.extend([*from, *to]);
            json!({"source": filtered[*from].unique_id, "target": filtered[*to].unique_id,
            "edge_type":edge.edge_type.label(), "collapsed_through":edge.collapsed_through})
        })
        .collect();
    let mut included: Vec<_> = included.into_iter().collect();
    included.sort_unstable_by_key(|idx| &filtered[*idx].unique_id);
    let nodes: Vec<_> = included.iter().map(|idx| {
        let node = &filtered[*idx];
        json!({"unique_id":node.unique_id, "name":node.label, "node_type":node.node_type.label()})
    }).collect();
    Ok(json!({"nodes":nodes, "edges":edges, "not_found":not_found, "page":page.metadata()}))
}

pub(super) fn get_impact(args: &Value, state: &McpState) -> Result<Value> {
    let model = required_string(args, "model")?;
    let normalized = resolve_node_unique_id(state, model).unwrap_or_else(|| model.to_string());
    let idx = graph::filter::try_resolve_node_quiet(state.dag(), &normalized)
        .with_context(|| format!("model not found: {model}"))?;
    let report = graph::impact::compute_impact(&state.build, idx)
        .with_context(|| format!("model not found: {model}"))?;
    let detail = column_result::Detail::parse(args)?;
    // Page through a reordered view; the report itself stays as computed.
    let mut ordered: Vec<&graph::impact::ImpactedNode> = report.impacted_nodes().iter().collect();
    ordered.sort_by_key(|node| {
        (
            node.distance,
            std::cmp::Reverse(node.severity),
            node.unique_id.clone(),
        )
    });
    let page = Page::new(
        args,
        20,
        ordered.len(),
        state.manifest_hash,
        ("get_impact", &normalized, detail),
    )?;
    let nodes = &ordered[page.range()];
    let mut value = json!({
        "source_model":report.source_model, "overall_severity":report.overall_severity,
        "affected_models":report.affected_models, "affected_tests":report.affected_tests,
        "affected_exposures":report.affected_exposures,
        "source_node":report.source_node(), "exploration":report.exploration(), "proves_no_impact":report.proves_no_impact(),
        "page":page.metadata(),
    });
    if detail.full() {
        let ids: HashSet<_> = nodes.iter().map(|node| node.unique_id.as_str()).collect();
        value["impacted_nodes"] = json!(nodes);
        value["exposure_paths"] = json!(
            report
                .exposure_paths
                .iter()
                .filter(|path| ids.contains(path.exposure_id.as_str()))
                .map(|path| json!({"exposure_id":path.exposure_id, "exposure":path.exposure, "path":path.path}))
                .collect::<Vec<_>>()
        );
        value["exposure_paths_truncated"] = json!(report.exposure_paths_truncated);
    } else {
        value["impacted_nodes"] = json!(
            nodes
                .iter()
                .map(|node| json!({
                    "unique_id":node.unique_id, "label":node.label, "node_type":node.node_type,
                    "distance":node.distance, "severity":node.severity,
                }))
                .collect::<Vec<_>>()
        );
    }
    Ok(value)
}

pub(super) fn get_column_lineage(args: &Value, state: &McpState) -> Result<Value> {
    let model = required_string(args, "model")?;
    let column = required_string(args, "column")?;
    let direction = required_string(args, "direction")?;
    let detail = column_result::Detail::parse(args)?;
    let seeds = read_seed_schemas(&state.project_dir, &state.manifest);
    let snapshot = super::page::fingerprint((state.manifest_hash, state.dialect, &seeds));
    let key = (snapshot, model, column, direction, detail);
    let mut cache = state.column_lineage_cache.borrow_mut();
    let mut analysis = graph::column_lineage::ColumnLineageAnalysis::new(
        &state.manifest,
        state.dialect,
        &mut cache,
    )
    .with_seed_schemas(&seeds);

    match direction {
        "upstream" => {
            let mut report = analysis.compute_cross_model_column_lineage(model);
            let cross_errors = std::mem::take(&mut report.errors);
            report.select_columns(&[column], state.dialect);
            // With the diagnostics cleared, what remains are the findings of the
            // selection itself: the name was not found, or is ambiguous.
            let lookup_errors = std::mem::take(&mut report.errors);
            if report.output.is_none() {
                report.errors = cross_errors;
            } else {
                // Collect the short names of models on the target column's lineage path.
                // Table names from SQL may be fully qualified (e.g. "db.schema.model"); take
                // the last dot-separated segment and strip quotes to get the model short name.
                let selected = report.output.iter().flat_map(|output| &output.columns);
                let mut upstream_models: HashSet<String> = selected
                    .clone()
                    .flat_map(|slot| {
                        slot.lineage.known().iter().flat_map(|src| {
                            std::iter::once(normalize_table_short_name(&src.table))
                                .chain(src.model_path.iter().map(|hop| hop.output.model.clone()))
                        })
                    })
                    .collect();
                // Failed resources are boundaries rather than known sources.
                // Column selection has already kept only the boundaries of
                // the requested root outputs.
                for boundary in &report.boundaries {
                    upstream_models.insert(boundary.resource.name.clone());
                    upstream_models.extend(
                        boundary
                            .model_path
                            .iter()
                            .map(|hop| hop.output.model.clone()),
                    );
                }
                let selected_slots: HashSet<_> = selected.map(|slot| slot.id).collect();
                // Partition cross-model errors:
                // - Global errors (ParseFailure, NoCompiledCode, etc.) are kept only when
                //   their message references a model on the target column's lineage path.
                //   This prevents errors from models used by *other* columns from leaking in.
                // - Cross-model reports already rebase slot diagnostics to the
                //   requested output slot, so retain only that exact slot identity.
                let mut cross_global_errors = Vec::new();
                let mut cross_column_errors = Vec::new();
                for err in cross_errors {
                    if err.is_column_scoped() {
                        if err
                            .slot()
                            .is_some_and(|slot| selected_slots.contains(&slot))
                            && !cross_column_errors.contains(&err)
                        {
                            cross_column_errors.push(err);
                        }
                    } else if err.is_model_failure()
                        && !upstream_models.is_empty()
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

                let explained = !cross_column_errors.is_empty()
                    || target_global_errors
                        .iter()
                        .chain(&cross_global_errors)
                        .any(|err| err.is_model_failure());

                report.errors = target_global_errors;
                report.errors.extend(cross_global_errors);
                report.errors.extend(cross_column_errors);
                report
                    .errors
                    .extend(lookup_errors.into_iter().filter(|err| {
                        !explained
                            || err.kind
                                == graph::column_lineage::ColumnLineageErrorKind::ColumnAmbiguous
                    }));
            }
            column_result::upstream(args, key, &report)
        }
        "downstream" => {
            let report = analysis.compute_column_impact(
                model,
                column,
                graph::column_lineage::ImpactScope::ValueOnly,
            );
            column_result::downstream(args, key, &report)
        }
        _ => anyhow::bail!("direction must be 'upstream' or 'downstream'"),
    }
}

pub(super) fn resolve_node_unique_id(state: &McpState, name: &str) -> Option<String> {
    graph::filter::resolve_node_by_name(state.dag(), name)
        .ok()
        .map(|idx| state.dag()[idx].unique_id.clone())
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
