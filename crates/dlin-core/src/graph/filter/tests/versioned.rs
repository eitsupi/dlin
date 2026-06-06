use super::*;

#[test]
fn test_find_versioned_model_by_base_name() {
    let g = make_versioned_graph();
    // "my_model" matches the alias on v2 → resolves to v2
    let idx = resolve_node_by_name(&g, "my_model").unwrap();
    assert_eq!(g[idx].unique_id, "model.my_model.v2");
}

#[test]
fn test_find_versioned_model_by_unversioned_unique_id() {
    let g = make_versioned_graph();
    // "model.my_model" is the alias on v2
    let idx = resolve_node_by_name(&g, "model.my_model").unwrap();
    assert_eq!(g[idx].unique_id, "model.my_model.v2");
}

#[test]
fn test_find_versioned_model_by_explicit_version_label() {
    let g = make_versioned_graph();
    // "my_model.v1" exact label match → v1
    let idx = resolve_node_by_name(&g, "my_model.v1").unwrap();
    assert_eq!(g[idx].unique_id, "model.my_model.v1");
}

#[test]
fn test_find_versioned_model_respects_explicit_latest_version() {
    // latest_version=1 even though v2 also exists: "my_model" should resolve to v1
    let mut g = LineageGraph::new();
    let v1 = g.add_node(make_node(
        "model.my_model.v1",
        "my_model.v1",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_node(make_node(
        "model.my_model.v2",
        "my_model.v2",
        NodeType::Model,
        None,
        vec![],
    ));
    // Alias points to v1, not the numerically highest v2
    g[v1].aliases.push("model.my_model".to_string());

    let idx = resolve_node_by_name(&g, "my_model").unwrap();
    assert_eq!(g[idx].unique_id, "model.my_model.v1");
}

#[test]
fn test_find_versioned_model_phantom_not_selected() {
    // A phantom for model.my_model.v99 must not be chosen over the real v2
    let mut g = LineageGraph::new();
    g.add_node(make_node(
        "model.my_model.v99",
        "?:my_model.v99",
        NodeType::Phantom,
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
    g[v2].aliases.push("model.my_model".to_string());

    let idx = resolve_node_by_name(&g, "my_model").unwrap();
    assert_eq!(g[idx].unique_id, "model.my_model.v2");
}

#[test]
fn test_selector_model_name_matches_versioned_base_name() {
    // Selector "my_model" should match only the latest-version node (v2) via its
    // "model.my_model" alias, not the older v1 node.
    let mut g = LineageGraph::new();
    let v1 = g.add_node(make_node(
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
    g[v2].aliases.push("model.my_model".to_string());

    let selectors = parse_selectors("my_model");
    let matched = apply_selectors(&g, &selectors);
    assert_eq!(matched.len(), 1, "exactly the latest version should match");
    assert!(matched.contains(&v2), "v2 should match via alias");
    assert!(!matched.contains(&v1), "v1 should not match");
}

#[test]
fn test_selector_model_name_matches_qualified_alias() {
    // Selector "model.my_model" (fully-qualified alias form) should also find the latest node.
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
    g[v2].aliases.push("model.my_model".to_string());

    let selectors = parse_selectors("model.my_model");
    let matched = apply_selectors(&g, &selectors);
    assert_eq!(matched.len(), 1);
    assert!(matched.contains(&v2));
}
