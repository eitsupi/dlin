use super::*;
use crate::CollapseMode;

#[test]
fn test_collapse_endpoints_basic() {
    // A(source) -> B(model) -> C(model) -> D(exposure)
    // Endpoints: A (in-degree=0), D (out-degree=0)
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    let d = g.add_node(make_node(
        "exposure.dash",
        "dash",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Exposure));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    assert_eq!(collapsed.node_count(), 2);
    assert_eq!(collapsed.edge_count(), 1);

    let labels: HashSet<String> = collapsed
        .node_indices()
        .map(|i| collapsed[i].label.clone())
        .collect();
    assert!(labels.contains("a"));
    assert!(labels.contains("dash"));

    let edge = collapsed.edge_references().next().unwrap();
    assert_eq!(edge.weight().collapsed_through, Some(2));
}

#[test]
fn test_collapse_endpoints_fan_out() {
    // A -> B -> C, A -> B -> D
    // A: in-degree=0, C,D: out-degree=0 → all three kept
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    let d = g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, d, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    assert_eq!(collapsed.node_count(), 3); // a, c, d
    assert_eq!(collapsed.edge_count(), 2);

    let labels: HashSet<String> = collapsed
        .node_indices()
        .map(|i| collapsed[i].label.clone())
        .collect();
    assert!(labels.contains("a"));
    assert!(labels.contains("c"));
    assert!(labels.contains("d"));
    assert!(!labels.contains("b"));
}

#[test]
fn test_collapse_endpoints_all_models() {
    // A -> B (both endpoints: A in-degree=0, B out-degree=0)
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    assert_eq!(collapsed.node_count(), 2);
    assert_eq!(collapsed.edge_count(), 1);
}

#[test]
fn test_collapse_endpoints_preserves_leaf_model() {
    // source -> stg -> mart (out-degree=0)
    // Endpoints mode keeps mart because it's a topological endpoint
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node("model.stg", "stg", NodeType::Model, None, vec![]));
    let mart = g.add_node(make_node(
        "model.mart",
        "mart",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    assert_eq!(collapsed.node_count(), 2); // source + mart
    let labels: HashSet<String> = collapsed
        .node_indices()
        .map(|i| collapsed[i].label.clone())
        .collect();
    assert!(labels.contains("x"));
    assert!(labels.contains("mart"));
}

// -- Collapse intermediate tests (focal mode) -----------------------------

#[test]
fn test_collapse_focal_source_exposure_only() {
    // source -> stg -> exposure: focal keeps source + exposure
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node("model.stg", "stg", NodeType::Model, None, vec![]));
    let ea = g.add_node(make_node(
        "exposure.a",
        "exp_a",
        NodeType::Exposure,
        None,
        vec![],
    ));
    let eb = g.add_node(make_node(
        "exposure.b",
        "exp_b",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, ea, EdgeData::direct(EdgeType::Exposure));
    g.add_edge(stg, eb, EdgeData::direct(EdgeType::Exposure));

    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &HashSet::new());
    assert_eq!(collapsed.node_count(), 3); // source, exp_a, exp_b
    assert_eq!(collapsed.edge_count(), 2);

    let labels: HashSet<String> = collapsed
        .node_indices()
        .map(|i| collapsed[i].label.clone())
        .collect();
    assert!(labels.contains("x"));
    assert!(labels.contains("exp_a"));
    assert!(labels.contains("exp_b"));
    assert!(!labels.contains("stg"));
}

#[test]
fn test_collapse_focal_models_only_empty() {
    // All-model graph: focal mode → empty (no Source/Exposure)
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &HashSet::new());
    assert_eq!(collapsed.node_count(), 0);
}

#[test]
fn test_collapse_focal_with_preserve() {
    // All-model graph with one preserved: only the preserved node remains
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));

    let preserve = HashSet::from([a]);
    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &preserve);
    assert_eq!(collapsed.node_count(), 1);
    assert_eq!(
        collapsed[collapsed.node_indices().next().unwrap()].label,
        "a"
    );
}

#[test]
fn test_collapse_focal_ignores_bfs_pseudoendpoint() {
    // source -> stg -> mart (out-degree=0, but Model)
    // Focal mode collapses mart; Endpoints mode would keep it
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node("model.stg", "stg", NodeType::Model, None, vec![]));
    let mart = g.add_node(make_node(
        "model.mart",
        "mart",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &HashSet::new());
    assert_eq!(collapsed.node_count(), 1);
    assert_eq!(
        collapsed[collapsed.node_indices().next().unwrap()].label,
        "x"
    );
}

