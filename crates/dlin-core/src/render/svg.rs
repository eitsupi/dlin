use std::io::Write;

use petgraph::visit::{EdgeRef, IntoEdgeReferences};

use crate::graph::types::*;
use crate::render::layout::{LayoutResult, sugiyama_layout};

const NODE_WIDTH: f64 = 160.0;
const NODE_HEIGHT: f64 = 40.0;
const LAYER_SPACING: f64 = 220.0;
const NODE_SPACING: f64 = 60.0;
const PADDING: f64 = 40.0;
const LEGEND_ENTRY_SPACING: f64 = 80.0;

const LEGEND_TYPES: &[(&str, &str)] = &[
    ("model", "#4A90D9"),
    ("source", "#27AE60"),
    ("seed", "#F39C12"),
    ("snapshot", "#8E44AD"),
    ("test", "#1ABC9C"),
    ("exposure", "#E74C3C"),
    ("semantic_model", "#16A085"),
    ("metric", "#D35400"),
    ("saved_query", "#2980B9"),
    ("phantom", "#BDC3C7"),
];

fn legend_min_width() -> f64 {
    PADDING + LEGEND_TYPES.len() as f64 * LEGEND_ENTRY_SPACING
}

fn node_fill(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::Model => "#4A90D9",
        NodeType::Source => "#27AE60",
        NodeType::Seed => "#F39C12",
        NodeType::Snapshot => "#8E44AD",
        NodeType::Test => "#1ABC9C",
        NodeType::Exposure => "#E74C3C",
        NodeType::SemanticModel => "#16A085",
        NodeType::Metric => "#D35400",
        NodeType::SavedQuery => "#2980B9",
        NodeType::Phantom => "#BDC3C7",
    }
}

fn node_font_color(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::Phantom => "#000000",
        _ => "#ffffff",
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn edge_style(edge_type: EdgeType) -> &'static str {
    match edge_type {
        EdgeType::Ref => "stroke:#555;stroke-width:1.5",
        EdgeType::Source => "stroke:#555;stroke-width:1.5;stroke-dasharray:5,3",
        EdgeType::Test => "stroke:#555;stroke-width:1;stroke-dasharray:2,2",
        EdgeType::Exposure => "stroke:#555;stroke-width:2.5",
    }
}

fn node_center(layer: usize, pos: usize) -> (f64, f64) {
    let x = PADDING + layer as f64 * LAYER_SPACING + NODE_WIDTH / 2.0;
    let y = PADDING + pos as f64 * (NODE_HEIGHT + NODE_SPACING) + NODE_HEIGHT / 2.0;
    (x, y)
}

/// Render SVG to stdout
pub fn render_svg(graph: &LineageGraph) {
    super::handle_stdout_result(render_svg_to_writer(graph, &mut std::io::stdout().lock()));
}

