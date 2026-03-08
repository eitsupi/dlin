use std::collections::HashSet;
use std::io::{IsTerminal, Write};

use serde::Serialize;
use serde_json::Value;

use crate::cli::ListOutputFormat;
use crate::graph::types::*;

/// All available node fields for list JSON output.
pub const LIST_NODE_FIELDS: &[&str] = &["unique_id", "label", "node_type", "file_path"];

/// Resolve which fields to emit for list JSON, and validate field names.
pub fn resolve_list_fields(
    json_fields: Option<&[String]>,
    json_full: bool,
) -> Result<HashSet<String>, String> {
    if json_full {
        return Ok(LIST_NODE_FIELDS.iter().map(|s| (*s).to_string()).collect());
    }
    match json_fields {
        Some(fields) => {
            let known: HashSet<&str> = LIST_NODE_FIELDS.iter().copied().collect();
            let mut unknown: Vec<&str> = Vec::new();
            for f in fields {
                if !known.contains(f.as_str()) {
                    unknown.push(f);
                }
            }
            if !unknown.is_empty() {
                return Err(format!(
                    "unknown JSON field(s): {}. Available fields: {}",
                    unknown.join(", "),
                    LIST_NODE_FIELDS.join(", "),
                ));
            }
            Ok(fields.iter().cloned().collect())
        }
        None => Ok(LIST_NODE_FIELDS.iter().map(|s| (*s).to_string()).collect()),
    }
}

#[derive(Serialize)]
struct ListNode {
    unique_id: String,
    label: String,
    node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
}

/// Render node list to stdout.
pub fn render_list(graph: &LineageGraph, format: &ListOutputFormat, fields: &HashSet<String>) {
    let mut stdout = std::io::stdout().lock();
    match format {
        ListOutputFormat::Plain => render_list_plain(graph, &mut stdout),
        ListOutputFormat::Json => {
            let pretty = stdout.is_terminal();
            render_list_json(graph, fields, &mut stdout, pretty);
        }
    }
}

pub fn render_list_plain<W: Write>(graph: &LineageGraph, w: &mut W) {
    let mut entries: Vec<(&str, &str)> = graph
        .node_indices()
        .map(|idx| {
            let node = &graph[idx];
            (node.node_type.label(), node.label.as_str())
        })
        .collect();
    entries.sort_unstable();

    for (node_type, label) in entries {
        writeln!(w, "{}\t{}", node_type, label).unwrap();
    }
}

pub fn render_list_json<W: Write>(
    graph: &LineageGraph,
    fields: &HashSet<String>,
    w: &mut W,
    pretty: bool,
) {
    let all_fields: HashSet<String> = LIST_NODE_FIELDS.iter().map(|s| (*s).to_string()).collect();
    let use_all = *fields == all_fields;

    let mut nodes: Vec<(String, String, Value)> = graph
        .node_indices()
        .map(|idx| {
            let node = &graph[idx];
            let full = ListNode {
                unique_id: node.unique_id.clone(),
                label: node.label.clone(),
                node_type: node.node_type.label().to_string(),
                file_path: node.file_path.as_ref().map(|p| p.to_string_lossy().into()),
            };
            let sort_key_type = full.node_type.clone();
            let sort_key_label = full.label.clone();
            let value = if use_all {
                serde_json::to_value(&full).unwrap()
            } else {
                super::json::filter_fields(serde_json::to_value(&full).unwrap(), fields)
            };
            (sort_key_type, sort_key_label, value)
        })
        .collect();
    nodes.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let nodes: Vec<Value> = nodes.into_iter().map(|(_, _, v)| v).collect();

    if pretty {
        serde_json::to_writer_pretty(&mut *w, &nodes).unwrap();
    } else {
        serde_json::to_writer(&mut *w, &nodes).unwrap();
    }
    writeln!(w).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_fields() -> HashSet<String> {
        LIST_NODE_FIELDS.iter().map(|s| (*s).to_string()).collect()
    }

    fn make_node(unique_id: &str, label: &str, node_type: NodeType) -> NodeData {
        NodeData {
            unique_id: unique_id.into(),
            label: label.into(),
            node_type,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
        }
    }

    fn make_test_graph() -> LineageGraph {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        graph.add_node(make_node(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
        ));
        graph
    }

    #[test]
    fn test_plain_sorted_output() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_plain(&graph, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert_eq!(
            output,
            "model\torders\nmodel\tstg_orders\nsource\traw.orders\n"
        );
    }

    #[test]
    fn test_plain_empty_graph() {
        let graph = LineageGraph::new();
        let mut buf = Vec::new();
        render_list_plain(&graph, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn test_json_sorted_output() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, false);
        let output = String::from_utf8(buf).unwrap();

        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.len(), 3);
        // Sorted by type then label
        assert_eq!(parsed[0]["node_type"], "model");
        assert_eq!(parsed[0]["label"], "orders");
        assert_eq!(parsed[1]["node_type"], "model");
        assert_eq!(parsed[1]["label"], "stg_orders");
        assert_eq!(parsed[2]["node_type"], "source");
        assert_eq!(parsed[2]["label"], "raw.orders");
    }

    #[test]
    fn test_json_compact_single_line() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, false);
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "compact JSON should be a single line");
    }

    #[test]
    fn test_json_pretty_multi_line() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, true);
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert!(lines.len() > 1, "pretty JSON should be multi-line");
    }

    #[test]
    fn test_json_empty_graph() {
        let graph = LineageGraph::new();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, false);
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_json_has_unique_id() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, false);
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed[0]["unique_id"], "model.orders");
    }

    #[test]
    fn test_json_includes_file_path() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(std::path::PathBuf::from("models/orders.sql")),
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
        });
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, false);
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed[0]["file_path"], "models/orders.sql");
    }

    #[test]
    fn test_json_omits_null_file_path() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("source.raw.orders", "raw.orders", NodeType::Source));
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, false);
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert!(parsed[0].get("file_path").is_none());
    }

    #[test]
    fn test_snapshot_list_plain() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let mut buf = Vec::new();
        render_list_plain(&graph, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_list_json() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), &mut buf, true);
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }
}
