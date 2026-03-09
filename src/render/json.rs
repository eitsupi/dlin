use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};

use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use serde::Serialize;
use serde_json::Value;

use crate::graph::types::*;

/// All available node fields for graph JSON output.
pub const GRAPH_NODE_FIELDS: &[&str] = &[
    "unique_id",
    "label",
    "node_type",
    "file_path",
    "description",
    "materialization",
    "tags",
    "columns",
    "sql_content",
];

/// Default node fields when neither --json-fields nor --json-full is specified.
pub const GRAPH_DEFAULT_FIELDS: &[&str] = &["unique_id", "label", "node_type", "file_path"];

/// Resolve which fields to emit, and validate field names.
/// Returns `Err` with a message listing available fields if any name is unknown.
pub fn resolve_graph_fields(
    json_fields: Option<&[String]>,
    json_full: bool,
) -> Result<HashSet<String>, String> {
    if json_full {
        return Ok(GRAPH_NODE_FIELDS.iter().map(|s| (*s).to_string()).collect());
    }
    match json_fields {
        Some(fields) => {
            let known: HashSet<&str> = GRAPH_NODE_FIELDS.iter().copied().collect();
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
                    GRAPH_NODE_FIELDS.join(", "),
                ));
            }
            Ok(fields.iter().cloned().collect())
        }
        None => Ok(GRAPH_DEFAULT_FIELDS
            .iter()
            .map(|s| (*s).to_string())
            .collect()),
    }
}

#[derive(Serialize)]
struct JsonGraph {
    nodes: Vec<Value>,
    edges: Vec<JsonEdge>,
}

#[derive(Serialize)]
struct JsonEdge {
    source: String,
    target: String,
    edge_type: String,
}

/// Build a JSON object containing only the requested fields for a node.
pub fn build_node_value(
    node: &NodeData,
    fields: &HashSet<String>,
    sql_contents: Option<&HashMap<String, String>>,
) -> Value {
    let mut map = serde_json::Map::new();
    if fields.contains("unique_id") {
        map.insert("unique_id".into(), Value::String(node.unique_id.clone()));
    }
    if fields.contains("label") {
        map.insert("label".into(), Value::String(node.label.clone()));
    }
    if fields.contains("node_type") {
        map.insert(
            "node_type".into(),
            Value::String(node.node_type.label().to_string()),
        );
    }
    if fields.contains("file_path") {
        if let Some(ref p) = node.file_path {
            map.insert(
                "file_path".into(),
                Value::String(p.to_string_lossy().into()),
            );
        }
    }
    if fields.contains("description") {
        if let Some(ref d) = node.description {
            map.insert("description".into(), Value::String(d.clone()));
        }
    }
    if fields.contains("materialization") {
        if let Some(ref m) = node.materialization {
            map.insert("materialization".into(), Value::String(m.clone()));
        }
    }
    if fields.contains("tags") && !node.tags.is_empty() {
        map.insert(
            "tags".into(),
            Value::Array(node.tags.iter().map(|t| Value::String(t.clone())).collect()),
        );
    }
    if fields.contains("columns") && !node.columns.is_empty() {
        map.insert(
            "columns".into(),
            Value::Array(
                node.columns
                    .iter()
                    .map(|c| Value::String(c.clone()))
                    .collect(),
            ),
        );
    }
    if fields.contains("sql_content") {
        if let Some(sql) = sql_contents.and_then(|m| m.get(&node.unique_id)) {
            map.insert("sql_content".into(), Value::String(sql.clone()));
        }
    }
    Value::Object(map)
}

/// Render the lineage graph as JSON to stdout.
/// Pretty-prints when stdout is a terminal, compact otherwise.
pub fn render_json(
    graph: &LineageGraph,
    sql_contents: Option<&HashMap<String, String>>,
    fields: &HashSet<String>,
) {
    let mut stdout = std::io::stdout().lock();
    let pretty = stdout.is_terminal();
    super::handle_stdout_result(render_json_to_writer(graph, sql_contents, fields, &mut stdout, pretty));
}

fn render_json_to_writer<W: Write>(
    graph: &LineageGraph,
    sql_contents: Option<&HashMap<String, String>>,
    fields: &HashSet<String>,
    w: &mut W,
    pretty: bool,
) -> std::io::Result<()> {
    let mut nodes: Vec<(String, Value)> = graph
        .node_indices()
        .map(|idx| {
            let node = &graph[idx];
            let sort_key = node.unique_id.clone();
            let value = build_node_value(node, fields, sql_contents);
            (sort_key, value)
        })
        .collect();
    nodes.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let nodes: Vec<Value> = nodes.into_iter().map(|(_, v)| v).collect();

    let mut edges: Vec<JsonEdge> = graph
        .edge_references()
        .map(|edge| {
            let source = &graph[edge.source()];
            let target = &graph[edge.target()];
            JsonEdge {
                source: source.unique_id.clone(),
                target: target.unique_id.clone(),
                edge_type: edge_type_label(edge.weight().edge_type),
            }
        })
        .collect();
    edges.sort_unstable_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then(a.target.cmp(&b.target))
            .then(a.edge_type.cmp(&b.edge_type))
    });

    let json_graph = JsonGraph { nodes, edges };
    if pretty {
        serde_json::to_writer_pretty(&mut *w, &json_graph).map_err(super::serde_io_error)?;
    } else {
        serde_json::to_writer(&mut *w, &json_graph).map_err(super::serde_io_error)?;
    }
    writeln!(w)?;
    Ok(())
}

