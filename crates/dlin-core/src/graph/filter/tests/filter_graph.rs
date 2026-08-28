use super::*;

#[test]
fn test_filter_no_focus_returns_all_nodes() {
    let g = make_test_graph();
    let filtered = filter_graph(&g, &[], None, None, &[], false).unwrap();
    // With no focus and no selectors, all nodes pass through unfiltered
    assert_eq!(filtered.node_count(), 4);
    let types: std::collections::HashSet<&str> = filtered
        .node_indices()
        .map(|i| filtered[i].node_type.label())
        .collect();
    assert!(types.contains("source"), "source node should be present");
    assert!(types.contains("model"), "model nodes should be present");
    assert!(
        types.contains("exposure"),
        "exposure node should be present"
    );
}

#[test]
fn test_filter_focus_upstream_1() {
    let g = make_test_graph();
    // Focus on "orders" with 1 upstream, 0 downstream
    let filtered = filter_graph(&g, &["orders".into()], Some(1), Some(0), &[], false).unwrap();
    // Should have: orders + stg_orders (1 upstream)
    assert_eq!(filtered.node_count(), 2);
}

#[test]
fn test_filter_excludes_exposures_via_output_filter() {
    let g = make_test_graph();
    let filtered = filter_graph(&g, &[], None, None, &[], false).unwrap();
    // Apply output filter to exclude exposures
    let filtered = filter_output_node_types(&filtered, &["model".into(), "source".into()], false);
    assert_eq!(filtered.node_count(), 3);
}

#[test]
fn test_filter_model_not_found_returns_error() {
    let g = make_test_graph();
    // All specified models not found → error
    let result = filter_graph(&g, &["nonexistent".into()], None, None, &[], false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("model not found"));
}

#[test]
fn test_filter_focus_source_by_label() {
    let g = make_test_graph();
    // Focus on source node using its label "raw.orders"
    let filtered = filter_graph(&g, &["raw.orders".into()], None, Some(1), &[], false).unwrap();
    // raw.orders + stg_orders (1 downstream)
    assert_eq!(filtered.node_count(), 2);
}

#[test]
fn test_filter_focus_source_by_unique_id() {
    let g = make_test_graph();
    // Focus on source node using full unique_id
    let filtered =
        filter_graph(&g, &["source.raw.orders".into()], None, Some(1), &[], false).unwrap();
    // source.raw.orders + stg_orders (1 downstream)
    assert_eq!(filtered.node_count(), 2);
}

#[test]
fn test_filter_focus_exposure_by_label() {
    let g = make_test_graph();
    let filtered = filter_graph(&g, &["dashboard".into()], Some(1), None, &[], false).unwrap();
    // dashboard + orders (1 upstream)
    assert_eq!(filtered.node_count(), 2);
}

#[test]
fn test_try_resolve_node_found() {
    let g = make_test_graph();
    assert!(try_resolve_node(&g, "orders").is_some());
}

#[test]
fn test_try_resolve_node_not_found() {
    let g = make_test_graph();
    assert!(try_resolve_node(&g, "nonexistent").is_none());
}

#[test]
fn test_lookup_prefers_exact_canonical_id_over_lower_priority_label() {
    let mut g = LineageGraph::new();
    let canonical = g.add_node(make_node(
        "model.package.orders",
        "orders",
        NodeType::Model,
        None,
        vec![],
    ));
    g.add_node(make_node(
        "model.package.other",
        "model.package.orders",
        NodeType::Model,
        None,
        vec![],
    ));

    let found = resolve_node_by_name(&g, "model.package.orders").unwrap();
    assert_eq!(found, canonical);
}

#[test]
fn test_lookup_ambiguity_uses_sorted_canonical_id() {
    let mut g = LineageGraph::new();
    let package_b = g.add_node(make_node(
        "model.package_b.orders",
        "orders",
        NodeType::Model,
        None,
        vec![],
    ));
    let package_a = g.add_node(make_node(
        "model.package_a.orders",
        "orders",
        NodeType::Model,
        None,
        vec![],
    ));
    g[package_a].aliases.push("model.orders".to_string());
    g[package_b].aliases.push("model.orders".to_string());

    match find_node_by_name(&g, "orders") {
        NodeLookupResult::Ambiguous(index, ids) => {
            assert_eq!(index, package_a);
            assert_eq!(
                ids,
                vec![
                    "model.package_a.orders".to_string(),
                    "model.package_b.orders".to_string()
                ]
            );
        }
        other => panic!("expected an ambiguous lookup, got {other:?}"),
    }

    let found = try_resolve_node_quiet(&g, "orders").unwrap();
    assert_eq!(found, package_a);
}

