use std::collections::{HashMap, HashSet};
use std::io::{self, IsTerminal, Write};

use serde_json::Value;

use super::json::build_node_value;
use crate::ListOutputFormat;
use crate::graph::types::*;

/// Default node fields of list JSON: the graph defaults plus the dependency
/// analysis, so that a listed node shows whether its dependencies are known.
pub const LIST_DEFAULT_FIELDS: &[&str] = &[
    "unique_id",
    "label",
    "node_type",
    "file_path",
    "dependency_analysis",
];

/// Resolve which fields to emit for list JSON, and validate field names.
/// Uses the same field set as graph JSON output, with its own defaults.
pub fn resolve_list_fields(
    json_fields: Option<&[String]>,
    json_full: bool,
) -> Result<HashSet<String>, String> {
    if json_fields.is_none() && !json_full {
        return Ok(LIST_DEFAULT_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect());
    }
    super::json::resolve_graph_fields(json_fields, json_full)
}

/// Render node list to stdout.
pub fn render_list(
    graph: &LineageGraph,
    format: &ListOutputFormat,
    fields: &HashSet<String>,
    sql_contents: Option<&HashMap<String, String>>,
) {
    let mut stdout = std::io::stdout().lock();
    let result = match format {
        ListOutputFormat::Plain => render_list_plain(graph, &mut stdout),
        ListOutputFormat::Json => {
            let pretty = stdout.is_terminal();
            render_list_json(graph, fields, sql_contents, &mut stdout, pretty)
        }
    };
    super::handle_stdout_result(result);
}

pub fn render_list_plain<W: Write>(graph: &LineageGraph, w: &mut W) -> io::Result<()> {
    let mut entries: Vec<(&str, &str)> = graph
        .node_indices()
        .map(|idx| {
            let node = &graph[idx];
            (node.node_type.label(), node.label.as_str())
        })
        .collect();
    entries.sort_unstable();

    for (node_type, label) in entries {
        writeln!(w, "{}\t{}", node_type, label)?;
    }
    Ok(())
}

pub fn render_list_json<W: Write>(
    graph: &LineageGraph,
    fields: &HashSet<String>,
    sql_contents: Option<&HashMap<String, String>>,
    w: &mut W,
    pretty: bool,
) -> io::Result<()> {
    let mut nodes: Vec<(String, String, Value)> = graph
        .node_indices()
        .map(|idx| {
            let node = &graph[idx];
            let sort_key_type = node.node_type.label().to_string();
            let sort_key_label = node.label.clone();
            let value = build_node_value(node, fields, sql_contents);
            (sort_key_type, sort_key_label, value)
        })
        .collect();
    nodes.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let nodes: Vec<Value> = nodes.into_iter().map(|(_, _, v)| v).collect();

    if pretty {
        serde_json::to_writer_pretty(&mut *w, &nodes).map_err(super::serde_io_error)?;
    } else {
        serde_json::to_writer(&mut *w, &nodes).map_err(super::serde_io_error)?;
    }
    writeln!(w)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::test_helpers::make_node;

    fn all_fields() -> HashSet<String> {
        super::super::json::GRAPH_NODE_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    fn make_test_graph() -> LineageGraph {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        graph.add_node(make_node("model.stg_orders", "stg_orders", NodeType::Model));
        graph
    }

    #[test]
    fn test_plain_sorted_output() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_plain(&graph, &mut buf).unwrap();
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
        render_list_plain(&graph, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.is_empty());
    }

    #[test]
    fn test_json_sorted_output() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, false).unwrap();
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
        render_list_json(&graph, &all_fields(), None, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "compact JSON should be a single line");
    }

    #[test]
    fn test_json_pretty_multi_line() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert!(lines.len() > 1, "pretty JSON should be multi-line");
    }

    #[test]
    fn test_json_empty_graph() {
        let graph = LineageGraph::new();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_json_has_unique_id() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, false).unwrap();
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
            exposure: None,
            aliases: vec![],
            dependency_analysis: crate::graph::dependency::DependencyAnalysis::new([]),
        });
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed[0]["file_path"], "models/orders.sql");
    }

    #[test]
    fn test_json_null_file_path() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert!(parsed[0]["file_path"].is_null());
    }

    #[test]
    fn test_snapshot_list_plain() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let mut buf = Vec::new();
        render_list_plain(&graph, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_list_json() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let mut buf = Vec::new();
        render_list_json(&graph, &all_fields(), None, &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn default_list_fields_include_dependency_analysis() {
        let list = resolve_list_fields(None, false).unwrap();
        assert!(list.contains("dependency_analysis"));
        let graph = super::super::json::resolve_graph_fields(None, false).unwrap();
        assert!(!graph.contains("dependency_analysis"));
        let selected = resolve_list_fields(Some(&["unique_id".to_string()]), false).unwrap();
        assert_eq!(selected, HashSet::from(["unique_id".to_string()]));
        assert!(
            resolve_list_fields(None, true)
                .unwrap()
                .contains("dependency_analysis")
        );
    }

    #[test]
    fn json_emits_the_dependency_analysis() {
        let graph = make_test_graph();
        let mut buf = Vec::new();
        let fields = resolve_list_fields(None, false).unwrap();
        render_list_json(&graph, &fields, None, &mut buf, false).unwrap();
        let parsed: Value = serde_json::from_slice(&buf).unwrap();
        assert_eq!(
            parsed[0]["dependency_analysis"],
            serde_json::json!({"status": "complete", "unresolved": []})
        );
    }
}