fn edge_type_label(edge_type: EdgeType) -> String {
    match edge_type {
        EdgeType::Ref => "ref",
        EdgeType::Source => "source",
        EdgeType::Test => "test",
        EdgeType::Exposure => "exposure",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn all_fields() -> HashSet<String> {
        GRAPH_NODE_FIELDS.iter().map(|s| (*s).to_string()).collect()
    }

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_json_to_writer(graph, None, &all_fields(), &mut buf, true).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_empty_graph() {
        let graph = LineageGraph::new();
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["nodes"].as_array().unwrap().len(), 0);
        assert_eq!(parsed["edges"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_single_node() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let nodes = parsed["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0]["unique_id"], "model.orders");
        assert_eq!(nodes[0]["label"], "orders");
        assert_eq!(nodes[0]["node_type"], "model");
        assert!(nodes[0].get("file_path").is_none());
        assert!(nodes[0].get("description").is_none());
    }

    #[test]
    fn test_node_with_file_path_and_description() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(PathBuf::from("models/orders.sql")),
            description: Some("Orders mart model".into()),
            materialization: None,
            tags: vec![],
            columns: vec![],
        });
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let nodes = parsed["nodes"].as_array().unwrap();
        assert_eq!(nodes[0]["file_path"], "models/orders.sql");
        assert_eq!(nodes[0]["description"], "Orders mart model");
    }

    #[test]
    fn test_edges() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let b = graph.add_node(make_node("model.stg_orders", "stg_orders", NodeType::Model));
        graph.add_edge(
            a,
            b,
            EdgeData {
                edge_type: EdgeType::Source,
            },
        );

        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let edges = parsed["edges"].as_array().unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0]["source"], "source.raw.orders");
        assert_eq!(edges[0]["target"], "model.stg_orders");
        assert_eq!(edges[0]["edge_type"], "source");
    }

    #[test]
    fn test_all_edge_types() {
        assert_eq!(edge_type_label(EdgeType::Ref), "ref");
        assert_eq!(edge_type_label(EdgeType::Source), "source");
        assert_eq!(edge_type_label(EdgeType::Test), "test");
        assert_eq!(edge_type_label(EdgeType::Exposure), "exposure");
    }

    #[test]
    fn test_all_node_types() {
        let mut graph = LineageGraph::new();
        let types = [
            ("model.a", NodeType::Model, "model"),
            ("source.a.b", NodeType::Source, "source"),
            ("seed.a", NodeType::Seed, "seed"),
            ("snapshot.a", NodeType::Snapshot, "snapshot"),
            ("test.a", NodeType::Test, "test"),
            ("exposure.a", NodeType::Exposure, "exposure"),
            ("model.unknown", NodeType::Phantom, "phantom"),
        ];
        for (id, nt, _) in &types {
            graph.add_node(make_node(id, "a", *nt));
        }
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let nodes = parsed["nodes"].as_array().unwrap();
        // Nodes are sorted by unique_id; verify all expected types are present
        let mut actual: Vec<(&str, &str)> = nodes
            .iter()
            .map(|n| (n["unique_id"].as_str().unwrap(), n["node_type"].as_str().unwrap()))
            .collect();
        actual.sort();
        let mut expected: Vec<(&str, &str)> = types.iter().map(|(id, _, t)| (*id, *t)).collect();
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_deterministic_node_order() {
        let mut graph = LineageGraph::new();
        // Add nodes in reverse alphabetical order
        graph.add_node(make_node("model.z_last", "z_last", NodeType::Model));
        graph.add_node(make_node("model.a_first", "a_first", NodeType::Model));
        graph.add_node(make_node("model.m_middle", "m_middle", NodeType::Model));
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let nodes = parsed["nodes"].as_array().unwrap();
        assert_eq!(nodes[0]["unique_id"], "model.a_first");
        assert_eq!(nodes[1]["unique_id"], "model.m_middle");
        assert_eq!(nodes[2]["unique_id"], "model.z_last");
    }

    #[test]
    fn test_deterministic_edge_order() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        let c = graph.add_node(make_node("model.c", "c", NodeType::Model));
        // Add edges in reverse order
        graph.add_edge(c, a, EdgeData { edge_type: EdgeType::Ref });
        graph.add_edge(a, b, EdgeData { edge_type: EdgeType::Ref });
        graph.add_edge(a, c, EdgeData { edge_type: EdgeType::Ref });
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let edges = parsed["edges"].as_array().unwrap();
        // Sorted by (source, target)
        assert_eq!(edges[0]["source"], "model.a");
        assert_eq!(edges[0]["target"], "model.b");
        assert_eq!(edges[1]["source"], "model.a");
        assert_eq!(edges[1]["target"], "model.c");
        assert_eq!(edges[2]["source"], "model.c");
        assert_eq!(edges[2]["target"], "model.a");
    }

    #[test]
    fn test_valid_json() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_edge(
            a,
            b,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        let output = render_to_string(&graph);
        // Should parse as valid JSON
        let _: serde_json::Value = serde_json::from_str(&output).unwrap();
    }

    #[test]
    fn test_snapshot_lineage() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_node_metadata() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(PathBuf::from("models/orders.sql")),
            description: Some("Orders mart model".into()),
            materialization: Some("table".into()),
            tags: vec!["daily".into(), "core".into()],
            columns: vec!["order_id".into(), "customer_id".into()],
        });
        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_json_with_sql() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(PathBuf::from("models/orders.sql")),
            description: None,
            materialization: Some("table".into()),
            tags: vec![],
            columns: vec![],
        });
        graph.add_node(make_node("source.raw.orders", "raw.orders", NodeType::Source));
        let sql_contents = HashMap::from([
            ("model.orders".to_string(), "SELECT * FROM {{ ref('stg_orders') }}".to_string()),
        ]);
        let mut buf = Vec::new();
        render_json_to_writer(&graph, Some(&sql_contents), &all_fields(), &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_compact_json_single_line() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_edge(a, b, EdgeData { edge_type: EdgeType::Ref });
        let mut buf = Vec::new();
        render_json_to_writer(&graph, None, &all_fields(), &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "compact JSON should be a single line");
        let _: serde_json::Value = serde_json::from_str(&output).unwrap();
    }

    #[test]
    fn test_node_with_materialization_tags_columns() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: None,
            description: None,
            materialization: Some("table".into()),
            tags: vec!["daily".into(), "core".into()],
            columns: vec!["order_id".into(), "customer_id".into()],
        });
        let output = render_to_string(&graph);
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let node = &parsed["nodes"][0];
        assert_eq!(node["materialization"], "table");
        assert_eq!(node["tags"][0], "daily");
        assert_eq!(node["tags"][1], "core");
        assert_eq!(node["columns"][0], "order_id");
        assert_eq!(node["columns"][1], "customer_id");
    }

    // -- Field filtering tests ------------------------------------------------

    #[test]
    fn test_default_fields_only() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(PathBuf::from("models/orders.sql")),
            description: Some("desc".into()),
            materialization: Some("table".into()),
            tags: vec!["daily".into()],
            columns: vec!["id".into()],
        });
        let fields = resolve_graph_fields(None, false).unwrap();
        let mut buf = Vec::new();
        render_json_to_writer(&graph, None, &fields, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let node = &parsed["nodes"][0];
        // Default fields present
        assert_eq!(node["unique_id"], "model.orders");
        assert_eq!(node["label"], "orders");
        assert_eq!(node["node_type"], "model");
        assert_eq!(node["file_path"], "models/orders.sql");
        // Non-default fields absent
        assert!(node.get("description").is_none());
        assert!(node.get("materialization").is_none());
        assert!(node.get("tags").is_none());
        assert!(node.get("columns").is_none());
    }

    #[test]
    fn test_custom_fields() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(PathBuf::from("models/orders.sql")),
            description: Some("desc".into()),
            materialization: Some("table".into()),
            tags: vec![],
            columns: vec![],
        });
        let fields = resolve_graph_fields(
            Some(&["unique_id".into(), "description".into()]),
            false,
        ).unwrap();
        let mut buf = Vec::new();
        render_json_to_writer(&graph, None, &fields, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let node = &parsed["nodes"][0];
        assert_eq!(node["unique_id"], "model.orders");
        assert_eq!(node["description"], "desc");
        // Other fields absent
        assert!(node.get("label").is_none());
        assert!(node.get("node_type").is_none());
        assert!(node.get("file_path").is_none());
    }

    #[test]
    fn test_json_full_includes_all() {
        let mut graph = LineageGraph::new();
        graph.add_node(NodeData {
            unique_id: "model.orders".into(),
            label: "orders".into(),
            node_type: NodeType::Model,
            file_path: Some(PathBuf::from("models/orders.sql")),
            description: Some("desc".into()),
            materialization: Some("table".into()),
            tags: vec!["daily".into()],
            columns: vec!["id".into()],
        });
        let fields = resolve_graph_fields(None, true).unwrap();
        let mut buf = Vec::new();
        render_json_to_writer(&graph, None, &fields, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let node = &parsed["nodes"][0];
        assert_eq!(node["description"], "desc");
        assert_eq!(node["materialization"], "table");
        assert_eq!(node["tags"][0], "daily");
        assert_eq!(node["columns"][0], "id");
    }

    #[test]
    fn test_unknown_field_error() {
        let result = resolve_graph_fields(
            Some(&["unique_id".into(), "nonexistent".into()]),
            false,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("nonexistent"));
        assert!(err.contains("Available fields"));
    }
}
