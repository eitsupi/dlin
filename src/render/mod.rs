use std::io;

pub mod ascii;
pub mod dot;
pub mod html;
pub mod impact;
pub mod json;
pub mod layout;
pub mod list;
pub mod mermaid;
pub mod plain;
pub mod summary;
pub mod svg;

/// Handle an I/O result from writing to stdout.
/// Silently ignores `BrokenPipe` errors (e.g. `cmd | head`).
pub(crate) fn handle_stdout_result(result: io::Result<()>) {
    if let Err(e) = result
        && e.kind() != io::ErrorKind::BrokenPipe
    {
        eprintln!("error writing output: {}", e);
    }
}

/// Convert a `serde_json::Error` into an `io::Error`, preserving the
/// underlying I/O error kind (e.g. `BrokenPipe`) when present.
pub(crate) fn serde_io_error(e: serde_json::Error) -> io::Error {
    match e.io_error_kind() {
        Some(kind) => io::Error::new(kind, e),
        None => io::Error::other(e),
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use crate::graph::types::*;

    pub fn make_node(unique_id: &str, label: &str, node_type: NodeType) -> NodeData {
        NodeData {
            unique_id: unique_id.into(),
            label: label.into(),
            node_type,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        }
    }

    /// Build a representative lineage graph for snapshot tests:
    /// source -> staging -> mart -> test, mart -> exposure
    pub fn make_sample_lineage_graph() -> LineageGraph {
        let mut graph = LineageGraph::new();
        let src = graph.add_node(make_node(
            "source.raw.orders",
            "raw.orders",
            NodeType::Source,
        ));
        let stg = graph.add_node(make_node("model.stg_orders", "stg_orders", NodeType::Model));
        let mart = graph.add_node(make_node("model.orders", "orders", NodeType::Model));
        let t = graph.add_node(make_node(
            "test.orders_positive",
            "orders_positive",
            NodeType::Test,
        ));
        let exp = graph.add_node(make_node(
            "exposure.dashboard",
            "dashboard",
            NodeType::Exposure,
        ));

        graph.add_edge(
            src,
            stg,
            EdgeData {
                edge_type: EdgeType::Source,
            },
        );
        graph.add_edge(
            stg,
            mart,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        graph.add_edge(
            mart,
            t,
            EdgeData {
                edge_type: EdgeType::Test,
            },
        );
        graph.add_edge(
            mart,
            exp,
            EdgeData {
                edge_type: EdgeType::Exposure,
            },
        );

        graph
    }
}
