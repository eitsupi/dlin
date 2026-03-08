use std::collections::HashMap;
use std::io::{IsTerminal, Write};

use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use serde::Serialize;

use crate::graph::types::*;

#[derive(Serialize)]
struct JsonGraph {
    nodes: Vec<JsonNode>,
    edges: Vec<JsonEdge>,
}

#[derive(Serialize)]
struct JsonNode {
    unique_id: String,
    label: String,
    node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    materialization: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    columns: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sql_content: Option<String>,
}

#[derive(Serialize)]
struct JsonEdge {
    source: String,
    target: String,
    edge_type: String,
}

/// Render the lineage graph as JSON to stdout.
/// Pretty-prints when stdout is a terminal, compact otherwise.
pub fn render_json(graph: &LineageGraph, sql_contents: Option<&HashMap<String, String>>) {
    let mut stdout = std::io::stdout().lock();
    let pretty = stdout.is_terminal();
    render_json_to_writer(graph, sql_contents, &mut stdout, pretty);
}

fn render_json_to_writer<W: Write>(
    graph: &LineageGraph,
    sql_contents: Option<&HashMap<String, String>>,
    w: &mut W,
    pretty: bool,
) {
    let mut nodes: Vec<JsonNode> = graph
        .node_indices()
        .map(|idx| {
            let node = &graph[idx];
            JsonNode {
                unique_id: node.unique_id.clone(),
                label: node.label.clone(),
                node_type: node.node_type.label().to_string(),
                file_path: node.file_path.as_ref().map(|p| p.to_string_lossy().into()),
                description: node.description.clone(),
                materialization: node.materialization.clone(),
                tags: node.tags.clone(),
                columns: node.columns.clone(),
                sql_content: sql_contents
                    .and_then(|m| m.get(&node.unique_id))
                    .cloned(),
            }
        })
        .collect();
    nodes.sort_unstable_by(|a, b| a.unique_id.cmp(&b.unique_id));

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
        serde_json::to_writer_pretty(&mut *w, &json_graph).unwrap();
    } else {
        serde_json::to_writer(&mut *w, &json_graph).unwrap();
    }
    writeln!(w).unwrap();
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

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_json_to_writer(graph, None, &mut buf, true);
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
        render_json_to_writer(&graph, Some(&sql_contents), &mut buf, true);
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
        render_json_to_writer(&graph, None, &mut buf, false);
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
}
