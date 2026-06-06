use super::*;
use petgraph::visit::NodeIndexable;
use std::path::PathBuf;

fn make_node(
    unique_id: &str,
    label: &str,
    node_type: NodeType,
    file_path: Option<PathBuf>,
    tags: Vec<String>,
) -> NodeData {
    NodeData {
        unique_id: unique_id.into(),
        label: label.into(),
        node_type,
        file_path,
        description: None,
        materialization: None,
        tags,
        columns: vec![],
        exposure: None,
        aliases: vec![],
    }
}

fn make_test_graph() -> LineageGraph {
    let mut g = LineageGraph::new();
    // A -> B -> C -> D
    let a = g.add_node(make_node(
        "source.raw.orders",
        "raw.orders",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node(
        "model.stg_orders",
        "stg_orders",
        NodeType::Model,
        None,
        vec![],
    ));
    let c = g.add_node(make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        None,
        vec![],
    ));
    let d = g.add_node(make_node(
        "exposure.dashboard",
        "dashboard",
        NodeType::Exposure,
        None,
        vec![],
    ));

    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Exposure));
    g
}

fn make_tagged_graph() -> LineageGraph {
    let mut g = LineageGraph::new();
    // A: source, no tags, path schema.yml
    let a = g.add_node(make_node(
        "source.raw.orders",
        "raw.orders",
        NodeType::Source,
        Some(PathBuf::from("models/staging/schema.yml")),
        vec![],
    ));
    // B: model, tag:nightly, path models/staging/stg_orders.sql
    let b = g.add_node(make_node(
        "model.stg_orders",
        "stg_orders",
        NodeType::Model,
        Some(PathBuf::from("models/staging/stg_orders.sql")),
        vec!["nightly".into()],
    ));
    // C: model, tag:daily, path models/marts/orders.sql
    let c = g.add_node(make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        Some(PathBuf::from("models/marts/orders.sql")),
        vec!["daily".into()],
    ));
    // D: exposure, no tags, no path
    let d = g.add_node(make_node(
        "exposure.dashboard",
        "dashboard",
        NodeType::Exposure,
        None,
        vec![],
    ));

    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Exposure));
    g
}

fn render_mermaid(graph: &LineageGraph) -> String {
    let mut buf = Vec::new();
    crate::render::mermaid::render_mermaid_to_writer(
        graph,
        &mut buf,
        None,
        crate::Direction::LR,
        false,
    )
    .unwrap();
    String::from_utf8(buf).unwrap()
}

fn make_node_with_desc(unique_id: &str, label: &str, description: Option<&str>) -> NodeData {
    NodeData {
        unique_id: unique_id.into(),
        label: label.into(),
        node_type: NodeType::Model,
        file_path: None,
        description: description.map(|s| s.to_string()),
        materialization: None,
        tags: vec![],
        columns: vec![],
        exposure: None,
        aliases: vec![],
    }
}

fn make_search_graph() -> LineageGraph {
    let mut g = LineageGraph::new();
    g.add_node(make_node_with_desc(
        "model.stg_orders",
        "stg_orders",
        Some("Staging model for order data"),
    ));
    g.add_node(make_node_with_desc(
        "model.stg_customers",
        "stg_customers",
        Some("Staging model for customer data"),
    ));
    g.add_node(make_node_with_desc(
        "model.order_summary",
        "order_summary",
        None,
    ));
    g.add_node(make_node_with_desc("model.payments", "payments", None));
    g
}

fn re(pattern: &str) -> regex::Regex {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(true)
        .build()
        .unwrap()
}

fn make_versioned_graph() -> LineageGraph {
    let mut g = LineageGraph::new();
    g.add_node(make_node(
        "model.my_model.v1",
        "my_model.v1",
        NodeType::Model,
        None,
        vec![],
    ));
    let v2 = g.add_node(make_node(
        "model.my_model.v2",
        "my_model.v2",
        NodeType::Model,
        None,
        vec![],
    ));
    // Simulate the latest-version alias registered by build_graph
    g[v2].aliases.push("model.my_model".to_string());
    g
}

mod collapse;
mod filter_graph;
mod output;
mod search;
mod selector;
mod transitive;
mod versioned;
