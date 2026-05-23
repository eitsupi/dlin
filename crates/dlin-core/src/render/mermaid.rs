use std::collections::BTreeMap;
use std::io::{self, Write};

use petgraph::visit::{EdgeRef, IntoEdgeReferences};

use crate::graph::types::*;
use crate::{Direction, GroupBy};

/// Render the lineage graph as a Mermaid flowchart to stdout
pub fn render_mermaid(
    graph: &LineageGraph,
    group_by: Option<GroupBy>,
    direction: Direction,
    show_columns: bool,
) {
    super::handle_stdout_result(render_mermaid_to_writer(
        graph,
        &mut std::io::stdout().lock(),
        group_by,
        direction,
        show_columns,
    ));
}

pub(crate) fn render_mermaid_to_writer<W: Write>(
    graph: &LineageGraph,
    w: &mut W,
    group_by: Option<GroupBy>,
    direction: Direction,
    show_columns: bool,
) -> io::Result<()> {
    writeln!(w, "flowchart {direction}")?;

    if graph.node_count() == 0 {
        return Ok(());
    }

    // Collect and sort nodes by unique_id
    let mut nodes: Vec<_> = graph.node_indices().map(|idx| &graph[idx]).collect();
    nodes.sort_by_key(|n| &n.unique_id);

    match group_by {
        Some(GroupBy::NodeType) => write_nodes_grouped_by_type(w, &nodes, show_columns)?,
        Some(GroupBy::Directory) => write_nodes_grouped_by_directory(w, &nodes, show_columns)?,
        None => write_nodes_flat(w, &nodes, show_columns)?,
    }

    writeln!(w)?;

    // Collect and sort edges by (source_id, target_id, edge_type)
    let mut edges: Vec<_> = graph
        .edge_references()
        .map(|edge| {
            let source = &graph[edge.source()];
            let target = &graph[edge.target()];
            let w = edge.weight();
            (
                mermaid_id(&source.unique_id),
                mermaid_id(&target.unique_id),
                w.edge_type,
                w.collapsed_through,
            )
        })
        .collect();
    edges.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
            .then(a.3.cmp(&b.3))
    });

    // Render edges
    for (src_id, tgt_id, edge_type, collapsed) in &edges {
        // Quote labels containing special characters (parentheses etc.)
        // to prevent Mermaid from misparsing the edge label text.
        let label = match collapsed {
            Some(n) => format!(r#""{} (via {})""#, edge_type.label(), n),
            None => edge_type.label().to_string(),
        };
        let arrow = match edge_type {
            EdgeType::Ref => format!("    {} -->|{}| {}", src_id, label, tgt_id),
            EdgeType::Source => format!("    {} -.->|{}| {}", src_id, label, tgt_id),
            EdgeType::Test => format!("    {} -.->|{}| {}", src_id, label, tgt_id),
            EdgeType::Exposure => format!("    {} ==>|{}| {}", src_id, label, tgt_id),
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

/// Write nodes without grouping (flat list)
fn write_nodes_flat<W: Write>(
    w: &mut W,
    nodes: &[&NodeData],
    show_columns: bool,
) -> io::Result<()> {
    for node in nodes {
        writeln!(w, "    {}", node_shape(node, show_columns))?;
    }
    Ok(())
}

/// Write nodes grouped by node type using Mermaid subgraph blocks
fn write_nodes_grouped_by_type<W: Write>(
    w: &mut W,
    nodes: &[&NodeData],
    show_columns: bool,
) -> io::Result<()> {
    // Group nodes by NodeType, preserving sorted order within each group
    let mut groups: BTreeMap<NodeType, Vec<&NodeData>> = BTreeMap::new();
    for node in nodes {
        groups.entry(node.node_type).or_default().push(node);
    }

    // Render each group as a subgraph with a capitalized title
    for (node_type, group_nodes) in &groups {
        let type_label = node_type.label();
        let title = super::capitalize(type_label);
        writeln!(w, r#"    subgraph {type_label}["{title}"]"#)?;
        for node in group_nodes {
            writeln!(w, "        {}", node_shape(node, show_columns))?;
        }
        writeln!(w, "    end")?;
    }
    Ok(())
}

/// Write nodes grouped by file directory using Mermaid subgraph blocks
fn write_nodes_grouped_by_directory<W: Write>(
    w: &mut W,
    nodes: &[&NodeData],
    show_columns: bool,
) -> io::Result<()> {
    let mut groups: BTreeMap<String, Vec<&NodeData>> = BTreeMap::new();
    for node in nodes {
        let dir = super::directory_label(node);
        groups.entry(dir).or_default().push(node);
    }

    for (dir, group_nodes) in &groups {
        let subgraph_id = super::sanitize_id(dir);
        writeln!(w, r#"    subgraph {subgraph_id}["{dir}"]"#)?;
        for node in group_nodes {
            writeln!(w, "        {}", node_shape(node, show_columns))?;
        }
        writeln!(w, "    end")?;
    }
    Ok(())
}

/// Build the label string for a node, optionally including column names.
///
/// When `show_columns` is true and the node has columns, the label includes
/// a `---` text separator followed by the comma-separated column list.
fn node_label(node: &NodeData, show_columns: bool) -> String {
    if show_columns && !node.columns.is_empty() {
        // `---` is rendered as literal text in flowchart labels (not a graphical
        // rule), but serves as a clear visual separator between the node name
        // and its columns.
        let cols = node.columns.join(", ");
        format!(
            "{}<br/>---<br/>{}",
            super::mermaid_escape(&node.label),
            super::mermaid_escape(&cols)
        )
    } else {
        super::mermaid_escape(&node.label)
    }
}

/// Generate the Mermaid shape string for a node
fn node_shape(node: &NodeData, show_columns: bool) -> String {
    let id = mermaid_id(&node.unique_id);
    let label = node_label(node, show_columns);
    match node.node_type {
        NodeType::Model => format!(r#"{id}["{label}"]"#),
        NodeType::Source => format!(r#"{id}(["{label}"])"#),
        NodeType::Seed => format!(r#"{id}[/"{label}"\]"#),
        NodeType::Snapshot => format!(r#"{id}{{{{"{label}"}}}}"#),
        NodeType::Test => format!(r#"{id}{{"{label}"}}"#),
        NodeType::Exposure => format!(r#"{id}>"{label}"]"#),
        NodeType::Phantom => format!(r#"{id}("{label}")"#),
    }
}

/// Convert a unique_id to a valid Mermaid node ID (replace dots with underscores)
fn mermaid_id(unique_id: &str) -> String {
    unique_id.replace('.', "_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_helpers::{make_node, make_node_with_columns, make_node_with_path};

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_mermaid_to_writer(graph, &mut buf, None, Direction::LR, false).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn render_to_string_grouped(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_mermaid_to_writer(
            graph,
            &mut buf,
            Some(GroupBy::NodeType),
            Direction::LR,
            false,
        )
        .unwrap();
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
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Source));

        let output = render_to_string(&graph);
        assert!(output.contains("-.->|source|"));
    }

    #[test]
    fn test_ref_edge() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Ref));

        let output = render_to_string(&graph);
        assert!(output.contains("-->|ref|"));
    }

    #[test]
    fn test_exposure_edge() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("model.a", "a", NodeType::Model));
        let b = graph.add_node(make_node("exposure.dash", "dash", NodeType::Exposure));
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Exposure));

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
        graph.add_edge(a, t, EdgeData::direct(EdgeType::Test));

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

    #[test]
    fn test_transitive_edge_rendering() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("source.raw.a", "a", NodeType::Source));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        graph.add_edge(a, b, EdgeData::transitive(EdgeType::Source, 2));

        let output = render_to_string(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_mixed_direct_and_transitive_edges() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node("source.raw.a", "a", NodeType::Source));
        let b = graph.add_node(make_node("model.b", "b", NodeType::Model));
        let c = graph.add_node(make_node("model.c", "c", NodeType::Model));
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Source));
        graph.add_edge(a, c, EdgeData::transitive(EdgeType::Source, 3));
        graph.add_edge(b, c, EdgeData::direct(EdgeType::Ref));

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
    fn test_group_by_node_type_subgraph_structure() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        graph.add_node(make_node("source.raw.b", "raw.b", NodeType::Source));

        let output = render_to_string_grouped(&graph);
        assert!(output.contains("subgraph model[\"Model\"]"));
        assert!(output.contains("subgraph source[\"Source\"]"));
        assert!(output.contains("end"));
        // Nodes should be indented inside subgraph
        assert!(output.contains("        model_a[\"a\"]"));
        assert!(output.contains("        source_raw_b([\"raw.b\"])"));
    }

    fn render_to_string_directory(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_mermaid_to_writer(
            graph,
            &mut buf,
            Some(GroupBy::Directory),
            Direction::LR,
            false,
        )
        .unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_group_by_directory_subgraph_structure() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node_with_path(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
            "models/staging/stg_orders.sql",
        ));
        graph.add_node(make_node_with_path(
            "model.orders",
            "orders",
            NodeType::Model,
            "models/marts/orders.sql",
        ));
        graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        let output = render_to_string_directory(&graph);
        assert!(output.contains(r#"subgraph models_marts["models/marts"]"#));
        assert!(output.contains(r#"subgraph models_staging["models/staging"]"#));
        assert!(output.contains(r#"subgraph _other_["(other)"]"#) || output.contains("(other)"));
        assert!(output.contains("end"));
    }

    #[test]
    fn test_snapshot_group_by_directory() {
        let mut graph = LineageGraph::new();
        let src = graph.add_node(make_node_with_path(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
            "models/staging/schema.yml",
        ));
        let stg = graph.add_node(make_node_with_path(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
            "models/staging/stg_orders.sql",
        ));
        let mart = graph.add_node(make_node_with_path(
            "model.orders",
            "orders",
            NodeType::Model,
            "models/marts/orders.sql",
        ));
        let exp = graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        graph.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
        graph.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(mart, exp, EdgeData::direct(EdgeType::Exposure));

        let output = render_to_string_directory(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_direction_tb() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let mut buf = Vec::new();
        render_mermaid_to_writer(&graph, &mut buf, None, Direction::TB, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_direction_tb_grouped() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let mut buf = Vec::new();
        render_mermaid_to_writer(
            &graph,
            &mut buf,
            Some(GroupBy::NodeType),
            Direction::TB,
            false,
        )
        .unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    fn render_to_string_with_columns(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_mermaid_to_writer(graph, &mut buf, None, Direction::LR, true).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_show_columns_single_model() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node_with_columns(
            "model.orders",
            "orders",
            NodeType::Model,
            &["order_id", "customer_id", "status"],
        ));
        let output = render_to_string_with_columns(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_show_columns_lineage() {
        let mut graph = LineageGraph::new();
        let src = graph.add_node(make_node_with_columns(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
            &["id", "customer_id", "amount", "created_at"],
        ));
        let stg = graph.add_node(make_node_with_columns(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
            &["order_id", "customer_id", "amount"],
        ));
        let mart = graph.add_node(make_node_with_columns(
            "model.orders",
            "orders",
            NodeType::Model,
            &["order_id", "customer_id", "status", "total_amount"],
        ));
        let exp = graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        graph.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
        graph.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(mart, exp, EdgeData::direct(EdgeType::Exposure));

        let output = render_to_string_with_columns(&graph);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_show_columns_empty_columns_unchanged() {
        // Nodes without columns should render identically with show_columns=true
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));

        let without = render_to_string(&graph);
        let with = render_to_string_with_columns(&graph);
        assert_eq!(without, with);
    }

    #[test]
    fn test_show_columns_with_grouping() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node_with_columns(
            "model.orders",
            "orders",
            NodeType::Model,
            &["order_id", "status"],
        ));
        graph.add_node(make_node_with_columns(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
            &["id", "amount"],
        ));

        let mut buf = Vec::new();
        render_mermaid_to_writer(
            &graph,
            &mut buf,
            Some(GroupBy::NodeType),
            Direction::LR,
            true,
        )
        .unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_show_columns_with_collapse() {
        // Build a chain: source -> stg -> mart -> exposure
        // After collapse, only source and exposure remain (endpoints)
        let mut graph = LineageGraph::new();
        let src = graph.add_node(make_node_with_columns(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
            &["id", "customer_id", "amount"],
        ));
        let stg = graph.add_node(make_node_with_columns(
            "model.stg_orders",
            "stg_orders",
            NodeType::Model,
            &["order_id", "customer_id", "amount"],
        ));
        let mart = graph.add_node(make_node_with_columns(
            "model.orders",
            "orders",
            NodeType::Model,
            &["order_id", "customer_id", "status", "total_amount"],
        ));
        let exp = graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        graph.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
        graph.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(mart, exp, EdgeData::direct(EdgeType::Exposure));

        let collapsed = crate::graph::filter::collapse_intermediate(
            &graph,
            crate::CollapseMode::Endpoints,
            &std::collections::HashSet::new(),
        );
        let output = render_to_string_with_columns(&collapsed);
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_show_columns_disabled_ignores_columns() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node_with_columns(
            "model.orders",
            "orders",
            NodeType::Model,
            &["order_id", "status"],
        ));

        // show_columns=false should NOT include columns even if present
        let output = render_to_string(&graph);
        assert!(!output.contains("order_id"));
        assert!(!output.contains("status"));
        assert!(output.contains("model_orders[\"orders\"]"));
    }

    #[test]
    fn test_show_columns_escapes_quotes() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node_with_columns(
            "model.orders",
            "orders",
            NodeType::Model,
            &["Total Amount", r#"col "quoted""#],
        ));
        let output = render_to_string_with_columns(&graph);
        // Double quotes in column names must be escaped to #quot;
        assert!(output.contains("#quot;"));
        assert!(!output.contains(r#"col "quoted""#));
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_mermaid_escape() {
        assert_eq!(super::super::mermaid_escape("hello"), "hello");
        assert_eq!(
            super::super::mermaid_escape(r#"a "b" c"#),
            "a #quot;b#quot; c"
        );
        assert_eq!(super::super::mermaid_escape("a<b>c"), "a#lt;b#gt;c");
        assert_eq!(super::super::mermaid_escape("col#1"), "col#num;1");
    }
}
