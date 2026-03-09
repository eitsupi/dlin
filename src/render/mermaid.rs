use std::io::{self, Write};

use petgraph::visit::{EdgeRef, IntoEdgeReferences};

use crate::graph::types::*;

/// Render the lineage graph as a Mermaid flowchart to stdout
pub fn render_mermaid(graph: &LineageGraph) {
    super::handle_stdout_result(render_mermaid_to_writer(graph, &mut std::io::stdout().lock()));
}

fn render_mermaid_to_writer<W: Write>(graph: &LineageGraph, w: &mut W) -> io::Result<()> {
    writeln!(w, "flowchart LR")?;

    if graph.node_count() == 0 {
        return Ok(());
    }

    // Collect and sort nodes by unique_id
    let mut nodes: Vec<_> = graph.node_indices().map(|idx| &graph[idx]).collect();
    nodes.sort_by_key(|n| &n.unique_id);

    // Render nodes with type-specific shapes
    for node in &nodes {
        let id = mermaid_id(&node.unique_id);
        let label = &node.label;
        let shape = match node.node_type {
            NodeType::Model => format!("{}[\"{}\"]", id, label),
            NodeType::Source => format!("{}([\"{}\"])", id, label),
            NodeType::Seed => format!("{}[/\"{}\"\\]", id, label),
            NodeType::Snapshot => format!("{}{{{{\"{}\"}}}}",  id, label),
            NodeType::Test => format!("{}{{\"{}\"}}", id, label),
            NodeType::Exposure => format!("{}>\"{}\"]", id, label),
            NodeType::Phantom => format!("{}(\"{}\")", id, label),
        };
        writeln!(w, "    {}", shape)?;
    }

    writeln!(w)?;

    // Collect and sort edges by (source_id, target_id, edge_type)
    let mut edges: Vec<_> = graph.edge_references().map(|edge| {
        let source = &graph[edge.source()];
        let target = &graph[edge.target()];
        (mermaid_id(&source.unique_id), mermaid_id(&target.unique_id), edge.weight().edge_type.clone())
    }).collect();
    edges.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));

    // Render edges
    for (src_id, tgt_id, edge_type) in &edges {
        let arrow = match edge_type {
            EdgeType::Ref => format!("    {} -->|ref| {}", src_id, tgt_id),
            EdgeType::Source => format!("    {} -.->|source| {}", src_id, tgt_id),
            EdgeType::Test => format!("    {} -.->|test| {}", src_id, tgt_id),
            EdgeType::Exposure => format!("    {} ==>|exposure| {}", src_id, tgt_id),
        };
        writeln!(w, "{}", arrow)?;
    }

    writeln!(w)?;

    // Collect used node types
    let mut used_types: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for node in &nodes {
        used_types.insert(node.node_type.label());
    }

    // Style classes — only for types present in the graph
    let class_defs = [
        ("model", "fill:#4A90D9,stroke:#333,color:#fff"),
        ("source", "fill:#27AE60,stroke:#333,color:#fff"),
        ("seed", "fill:#F39C12,stroke:#333,color:#fff"),
        ("snapshot", "fill:#8E44AD,stroke:#333,color:#fff"),
        ("test", "fill:#1ABC9C,stroke:#333,color:#fff"),
        ("exposure", "fill:#E74C3C,stroke:#333,color:#fff"),
        ("phantom", "fill:#BDC3C7,stroke:#333,color:#000"),
    ];
    for (name, style) in &class_defs {
        if used_types.contains(name) {
            writeln!(w, "    classDef {} {}", name, style)?;
        }
    }

    // Apply classes (sorted by unique_id via nodes)
    for node in &nodes {
        let id = mermaid_id(&node.unique_id);
        let class = node.node_type.label();
        writeln!(w, "    class {} {}", id, class)?;
    }
    Ok(())
}

