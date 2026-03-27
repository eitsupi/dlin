use std::collections::BTreeMap;
use std::io::{self, Write};

use petgraph::visit::{EdgeRef, IntoEdgeReferences};

use crate::cli::GroupBy;
use crate::graph::types::*;

/// Render the lineage graph as Graphviz DOT format to stdout
pub fn render_dot(graph: &LineageGraph, group_by: Option<GroupBy>) {
    super::handle_stdout_result(render_dot_to_writer(
        graph,
        &mut std::io::stdout().lock(),
        group_by,
    ));
}

fn render_dot_to_writer<W: Write>(
    graph: &LineageGraph,
    w: &mut W,
    group_by: Option<GroupBy>,
) -> io::Result<()> {
    writeln!(w, "digraph dbt_lineage {{")?;
    writeln!(w, "  rankdir=LR;")?;
    writeln!(
        w,
        r#"  node [shape=box, style=filled, fontname="Helvetica"];"#
    )?;
    writeln!(w)?;

    match group_by {
        Some(GroupBy::NodeType) => write_nodes_grouped(w, graph)?,
        None => write_nodes_flat(w, graph)?,
    }

    writeln!(w)?;

    // Render edges
    for edge in graph.edge_references() {
        let source = &graph[edge.source()];
        let target = &graph[edge.target()];
        let ed = edge.weight();
        let style = match (&ed.edge_type, ed.collapsed_through.is_some()) {
            (EdgeType::Ref, false) => "",
            (EdgeType::Ref, true) => ", style=dashed",
            (EdgeType::Source, false) => ", style=dashed",
            (EdgeType::Source, true) => r#", style="dashed,bold""#,
            (EdgeType::Test, false) => ", style=dotted",
            (EdgeType::Test, true) => r#", style="dotted,dashed""#,
            (EdgeType::Exposure, false) => ", style=bold",
            (EdgeType::Exposure, true) => r#", style="bold,dashed""#,
        };
        let label = match ed.collapsed_through {
            Some(n) => format!("{} (via {})", ed.edge_type.label(), n),
            None => ed.edge_type.label().to_string(),
        };
        writeln!(
            w,
            r#"  "{}" -> "{}" [label="{label}"{style}];"#,
            source.unique_id, target.unique_id,
        )?;
    }

    writeln!(w, "}}")?;
    Ok(())
}

/// Write nodes without grouping (flat list)
fn write_nodes_flat<W: Write>(w: &mut W, graph: &LineageGraph) -> io::Result<()> {
    for idx in graph.node_indices() {
        let node = &graph[idx];
        write_node(w, node, "  ")?;
    }
    Ok(())
}

