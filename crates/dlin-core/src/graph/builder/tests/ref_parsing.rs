use super::*;

#[test]
fn test_resolve_ref_model() {
    let mut node_map = HashMap::new();
    let graph = &mut LineageGraph::new();
    let idx = graph.add_node(NodeData {
        unique_id: "model.orders".to_string(),
        label: "orders".to_string(),
        node_type: NodeType::Model,
        file_path: None,
        description: None,
        materialization: None,
        tags: vec![],
        columns: vec![],
        exposure: None,
        aliases: vec![],
    });
    node_map.insert("model.orders".to_string(), idx);

    assert_eq!(resolve_ref("orders", &node_map), "model.orders");
}

#[test]
fn test_resolve_ref_seed() {
    let mut node_map = HashMap::new();
    let graph = &mut LineageGraph::new();
    let idx = graph.add_node(NodeData {
        unique_id: "seed.countries".to_string(),
        label: "countries".to_string(),
        node_type: NodeType::Seed,
        file_path: None,
        description: None,
        materialization: None,
        tags: vec![],
        columns: vec![],
        exposure: None,
        aliases: vec![],
    });
    node_map.insert("seed.countries".to_string(), idx);

    assert_eq!(resolve_ref("countries", &node_map), "seed.countries");
}

#[test]
fn test_resolve_ref_snapshot() {
    let mut node_map = HashMap::new();
    let graph = &mut LineageGraph::new();
    let idx = graph.add_node(NodeData {
        unique_id: "snapshot.snap_orders".to_string(),
        label: "snap_orders".to_string(),
        node_type: NodeType::Snapshot,
        file_path: None,
        description: None,
        materialization: None,
        tags: vec![],
        columns: vec![],
        exposure: None,
        aliases: vec![],
    });
    node_map.insert("snapshot.snap_orders".to_string(), idx);

    assert_eq!(
        resolve_ref("snap_orders", &node_map),
        "snapshot.snap_orders"
    );
}

#[test]
fn test_resolve_ref_unknown_defaults_to_model() {
    let node_map = HashMap::new();
    assert_eq!(resolve_ref("unknown_ref", &node_map), "model.unknown_ref");
}

#[test]
fn test_parse_exposure_ref() {
    assert_eq!(
        parse_exposure_ref("ref('orders')"),
        Some(("orders".to_string(), None))
    );
    assert_eq!(
        parse_exposure_ref("ref(\"orders\")"),
        Some(("orders".to_string(), None))
    );
    assert_eq!(
        parse_exposure_ref("ref('my_model', version=2)"),
        Some(("my_model".to_string(), Some("2".to_string())))
    );
    assert_eq!(
        parse_exposure_ref("ref('my_model', v=3)"),
        Some(("my_model".to_string(), Some("3".to_string())))
    );
    // Package-qualified refs are not supported and return None
    assert_eq!(parse_exposure_ref("ref('other_pkg', 'orders')"), None);
    assert_eq!(parse_exposure_ref("source('raw', 'orders')"), None);
    assert_eq!(parse_exposure_ref("something_else"), None);
}