/// Convert a unique_id to a valid Mermaid node ID (replace dots with underscores)
fn mermaid_id(unique_id: &str) -> String {
    unique_id.replace('.', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        render_mermaid_to_writer(graph, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_empty_graph() {
        let graph = LineageGraph::new();
        let output = render_to_string(&graph);
        assert!(output.contains("flowchart LR"));
        assert!(!output.contains("classDef")); // no nodes = no style section
    }

    #[test]
    fn test_single_model_node() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let output = render_to_string(&graph);
        assert!(output.contains("flowchart LR"));
        assert!(output.contains("model_orders[\"orders\"]"));
        assert!(output.contains("class model_orders model"));
    }

    #[test]
    fn test_source_node_shape() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let output = render_to_string(&graph);
        assert!(output.contains("source_raw_orders([\"raw.orders\"])"));
    }

    #[test]
    fn test_edge_styles() {
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
        assert!(output.contains("-.->|source|"));
    }

    #[test]
    fn test_ref_edge() {
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
        assert!(output.contains("-->|ref|"));
    }

    #[test]
    fn test_exposure_edge() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("exposure.dash", "dash", NodeType::Exposure));
        graph.add_edge(
            a,
            b,
            EdgeData {
                edge_type: EdgeType::Exposure,
            },
        );

        let output = render_to_string(&graph);
        assert!(output.contains("==>|exposure|"));
    }

    #[test]
    fn test_mermaid_id() {
        assert_eq!(mermaid_id("model.orders"), "model_orders");
        assert_eq!(mermaid_id("source.raw.orders"), "source_raw_orders");
    }

    #[test]
    fn test_test_edge() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let t = graph.add_node(make_node("test.t", "t", NodeType::Test));
        graph.add_edge(
            a,
            t,
            EdgeData {
                edge_type: EdgeType::Test,
            },
        );

        let output = render_to_string(&graph);
        assert!(output.contains("-.->|test|"));
    }

    #[test]
    fn test_style_classes_only_used_types() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        let output = render_to_string(&graph);
        // Only model classDef should be present
        assert!(output.contains("classDef model fill:#4A90D9"));
        assert!(!output.contains("classDef source"));
        assert!(!output.contains("classDef seed"));
        assert!(!output.contains("classDef snapshot"));
        assert!(!output.contains("classDef test"));
        assert!(!output.contains("classDef exposure"));
        assert!(!output.contains("classDef phantom"));
    }

    #[test]
    fn test_style_classes_multiple_types() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        graph.add_node(make_node("source.raw.b", "raw.b", NodeType::Source));
        let output = render_to_string(&graph);
        assert!(output.contains("classDef model fill:#4A90D9"));
        assert!(output.contains("classDef source fill:#27AE60"));
        assert!(!output.contains("classDef seed"));
    }

    #[test]
    fn test_snapshot_lineage() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_all_node_shapes() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        graph.add_node(make_node("source.a.b", "a.b", NodeType::Source));
        graph.add_node(make_node("seed.a", "a", NodeType::Seed));
        graph.add_node(make_node("snapshot.a", "a", NodeType::Snapshot));
        graph.add_node(make_node("test.a", "a", NodeType::Test));
        graph.add_node(make_node("exposure.a", "a", NodeType::Exposure));
        graph.add_node(make_node("model.unknown", "unknown", NodeType::Phantom));

        let output = render_to_string(&graph);
        // Model: [" "]
        assert!(output.contains("model_a[\"a\"]"));
        // Source: ([""])
        assert!(output.contains("source_a_b([\"a.b\"])"));
        // Seed: [/""\]
        assert!(output.contains("seed_a[/\"a\"\\]"));
        // Snapshot: {{"}}
        assert!(output.contains("snapshot_a{{\"a\"}}"));
        // Test: {""}
        assert!(output.contains("test_a{\"a\"}"));
        // Exposure: >""]
        assert!(output.contains("exposure_a>\"a\"]"));
        // Phantom: ("")
        assert!(output.contains("model_unknown(\"unknown\")"));
    }
}