/// Render SVG to a string (used by HTML renderer)
pub fn render_svg_to_string(graph: &LineageGraph) -> String {
    let mut buf = Vec::new();
    render_svg_to_writer(graph, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

pub fn render_svg_to_writer<W: Write>(graph: &LineageGraph, w: &mut W) -> std::io::Result<()> {
    let layout = sugiyama_layout(graph);

    let graph_width = if layout.num_layers == 0 {
        200.0
    } else {
        PADDING * 2.0 + layout.num_layers as f64 * LAYER_SPACING
    };
    let total_width = graph_width.max(legend_min_width());
    let total_height = if layout.max_layer_width == 0 {
        100.0
    } else {
        PADDING * 2.0 + layout.max_layer_width as f64 * (NODE_HEIGHT + NODE_SPACING)
    };

    writeln!(
        w,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}" width="{}" height="{}">"#,
        total_width, total_height, total_width, total_height
    )?;

    // Defs for arrowhead marker
    writeln!(w, "  <defs>")?;
    writeln!(
        w,
        r#"    <marker id="arrowhead" markerWidth="10" markerHeight="7" refX="10" refY="3.5" orient="auto">"#
    )?;
    writeln!(
        w,
        r##"      <polygon points="0 0, 10 3.5, 0 7" fill="#555" />"##
    )?;
    writeln!(w, "    </marker>")?;
    writeln!(w, "  </defs>")?;

    // Background
    writeln!(
        w,
        r##"  <rect width="100%" height="100%" fill="#1a1a2e" />"##
    )?;

    // Render edges first (behind nodes)
    render_svg_edges(w, graph, &layout)?;

    // Render nodes
    render_svg_nodes(w, graph, &layout)?;

    // Legend
    render_svg_legend(w, total_height)?;

    writeln!(w, "</svg>")?;
    Ok(())
}

fn render_svg_edges<W: Write>(
    w: &mut W,
    graph: &LineageGraph,
    layout: &LayoutResult,
) -> std::io::Result<()> {
    for edge in graph.edge_references() {
        let source_pos = layout.positions.get(&edge.source());
        let target_pos = layout.positions.get(&edge.target());

        if let (Some(&(sl, sp)), Some(&(tl, tp))) = (source_pos, target_pos) {
            let (sx, sy) = node_center(sl, sp);
            let (tx, ty) = node_center(tl, tp);

            // Start from right edge of source, end at left edge of target
            let x1 = sx + NODE_WIDTH / 2.0;
            let y1 = sy;
            let x2 = tx - NODE_WIDTH / 2.0;
            let y2 = ty;

            let cx1 = x1 + (x2 - x1) * 0.4;
            let cx2 = x1 + (x2 - x1) * 0.6;

            let source_node = &graph[edge.source()];
            let target_node = &graph[edge.target()];
            let style = edge_style(edge.weight().edge_type);

            writeln!(
                w,
                r#"  <path d="M{},{} C{},{} {},{} {},{}" fill="none" style="{}" marker-end="url(#arrowhead)" data-source="{}" data-target="{}" />"#,
                x1,
                y1,
                cx1,
                y1,
                cx2,
                y2,
                x2,
                y2,
                style,
                xml_escape(&source_node.unique_id),
                xml_escape(&target_node.unique_id)
            )?;
        }
    }
    Ok(())
}

fn render_svg_nodes<W: Write>(
    w: &mut W,
    graph: &LineageGraph,
    layout: &LayoutResult,
) -> std::io::Result<()> {
    for idx in graph.node_indices() {
        let Some(&(layer, pos)) = layout.positions.get(&idx) else {
            continue;
        };
        let node = &graph[idx];
        let (cx, cy) = node_center(layer, pos);
        let x = cx - NODE_WIDTH / 2.0;
        let y = cy - NODE_HEIGHT / 2.0;

        let fill = node_fill(node.node_type);
        let font_color = node_font_color(node.node_type);
        let label = xml_escape(&node.display_name());

        writeln!(
            w,
            r#"  <g data-id="{}" class="node">"#,
            xml_escape(&node.unique_id)
        )?;
        writeln!(
            w,
            r#"    <rect x="{}" y="{}" width="{}" height="{}" rx="8" fill="{}" />"#,
            x, y, NODE_WIDTH, NODE_HEIGHT, fill
        )?;
        writeln!(
            w,
            r#"    <text x="{}" y="{}" text-anchor="middle" dominant-baseline="central" fill="{}" font-family="Helvetica,Arial,sans-serif" font-size="12">{}</text>"#,
            cx, cy, font_color, label
        )?;
        writeln!(w, "  </g>")?;
    }
    Ok(())
}

fn render_svg_legend<W: Write>(w: &mut W, total_height: f64) -> std::io::Result<()> {
    let legend_y = total_height - 30.0;
    let mut x = PADDING;
    for (label, color) in LEGEND_TYPES {
        writeln!(
            w,
            r#"  <rect x="{}" y="{}" width="12" height="12" rx="2" fill="{}" />"#,
            x, legend_y, color
        )?;
        writeln!(
            w,
            r##"  <text x="{}" y="{}" fill="#ccc" font-family="Helvetica,Arial,sans-serif" font-size="10">{}</text>"##,
            x + 16.0,
            legend_y + 10.0,
            label
        )?;
        x += LEGEND_ENTRY_SPACING;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::test_helpers::make_node;

    fn render_to_string(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        render_svg_to_writer(graph, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_empty_graph() {
        let graph = LineageGraph::new();
        let output = render_to_string(&graph);
        assert!(output.contains("<svg"));
        assert!(output.contains("</svg>"));
    }

    #[test]
    fn test_single_node() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let output = render_to_string(&graph);
        assert!(output.contains("data-id=\"model.orders\""));
        assert!(output.contains(">orders</text>"));
        assert!(output.contains("#4A90D9"));
    }

    #[test]
    fn test_edge_rendering() {
        let mut graph = LineageGraph::new();
        let a = graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let b = graph.add_node(make_node("model.stg_orders", "stg_orders", NodeType::Model));
        graph.add_edge(a, b, EdgeData::direct(EdgeType::Source));

        let output = render_to_string(&graph);
        assert!(output.contains("<path"));
        assert!(output.contains("marker-end"));
        assert!(output.contains("data-source=\"source.raw.orders\""));
        assert!(output.contains("data-target=\"model.stg_orders\""));
    }

    #[test]
    fn test_all_node_colors() {
        let types = [
            NodeType::Model,
            NodeType::Source,
            NodeType::Seed,
            NodeType::Snapshot,
            NodeType::Test,
            NodeType::Exposure,
            NodeType::SemanticModel,
            NodeType::Metric,
            NodeType::SavedQuery,
            NodeType::Phantom,
        ];
        for nt in types {
            let fill = node_fill(nt);
            assert!(fill.starts_with('#'), "node_fill failed for {:?}", nt);
        }
    }

    #[test]
    fn test_xml_escape() {
        assert_eq!(xml_escape("a<b>c"), "a&lt;b&gt;c");
        assert_eq!(xml_escape("a&b"), "a&amp;b");
        assert_eq!(xml_escape("a\"b"), "a&quot;b");
    }

    #[test]
    fn test_legend_present() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        let output = render_to_string(&graph);
        assert!(output.contains(">model</text>"));
        assert!(output.contains(">source</text>"));
    }

    #[test]
    fn test_legend_includes_semantic_layer_types() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        let output = render_to_string(&graph);
        assert!(
            output.contains(">semantic_model</text>"),
            "legend missing semantic_model"
        );
        assert!(output.contains(">metric</text>"), "legend missing metric");
        assert!(
            output.contains(">saved_query</text>"),
            "legend missing saved_query"
        );
        assert!(
            output.contains("#16A085"),
            "legend missing semantic_model color"
        );
        assert!(output.contains("#D35400"), "legend missing metric color");
        assert!(
            output.contains("#2980B9"),
            "legend missing saved_query color"
        );
    }

    #[test]
    fn test_legend_within_viewbox() {
        // A one-layer graph would normally produce total_width=300, but the legend
        // requires ~840px. The SVG must be at least as wide as the legend.
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        let output = render_to_string(&graph);

        // Extract viewBox width: viewBox="0 0 {width} {height}"
        let marker = "viewBox=\"0 0 ";
        let start = output.find(marker).unwrap() + marker.len();
        let width_str = output[start..].split(' ').next().unwrap();
        let svg_width: f64 = width_str.parse().unwrap();

        let expected_min = legend_min_width();
        assert!(
            svg_width >= expected_min,
            "SVG width {} < legend min width {}",
            svg_width,
            expected_min
        );
    }

    #[test]
    fn test_render_svg_to_string() {
        let mut graph = LineageGraph::new();
        graph.add_node(make_node("model.a", "a", NodeType::Model));
        let s = super::render_svg_to_string(&graph);
        assert!(s.contains("<svg"));
    }

    #[test]
    fn test_multi_node_all_edge_types() {
        let mut graph = LineageGraph::new();
        let src = graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let model = graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let test = graph.add_node(make_node(
            "test.orders_not_null",
            "orders_not_null",
            NodeType::Test,
        ));
        let exp = graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        graph.add_edge(src, model, EdgeData::direct(EdgeType::Source));
        graph.add_edge(src, model, EdgeData::direct(EdgeType::Ref));
        graph.add_edge(model, test, EdgeData::direct(EdgeType::Test));
        graph.add_edge(model, exp, EdgeData::direct(EdgeType::Exposure));

        let output = render_to_string(&graph);
        // Source edge style: dashed
        assert!(output.contains("stroke-dasharray:5,3"));
        // Test edge style: dotted
        assert!(output.contains("stroke-dasharray:2,2"));
        // Exposure edge style: thick
        assert!(output.contains("stroke-width:2.5"));
        // Ref edge style: basic
        assert!(output.contains("stroke-width:1.5"));
        // All nodes present
        assert!(output.contains("data-id=\"source.raw.orders\""));
        assert!(output.contains("data-id=\"model.orders\""));
        assert!(output.contains("data-id=\"test.orders_not_null\""));
        assert!(output.contains("data-id=\"exposure.dashboard\""));
    }

    #[test]
    fn test_node_font_color_all_types() {
        assert_eq!(node_font_color(NodeType::Phantom), "#000000");
        assert_eq!(node_font_color(NodeType::Model), "#ffffff");
        assert_eq!(node_font_color(NodeType::Source), "#ffffff");
        assert_eq!(node_font_color(NodeType::Seed), "#ffffff");
        assert_eq!(node_font_color(NodeType::Snapshot), "#ffffff");
        assert_eq!(node_font_color(NodeType::Test), "#ffffff");
        assert_eq!(node_font_color(NodeType::Exposure), "#ffffff");
    }

    #[test]
    fn test_edge_style_all_types() {
        let ref_style = edge_style(EdgeType::Ref);
        assert!(ref_style.contains("stroke-width:1.5"));
        assert!(!ref_style.contains("dasharray"));

        let source_style = edge_style(EdgeType::Source);
        assert!(source_style.contains("dasharray:5,3"));

        let test_style = edge_style(EdgeType::Test);
        assert!(test_style.contains("dasharray:2,2"));

        let exp_style = edge_style(EdgeType::Exposure);
        assert!(exp_style.contains("stroke-width:2.5"));
    }
}