/// Write nodes grouped by node type using DOT subgraph clusters
fn write_nodes_grouped<W: Write>(w: &mut W, graph: &LineageGraph) -> io::Result<()> {
    // Group nodes by node type
    let mut groups: BTreeMap<&str, Vec<&NodeData>> = BTreeMap::new();
    for idx in graph.node_indices() {
        let node = &graph[idx];
        groups.entry(node.node_type.label()).or_default().push(node);
    }

    for (type_label, group_nodes) in &groups {
        let (bg_color, _) = node_colors_for_label(type_label);
        let title = capitalize(type_label);
        writeln!(w, r#"  subgraph cluster_{type_label} {{"#)?;
        writeln!(w, r#"    label="{title}";"#)?;
        writeln!(w, "    style=rounded;")?;
        writeln!(w, r#"    color="{bg_color}";"#)?;
        writeln!(w)?;
        for node in group_nodes {
            write_node(w, node, "    ")?;
        }
        writeln!(w, "  }}")?;
    }
    Ok(())
}

/// Write a single node definition
fn write_node<W: Write>(w: &mut W, node: &NodeData, indent: &str) -> io::Result<()> {
    let (color, fontcolor) = node_colors(node.node_type);
    let label = node.display_name();
    writeln!(
        w,
        r#"{indent}"{}" [label="{label}", fillcolor="{color}", fontcolor="{fontcolor}"];"#,
        node.unique_id,
    )
}

fn node_colors(node_type: NodeType) -> (&'static str, &'static str) {
    match node_type {
        NodeType::Model => ("#4A90D9", "white"),
        NodeType::Source => ("#27AE60", "white"),
        NodeType::Seed => ("#F39C12", "white"),
        NodeType::Snapshot => ("#8E44AD", "white"),
        NodeType::Test => ("#1ABC9C", "white"),
        NodeType::Exposure => ("#E74C3C", "white"),
        NodeType::Phantom => ("#BDC3C7", "black"),
    }
}

/// Look up node colors by type label string (for cluster border color)
fn node_colors_for_label(label: &str) -> (&'static str, &'static str) {
    match label {
        "model" => node_colors(NodeType::Model),
        "source" => node_colors(NodeType::Source),
        "seed" => node_colors(NodeType::Seed),
        "snapshot" => node_colors(NodeType::Snapshot),
        "test" => node_colors(NodeType::Test),
        "exposure" => node_colors(NodeType::Exposure),
        "phantom" => node_colors(NodeType::Phantom),
        _ => ("#999999", "black"),
    }
}

/// Capitalize the first letter of a string
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_helpers::make_node;

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_dot_to_writer(graph, &mut buf, None).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn render_to_string_grouped(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_dot_to_writer(graph, &mut buf, Some(GroupBy::NodeType)).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_empty_graph() {
        let graph = LineageGraph::new();
        let output = render_to_string(&graph);
        assert!(output.contains("digraph dbt_lineage {"));
        assert!(output.contains("}"));
    }

    #[test]
    fn test_single_node() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let output = render_to_string(&graph);
        assert!(output.contains("\"model.orders\""));
        assert!(output.contains("label=\"orders\""));
        assert!(output.contains("fillcolor=\"#4A90D9\""));
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
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Source));

        let output = render_to_string(&graph);
        assert!(output.contains("style=dashed"));
        assert!(output.contains("label=\"source\""));
    }

    #[test]
    fn test_all_edge_type_labels() {
        let types = [
            (EdgeType::Ref, "ref"),
            (EdgeType::Source, "source"),
            (EdgeType::Test, "test"),
            (EdgeType::Exposure, "exposure"),
        ];
        for (et, expected) in types {
            let ed = EdgeData::direct(et);
            assert_eq!(ed.edge_type.label(), expected);
        }
    }

    #[test]
    fn test_node_colors_all_types() {
        let types = [
            NodeType::Model,
            NodeType::Source,
            NodeType::Seed,
            NodeType::Snapshot,
            NodeType::Test,
            NodeType::Exposure,
            NodeType::Phantom,
        ];
        for nt in types {
            let (color, fontcolor) = node_colors(nt);
            assert!(
                color.starts_with('#'),
                "Color for {:?} should start with #",
                nt
            );
            assert!(!fontcolor.is_empty());
        }
    }

    #[test]
    fn test_multiple_edges_different_styles() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        let c = graph.add_node(make_node("test.t", "t", NodeType::Test));
        let d = graph.add_node(make_node("exposure.e", "e", NodeType::Exposure));

        graph.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(b, c, EdgeData::direct(EdgeType::Test));
        graph.add_edge(b, d, EdgeData::direct(EdgeType::Exposure));

        let output = render_to_string(&graph);
        // Ref edges have no extra style
        assert!(output.contains("label=\"ref\""));
        assert!(output.contains("style=dotted"));
        assert!(output.contains("style=bold"));
    }

    #[test]
    fn test_all_node_types_render() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.m", "m", NodeType::Model));
        graph.add_node(make_node("source.s", "s", NodeType::Source));
        graph.add_node(make_node("seed.sd", "sd", NodeType::Seed));
        graph.add_node(make_node("snapshot.sn", "sn", NodeType::Snapshot));
        graph.add_node(make_node("test.t", "t", NodeType::Test));
        graph.add_node(make_node("exposure.e", "e", NodeType::Exposure));
        graph.add_node(make_node("phantom.p", "p", NodeType::Phantom));

        let output = render_to_string(&graph);
        // Verify all node colors appear
        assert!(output.contains("#4A90D9")); // Model
        assert!(output.contains("#27AE60")); // Source
        assert!(output.contains("#F39C12")); // Seed
        assert!(output.contains("#8E44AD")); // Snapshot
        assert!(output.contains("#1ABC9C")); // Test
        assert!(output.contains("#E74C3C")); // Exposure
        assert!(output.contains("#BDC3C7")); // Phantom
        assert!(output.contains("fontcolor=\"black\"")); // Phantom font
    }

    #[test]
    fn test_all_four_edge_styles_in_render() {
        let mut graph = LineageGraph::new();
        let s = graph.add_node(make_node("source.raw.o", "raw.o", NodeType::Source));
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        let t = graph.add_node(make_node("test.t", "t", NodeType::Test));
        let e = graph.add_node(make_node("exposure.e", "e", NodeType::Exposure));

        graph.add_edge(s, a, EdgeData::direct(EdgeType::Source));
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(b, t, EdgeData::direct(EdgeType::Test));
        graph.add_edge(b, e, EdgeData::direct(EdgeType::Exposure));

        let output = render_to_string(&graph);
        assert!(output.contains("label=\"source\""));
        assert!(output.contains("label=\"ref\""));
        assert!(output.contains("label=\"test\""));
        assert!(output.contains("label=\"exposure\""));
        assert!(output.contains("style=dashed"));
        assert!(output.contains("style=dotted"));
        assert!(output.contains("style=bold"));
    }

    #[test]
    fn test_transitive_ref_edge_style() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_edge(a, b, EdgeData::transitive(EdgeType::Ref, 2));

        let output = render_to_string(&graph);
        assert!(output.contains(r#"label="ref (via 2)""#));
        assert!(output.contains("style=dashed"));
    }

    #[test]
    fn test_transitive_source_edge_preserves_dashed() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("source.raw.a", "a", NodeType::Source));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_edge(a, b, EdgeData::transitive(EdgeType::Source, 3));

        let output = render_to_string(&graph);
        assert!(output.contains(r#"label="source (via 3)""#));
        assert!(output.contains(r#"style="dashed,bold""#));
    }

    #[test]
    fn test_transitive_exposure_edge_preserves_bold() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("exposure.e", "e", NodeType::Exposure));
        graph.add_edge(a, b, EdgeData::transitive(EdgeType::Exposure, 1));

        let output = render_to_string(&graph);
        assert!(output.contains(r#"label="exposure (via 1)""#));
        assert!(output.contains(r#"style="bold,dashed""#));
    }

    #[test]
    fn test_snapshot_lineage() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_group_by_node_type() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let output = render_to_string_grouped(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_group_by_node_type_cluster_structure() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        graph.add_node(make_node("source.raw.b", "raw.b", NodeType::Source));

        let output = render_to_string_grouped(&graph);
        assert!(output.contains("subgraph cluster_model {"));
        assert!(output.contains("subgraph cluster_source {"));
        assert!(output.contains("label=\"Model\""));
        assert!(output.contains("label=\"Source\""));
        assert!(output.contains("style=rounded;"));
    }
}
