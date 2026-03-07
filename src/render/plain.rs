use std::io::Write;

use crate::graph::types::*;

/// Render the lineage graph as plain text (one label per line) to stdout.
/// Output order follows graph insertion order (not alphabetical or topological).
pub fn render_plain(graph: &LineageGraph) {
    render_plain_to_writer(graph, &mut std::io::stdout().lock());
}

pub fn render_plain_to_writer<W: Write>(graph: &LineageGraph, w: &mut W) {
    for idx in graph.node_indices() {
        writeln!(w, "{}", graph[idx].label).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_helpers::make_node;

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_plain_to_writer(graph, &mut buf);
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_empty_graph() {
        let graph = LineageGraph::new();
        let output = render_to_string(&graph);
        assert!(output.is_empty());
    }

    #[test]
    fn test_single_node() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let output = render_to_string(&graph);
        assert_eq!(output, "orders\n");
    }

    #[test]
    fn test_multiple_nodes() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_node(make_node("source.raw.c", "raw.c", NodeType::Source));
        let output = render_to_string(&graph);
        assert_eq!(output, "a\nb\nraw.c\n");
    }

    #[test]
    fn test_snapshot_plain() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }
}