#[test]
fn test_collapse_focal_complex_chain() {
    // source -> model_a -> model_b -> model_c -> exposure
    //                  \-> model_d (leaf model)
    // Focal: only source and exposure kept
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "x",
        NodeType::Source,
        None,
        vec![],
    ));
    let ma = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let mb = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let mc = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    let md = g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
    let exp = g.add_node(make_node(
        "exposure.dash",
        "dash",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(src, ma, EdgeData::direct(EdgeType::Source));
    g.add_edge(ma, mb, EdgeData::direct(EdgeType::Ref));
    g.add_edge(mb, mc, EdgeData::direct(EdgeType::Ref));
    g.add_edge(mc, exp, EdgeData::direct(EdgeType::Exposure));
    g.add_edge(ma, md, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &HashSet::new());
    let labels: HashSet<String> = collapsed
        .node_indices()
        .map(|i| collapsed[i].label.clone())
        .collect();
    assert!(labels.contains("x"));
    assert!(labels.contains("dash"));
    assert!(!labels.contains("a"));
    assert!(!labels.contains("d")); // leaf model NOT kept in focal mode
    assert_eq!(collapsed.node_count(), 2);
}

#[test]
fn test_collapse_snapshot() {
    // Snapshot test: source -> stg -> int -> final -> exposure
    // All models collapsed, only source and exposure kept
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "raw_x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node(
        "model.stg_x",
        "stg_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let int = g.add_node(make_node(
        "model.int_x",
        "int_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let fin = g.add_node(make_node(
        "model.final_x",
        "final_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let exp = g.add_node(make_node(
        "exposure.dash",
        "dash",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, int, EdgeData::direct(EdgeType::Ref));
    g.add_edge(int, fin, EdgeData::direct(EdgeType::Ref));
    g.add_edge(fin, exp, EdgeData::direct(EdgeType::Exposure));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_collapse_skips_invalid_preserve_index() {
    // Invalid NodeIndex in preserve should be skipped without panic
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("source.a", "a", NodeType::Source, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));

    // Create indexes that are definitely invalid for g (out of range)
    let invalid_from_bound = NodeIndex::new(g.node_bound());
    let preserve = HashSet::from([invalid_from_bound, NodeIndex::new(999)]);
    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &preserve);
    // Both kept: source (in-degree=0) and model (out-degree=0) are endpoints
    assert_eq!(collapsed.node_count(), 2);
}

#[test]
fn test_collapse_preserves_focus_models() {
    // A -> B -> C -> D: collapse with B preserved should keep A, B, D
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("source.a", "a", NodeType::Source, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    let d = g.add_node(make_node(
        "exposure.dash",
        "dash",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Exposure));

    let preserve = HashSet::from([b]);
    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &preserve);
    assert_eq!(collapsed.node_count(), 3);

    let labels: HashSet<String> = collapsed
        .node_indices()
        .map(|i| collapsed[i].label.clone())
        .collect();
    assert!(labels.contains("a"), "endpoint a should be kept");
    assert!(labels.contains("b"), "focus model b should be preserved");
    assert!(labels.contains("dash"), "endpoint dash should be kept");
}

