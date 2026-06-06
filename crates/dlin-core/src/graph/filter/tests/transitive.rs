use super::*;

// -- Transitive edge tests -------------------------------------------------

#[test]
fn test_transitive_basic() {
    // A(source) -> B(seed) -> C(model): filter to source,model
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("seed.b", "b", NodeType::Seed, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    assert_eq!(filtered.node_count(), 2);
    assert_eq!(filtered.edge_count(), 1);

    let edge = filtered.edge_references().next().unwrap();
    assert_eq!(edge.weight().collapsed_through, Some(1));
    // max(Source, Ref) = Source
    assert_eq!(edge.weight().edge_type, EdgeType::Source);
}

#[test]
fn test_transitive_chain() {
    // A(source) -> B(seed) -> C(seed) -> D(model)
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("seed.b", "b", NodeType::Seed, None, vec![]));
    let c = g.add_node(make_node("seed.c", "c", NodeType::Seed, None, vec![]));
    let d = g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    assert_eq!(filtered.node_count(), 2);
    assert_eq!(filtered.edge_count(), 1);

    let edge = filtered.edge_references().next().unwrap();
    assert_eq!(edge.weight().collapsed_through, Some(2));
}

#[test]
fn test_transitive_diamond_dedup() {
    // A -> B -> D, A -> C -> D (B,C removed) => single A->D edge
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("seed.b", "b", NodeType::Seed, None, vec![]));
    let c = g.add_node(make_node("seed.c", "c", NodeType::Seed, None, vec![]));
    let d = g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(a, c, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, d, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    assert_eq!(filtered.node_count(), 2);
    assert_eq!(filtered.edge_count(), 1); // deduplicated
}

#[test]
fn test_transitive_skips_when_direct_edge_exists() {
    // A -> C (direct) and A -> B -> C (B removed)
    // Should only have the direct edge, no redundant transitive edge
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("seed.b", "b", NodeType::Seed, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    g.add_edge(a, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["model".into()], true);
    assert_eq!(filtered.node_count(), 2);
    assert_eq!(filtered.edge_count(), 1); // only the direct edge, no transitive duplicate
    // Verify it's the direct edge (no collapsed_through)
    let edge = filtered.edge_references().next().unwrap();
    assert!(edge.weight().collapsed_through.is_none());
}

#[test]
fn test_transitive_edge_type_max() {
    // A(source) -> B(test) -> C(model): Source->Test path, max=Test
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("test.b", "b", NodeType::Test, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Test));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    let edge = filtered.edge_references().next().unwrap();
    // max(Source, Test) = Test
    assert_eq!(edge.weight().edge_type, EdgeType::Test);
}

#[test]
fn test_transitive_disabled() {
    // Same graph as test_transitive_basic, but transitive=false
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("seed.b", "b", NodeType::Seed, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], false);
    assert_eq!(filtered.node_count(), 2);
    assert_eq!(filtered.edge_count(), 0); // no transitive edges
}

#[test]
fn test_transitive_preserves_direct_edges() {
    // A(source) -> B(model) -> C(seed) -> D(model)
    // Direct edge A->B should be preserved, transitive B->D added
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let c = g.add_node(make_node("seed.c", "c", NodeType::Seed, None, vec![]));
    let d = g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    assert_eq!(filtered.node_count(), 3); // source + 2 models
    assert_eq!(filtered.edge_count(), 2); // direct A->B + transitive B->D

    let mut has_direct = false;
    let mut has_transitive = false;
    for edge in filtered.edge_references() {
        match edge.weight().collapsed_through {
            None => has_direct = true,
            Some(_) => has_transitive = true,
        }
    }
    assert!(has_direct);
    assert!(has_transitive);
}