#[test]
fn test_lookup_qualified_alias_does_not_use_model_shorthand() {
    let mut g = LineageGraph::new();
    let model = g.add_node(make_node(
        "model.project.metric_model",
        "metric.revenue",
        NodeType::Model,
        None,
        vec![],
    ));
    g[model].aliases.push("model.metric.revenue".to_string());
    let metric = g.add_node(make_node(
        "metric.project.revenue",
        "revenue",
        NodeType::Metric,
        None,
        vec![],
    ));
    g[metric].aliases.push("metric.revenue".to_string());

    match find_node_by_name(&g, "metric.revenue") {
        NodeLookupResult::Found(index) => assert_eq!(index, metric),
        other => panic!("expected the metric alias to be found uniquely, got {other:?}"),
    }
}

#[test]
fn test_lookup_bare_model_name_uses_model_shorthand() {
    let mut g = LineageGraph::new();
    let model = g.add_node(make_node(
        "model.project.orders",
        "orders",
        NodeType::Model,
        None,
        vec![],
    ));
    g[model].aliases.push("model.orders".to_string());

    match find_node_by_name(&g, "orders") {
        NodeLookupResult::Found(index) => assert_eq!(index, model),
        other => panic!("expected the bare model alias to be found, got {other:?}"),
    }
}

#[test]
fn test_filter_multiple_focus_models() {
    let g = make_test_graph();
    // Focus on both "raw.orders" and "dashboard" with 0 upstream/downstream
    let filtered = filter_graph(
        &g,
        &["raw.orders".into(), "dashboard".into()],
        Some(0),
        Some(0),
        &[],
        false,
    )
    .unwrap();
    // Should have exactly the two focus nodes
    assert_eq!(filtered.node_count(), 2);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"raw.orders".to_string()));
    assert!(labels.contains(&"dashboard".to_string()));
}

#[test]
fn test_filter_multiple_focus_models_with_depth() {
    let g = make_test_graph();
    // Focus on "raw.orders" (downstream 1) and "dashboard" (upstream 1)
    // raw.orders -> stg_orders (1 downstream), dashboard <- orders (1 upstream)
    let filtered = filter_graph(
        &g,
        &["raw.orders".into(), "dashboard".into()],
        Some(1),
        Some(1),
        &[],
        false,
    )
    .unwrap();
    // All 4 nodes should be included
    assert_eq!(filtered.node_count(), 4);
}

#[test]
fn test_filter_multiple_focus_mixed_valid_invalid() {
    let g = make_test_graph();
    // "orders" exists, "nonexistent" does not — should skip the invalid one
    let filtered = filter_graph(
        &g,
        &["orders".into(), "nonexistent".into()],
        Some(0),
        Some(0),
        &[],
        false,
    )
    .unwrap();
    // Only "orders" should remain
    assert_eq!(filtered.node_count(), 1);
    assert_eq!(
        filtered[filtered.node_indices().next().unwrap()].label,
        "orders"
    );
}

#[test]
fn test_filter_multiple_focus_all_invalid() {
    let g = make_test_graph();
    let result = filter_graph(
        &g,
        &["no_such_a".into(), "no_such_b".into()],
        None,
        None,
        &[],
        false,
    );
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("no_such_a"));
    assert!(msg.contains("no_such_b"));
}

#[test]
fn test_filter_multiple_focus_overlapping_neighborhoods() {
    let g = make_test_graph();
    // stg_orders (downstream 1 = orders) and orders (upstream 1 = stg_orders)
    // Neighborhoods overlap — union should be 2 nodes, not 4
    let filtered = filter_graph(
        &g,
        &["stg_orders".into(), "orders".into()],
        Some(1),
        Some(1),
        &[],
        false,
    )
    .unwrap();
    let labels: HashSet<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    // stg_orders (focus) + orders (focus) + raw.orders (upstream of stg) + dashboard (downstream of orders)
    assert!(labels.contains("stg_orders"));
    assert!(labels.contains("orders"));
    assert!(labels.contains("raw.orders"));
    assert!(labels.contains("dashboard"));
    assert_eq!(filtered.node_count(), 4);
}