#[test]
fn test_collapse_snapshot_preserve_focus() {
    // Snapshot: source -> stg -> int -> final -> exposure, with int preserved
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "raw_x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node(
        "model.stg_x",
        "stg_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let int = g.add_node(make_node(
        "model.int_x",
        "int_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let fin = g.add_node(make_node(
        "model.final_x",
        "final_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let exp = g.add_node(make_node(
        "exposure.dash",
        "dash",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, int, EdgeData::direct(EdgeType::Ref));
    g.add_edge(int, fin, EdgeData::direct(EdgeType::Ref));
    g.add_edge(fin, exp, EdgeData::direct(EdgeType::Exposure));

    let preserve = HashSet::from([int]);
    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &preserve);
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_collapse_snapshot_endpoints_fan_out() {
    // source -> stg -> mart_a, source -> stg -> mart_b
    // Endpoints mode: source (in-degree=0), mart_a, mart_b (out-degree=0) kept
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "raw_x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node(
        "model.stg_x",
        "stg_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let mart_a = g.add_node(make_node(
        "model.mart_a",
        "mart_a",
        NodeType::Model,
        None,
        vec![],
    ));
    let mart_b = g.add_node(make_node(
        "model.mart_b",
        "mart_b",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart_a, EdgeData::direct(EdgeType::Ref));
    g.add_edge(stg, mart_b, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_collapse_snapshot_endpoints_leaf_model() {
    // source -> stg -> mart (leaf model, out-degree=0)
    // Endpoints mode keeps mart because it's a topological endpoint.
    // Compare with test_collapse_snapshot_bfs_pseudoendpoint (focal mode).
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "raw_x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node(
        "model.stg_x",
        "stg_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let mart = g.add_node(make_node(
        "model.mart_x",
        "mart_x",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Endpoints, &HashSet::new());
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_collapse_snapshot_bfs_pseudoendpoint() {
    // Same graph as above, but focal mode: mart is collapsed because
    // it's a Model, not Source/Exposure — BFS pseudo-endpoints are ignored.
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.x",
        "raw_x",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node(
        "model.stg_x",
        "stg_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let mart = g.add_node(make_node(
        "model.mart_x",
        "mart_x",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));

    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &HashSet::new());
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_collapse_snapshot_multiple_focus_models() {
    // Two sources -> stg -> mart_a, mart_b -> exposure
    // mart_a and mart_b are preserved as focus models
    let mut g = LineageGraph::new();
    let src_a = g.add_node(make_node(
        "source.raw.a",
        "raw_a",
        NodeType::Source,
        None,
        vec![],
    ));
    let src_b = g.add_node(make_node(
        "source.raw.b",
        "raw_b",
        NodeType::Source,
        None,
        vec![],
    ));
    let stg = g.add_node(make_node(
        "model.stg_x",
        "stg_x",
        NodeType::Model,
        None,
        vec![],
    ));
    let mart_a = g.add_node(make_node(
        "model.mart_a",
        "mart_a",
        NodeType::Model,
        None,
        vec![],
    ));
    let mart_b = g.add_node(make_node(
        "model.mart_b",
        "mart_b",
        NodeType::Model,
        None,
        vec![],
    ));
    let exp = g.add_node(make_node(
        "exposure.dash",
        "dash",
        NodeType::Exposure,
        None,
        vec![],
    ));
    g.add_edge(src_a, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(src_b, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart_a, EdgeData::direct(EdgeType::Ref));
    g.add_edge(stg, mart_b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(mart_a, exp, EdgeData::direct(EdgeType::Exposure));
    g.add_edge(mart_b, exp, EdgeData::direct(EdgeType::Exposure));

    let preserve = HashSet::from([mart_a, mart_b]);
    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &preserve);
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_collapse_snapshot_no_source_exposure() {
    // All-model graph: a -> b -> c with b preserved
    // No Source/Exposure in graph; only the preserved focus model remains
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
    let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));

    let preserve = HashSet::from([b]);
    let collapsed = collapse_intermediate(&g, CollapseMode::Focal, &preserve);
    insta::assert_snapshot!(render_mermaid(&collapsed));
}

#[test]
fn test_snapshot_transitive_node_type_filter() {
    // source -> seed -> seed -> model: filter to source,model
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.events",
        "events",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node(
        "seed.raw_events",
        "raw_events",
        NodeType::Seed,
        None,
        vec![],
    ));
    let c = g.add_node(make_node(
        "seed.cleaned_events",
        "cleaned_events",
        NodeType::Seed,
        None,
        vec![],
    ));
    let d = g.add_node(make_node(
        "model.mart_events",
        "mart_events",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    insta::assert_snapshot!(render_mermaid(&filtered));
}

#[test]
fn test_snapshot_transitive_select_filter() {
    // A -> B -> C -> D -> E: selector keeps A, C, E (tag:keep)
    // B and D are excluded => transitive edges A->C (via 1) and C->E (via 1)
    let mut g = LineageGraph::new();
    g.add_node(make_node(
        "model.a",
        "a",
        NodeType::Model,
        None,
        vec!["keep".into()],
    ));
    g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
    g.add_node(make_node(
        "model.c",
        "c",
        NodeType::Model,
        None,
        vec!["keep".into()],
    ));
    g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
    g.add_node(make_node(
        "model.e",
        "e",
        NodeType::Model,
        None,
        vec!["keep".into()],
    ));
    let idx: Vec<_> = g.node_indices().collect();
    g.add_edge(idx[0], idx[1], EdgeData::direct(EdgeType::Ref));
    g.add_edge(idx[1], idx[2], EdgeData::direct(EdgeType::Ref));
    g.add_edge(idx[2], idx[3], EdgeData::direct(EdgeType::Ref));
    g.add_edge(idx[3], idx[4], EdgeData::direct(EdgeType::Ref));

    let selectors = parse_selectors("tag:keep");
    let filtered = filter_graph(&g, &[], None, None, &selectors, true).unwrap();
    insta::assert_snapshot!(render_mermaid(&filtered));
}

#[test]
fn test_snapshot_transitive_select_with_node_type() {
    // source -> seed -> model -> seed -> model: focus on all, node-type=source,model
    // Both filter_graph (transitive) and filter_output_node_types (transitive)
    let mut g = LineageGraph::new();
    let a = g.add_node(make_node(
        "source.raw.a",
        "a",
        NodeType::Source,
        None,
        vec![],
    ));
    let b = g.add_node(make_node(
        "seed.staging",
        "staging",
        NodeType::Seed,
        None,
        vec![],
    ));
    let c = g.add_node(make_node(
        "model.intermediate",
        "intermediate",
        NodeType::Model,
        None,
        vec![],
    ));
    let d = g.add_node(make_node(
        "seed.lookup",
        "lookup",
        NodeType::Seed,
        None,
        vec![],
    ));
    let e = g.add_node(make_node(
        "model.final",
        "final",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, d, EdgeData::direct(EdgeType::Ref));
    g.add_edge(d, e, EdgeData::direct(EdgeType::Ref));

    let filtered = filter_output_node_types(&g, &["source".into(), "model".into()], true);
    insta::assert_snapshot!(render_mermaid(&filtered));
}
