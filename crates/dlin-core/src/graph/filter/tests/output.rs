use super::*;

#[test]
fn test_type_filter_excludes_test_seed_snapshot() {
    let mut g = LineageGraph::new();
    let model = g.add_node(make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        None,
        vec![],
    ));
    let test = g.add_node(make_node(
        "test.orders_positive",
        "orders_positive",
        NodeType::Test,
        None,
        vec![],
    ));
    let seed = g.add_node(make_node(
        "seed.countries",
        "countries",
        NodeType::Seed,
        None,
        vec![],
    ));
    let snap = g.add_node(make_node(
        "snapshot.orders_hist",
        "orders_hist",
        NodeType::Snapshot,
        None,
        vec![],
    ));
    g.add_edge(model, test, EdgeData::direct(EdgeType::Test));
    g.add_edge(seed, model, EdgeData::direct(EdgeType::Ref));
    g.add_edge(model, snap, EdgeData::direct(EdgeType::Ref));

    // Explicit filter (model,source only) — excludes test, seed, snapshot
    let filtered = filter_output_node_types(
        &filter_graph(&g, &[], None, None, &[], false).unwrap(),
        &["model".into(), "source".into()],
        false,
    );
    assert_eq!(filtered.node_count(), 1); // Only the model remains
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"orders".to_string()));

    // Include model + test
    let filtered2 = filter_output_node_types(
        &filter_graph(&g, &[], None, None, &[], false).unwrap(),
        &["model".into(), "test".into()],
        false,
    );
    assert_eq!(filtered2.node_count(), 2); // model + test
}

// -- Output node-type filter tests -----------------------------------------

#[test]
fn test_filter_output_node_types_empty_returns_all() {
    let g = make_test_graph();
    let filtered = filter_output_node_types(&g, &[], false);
    assert_eq!(filtered.node_count(), g.node_count());
}

#[test]
fn test_filter_output_node_types_model_only() {
    let g = make_test_graph();
    let filtered = filter_output_node_types(&g, &["model".into()], false);
    assert_eq!(filtered.node_count(), 2);
    for idx in filtered.node_indices() {
        assert_eq!(filtered[idx].node_type, NodeType::Model);
    }
}

#[test]
fn test_filter_output_node_types_multiple() {
    let g = make_test_graph();
    let filtered = filter_output_node_types(&g, &["model".into(), "source".into()], false);
    assert_eq!(filtered.node_count(), 3);
}

#[test]
fn test_filter_output_node_types_no_match() {
    let g = make_test_graph();
    let filtered = filter_output_node_types(&g, &["test".into()], false);
    assert_eq!(filtered.node_count(), 0);
}

#[test]
fn test_known_node_type_labels_matches_node_type_variants() {
    // Ensure KNOWN_NODE_TYPE_LABELS stays in sync with NodeType variants.
    // Phantom is excluded because it's internal, not user-facing.
    let all_types = [
        NodeType::Model,
        NodeType::Source,
        NodeType::Seed,
        NodeType::Snapshot,
        NodeType::Test,
        NodeType::Exposure,
    ];
    for nt in &all_types {
        assert!(
            KNOWN_NODE_TYPE_LABELS.contains(&nt.label()),
            "NodeType::{:?} label '{}' missing from KNOWN_NODE_TYPE_LABELS",
            nt,
            nt.label()
        );
    }
    // Phantom should NOT be in the list
    assert!(!KNOWN_NODE_TYPE_LABELS.contains(&NodeType::Phantom.label()));
    // No extra entries
    assert_eq!(KNOWN_NODE_TYPE_LABELS.len(), all_types.len());
}

#[test]
fn test_validate_node_type_names_valid() {
    let result = validate_node_type_names(&["model".into(), "source".into()]);
    assert!(result.is_empty());
}

#[test]
fn test_validate_node_type_names_invalid() {
    let result = validate_node_type_names(&["model".into(), "modell".into(), "foo".into()]);
    assert_eq!(result, vec!["modell".to_string(), "foo".to_string()]);
}

#[test]
fn test_validate_node_type_names_case_insensitive() {
    let result = validate_node_type_names(&["Model".into(), "SOURCE".into()]);
    assert!(result.is_empty());
}

#[test]
fn test_filter_output_node_types_case_insensitive() {
    let g = make_test_graph();
    let filtered = filter_output_node_types(&g, &["Model".into()], false);
    assert_eq!(filtered.node_count(), 2);
    for idx in filtered.node_indices() {
        assert_eq!(filtered[idx].node_type, NodeType::Model);
    }
}

#[test]
fn test_filter_graph_rejects_cycle() {
    // Covers line 85: CycleDetected error
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, a, EdgeData::direct(EdgeType::Ref));

    let result = filter_graph(&g, &[], None, None, &[], false);
    assert!(result.is_err());
}
