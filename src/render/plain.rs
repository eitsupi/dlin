use std::collections::HashMap;
use std::io::Write;

use crate::graph::types::*;

/// Render the lineage graph as plain text (one label per line) to stdout.
/// Output order follows graph insertion order (not alphabetical or topological).
pub fn render_plain(graph: &LineageGraph, sql_contents: Option<&HashMap<String, String>>) {
    render_plain_to_writer(graph, sql_contents, &mut std::io::stdout().lock());
}

pub fn render_plain_to_writer<W: Write>(
    graph: &LineageGraph,
    sql_contents: Option<&HashMap<String, String>>,
    w: &mut W,
) {
    for idx in graph.node_indices() {
        let node = &graph[idx];
        writeln!(w, "{}", node.label).unwrap();
        if let Some(sql) = sql_contents.and_then(|m| m.get(&node.unique_id)) {
            writeln!(w, "--- SQL ---").unwrap();
            for line in sql.lines() {
                writeln!(w, "{}", line).unwrap();
            }
            writeln!(w, "----------").unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::render::test_helpers::make_node;

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_plain_to_writer(graph, None, &mut buf);
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
    fn test_snapshot_plain_with_sql() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        graph.add_node(make_node("source.raw.orders", "raw.orders", NodeType::Source));
        let sql_contents = HashMap::from([
            ("model.orders".to_string(), "SELECT * FROM {{ ref('stg_orders') }}".to_string()),
        ]);
        let mut buf = Vec::new();
        render_plain_to_writer(&graph, Some(&sql_contents), &mut buf);
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_plain() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }
}
