use super::*;

#[test]
fn test_parse_selectors_tag() {
    let selectors = parse_selectors("tag:nightly");
    assert_eq!(selectors.len(), 1);
    assert!(matches!(&selectors[0], Selector::Tag(_)));
}

#[test]
fn test_parse_selectors_path() {
    let selectors = parse_selectors("path:models/staging");
    assert_eq!(selectors.len(), 1);
    assert!(matches!(&selectors[0], Selector::Path(_)));
}

#[test]
fn test_parse_selectors_model_name() {
    let selectors = parse_selectors("orders");
    assert_eq!(selectors.len(), 1);
    assert!(matches!(&selectors[0], Selector::ModelName(_)));
}

#[test]
fn test_parse_selectors_multiple() {
    let selectors = parse_selectors("tag:nightly,path:models/staging,orders");
    assert_eq!(selectors.len(), 3);
    assert!(matches!(&selectors[0], Selector::Tag(_)));
    assert!(matches!(&selectors[1], Selector::Path(_)));
    assert!(matches!(&selectors[2], Selector::ModelName(_)));
}

#[test]
fn test_parse_selectors_whitespace_handling() {
    let selectors = parse_selectors(" tag:nightly , path:models/staging , orders ");
    assert_eq!(selectors.len(), 3);
    assert!(matches!(&selectors[0], Selector::Tag(_)));
    assert!(matches!(&selectors[1], Selector::Path(_)));
    assert!(matches!(&selectors[2], Selector::ModelName(_)));
}

#[test]
fn test_parse_selectors_empty_string() {
    let selectors = parse_selectors("");
    assert!(selectors.is_empty());
}

#[test]
fn test_parse_selectors_trailing_comma() {
    let selectors = parse_selectors("orders,");
    assert_eq!(selectors.len(), 1);
    assert!(matches!(&selectors[0], Selector::ModelName(_)));
}

#[test]
fn test_selector_by_tag() {
    let g = make_tagged_graph();
    let selectors = parse_selectors("tag:nightly");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 1);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"stg_orders".to_string()));
}

#[test]
fn test_selector_by_path() {
    let g = make_tagged_graph();
    let selectors = parse_selectors("path:models/staging");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    // Should match: raw.orders (schema.yml in models/staging) and stg_orders
    assert_eq!(filtered.node_count(), 2);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"raw.orders".to_string()));
    assert!(labels.contains(&"stg_orders".to_string()));
}

#[test]
fn test_selector_by_path_glob() {
    let g = make_tagged_graph();
    // **&#x2F;staging/** should match the same nodes as prefix "models/staging"
    let selectors = parse_selectors("path:**/staging/**");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 2);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"raw.orders".to_string()));
    assert!(labels.contains(&"stg_orders".to_string()));
}

#[test]
fn test_selector_by_path_glob_extension() {
    let g = make_tagged_graph();
    // Match only .sql files under staging
    let selectors = parse_selectors("path:models/staging/*.sql");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 1);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"stg_orders".to_string()));
}

#[test]
fn test_selector_by_model_name() {
    let g = make_tagged_graph();
    let selectors = parse_selectors("orders");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 1);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"orders".to_string()));
}

#[test]
fn test_selector_union_multiple() {
    let g = make_tagged_graph();
    // tag:nightly matches stg_orders, model name "orders" matches orders
    let selectors = parse_selectors("tag:nightly,orders");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 2);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"stg_orders".to_string()));
    assert!(labels.contains(&"orders".to_string()));
}

#[test]
fn test_selector_no_matches() {
    let g = make_tagged_graph();
    let selectors = parse_selectors("tag:nonexistent");
    let filtered = filter_graph(&g, &[], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 0);
}

#[test]
fn test_selector_with_focus_intersects() {
    let g = make_tagged_graph();
    // Focus on "orders" with full upstream, then select only tag:nightly
    // BFS from "orders" upstream: raw.orders, stg_orders, orders
    // BFS downstream: dashboard
    // Selector tag:nightly matches only stg_orders
    // Intersection: stg_orders
    let selectors = parse_selectors("tag:nightly");
    let filtered = filter_graph(&g, &["orders".into()], None, None, &selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 1);
    let labels: Vec<String> = filtered
        .node_indices()
        .map(|i| filtered[i].label.clone())
        .collect();
    assert!(labels.contains(&"stg_orders".to_string()));
}

#[test]
fn test_selector_empty_does_not_filter() {
    let g = make_tagged_graph();
    let no_selectors: Vec<Selector> = vec![];
    let filtered = filter_graph(&g, &[], None, None, &no_selectors, false).unwrap();
    assert_eq!(filtered.node_count(), 4);
}

#[test]
fn test_apply_selectors_directly() {
    let g = make_tagged_graph();
    let selectors = parse_selectors("tag:daily,stg_orders");
    let matched = apply_selectors(&g, &selectors);
    // tag:daily matches orders, stg_orders matches stg_orders
    assert_eq!(matched.len(), 2);
}

/// Helper: parse a single selector string and return the first selector.
fn sel(input: &str) -> Vec<Selector> {
    parse_selectors(input)
}

#[test]
fn test_node_matches_any_selector_tag() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        Some(PathBuf::from("models/x.sql")),
        vec!["nightly".into(), "daily".into()],
    );
    assert!(node_matches_any_selector(&node, &sel("tag:nightly")));
    assert!(node_matches_any_selector(&node, &sel("tag:daily")));
    assert!(!node_matches_any_selector(&node, &sel("tag:weekly")));
}

#[test]
fn test_node_matches_any_selector_tag_glob() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        None,
        vec!["nightly".into(), "finance_v2".into()],
    );
    assert!(node_matches_any_selector(&node, &sel("tag:night*")));
    assert!(node_matches_any_selector(&node, &sel("tag:finance_v?")));
    assert!(!node_matches_any_selector(&node, &sel("tag:daily*")));
}

#[test]
fn test_node_matches_any_selector_model_name_glob() {
    let node = make_node(
        "model.stg_orders",
        "stg_orders",
        NodeType::Model,
        None,
        vec![],
    );
    assert!(node_matches_any_selector(&node, &sel("stg_*")));
    assert!(node_matches_any_selector(&node, &sel("*orders")));
    assert!(!node_matches_any_selector(&node, &sel("fct_*")));
}

#[test]
fn test_node_matches_any_selector_path() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        Some(PathBuf::from("models/staging/x.sql")),
        vec![],
    );
    assert!(node_matches_any_selector(
        &node,
        &sel("path:models/staging")
    ));
    assert!(node_matches_any_selector(&node, &sel("path:models")));
    assert!(!node_matches_any_selector(&node, &sel("path:tests")));
}

#[test]
fn test_node_matches_any_selector_path_glob_doublestar() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        Some(PathBuf::from("models/staging/stg_orders.sql")),
        vec![],
    );
    assert!(node_matches_any_selector(&node, &sel("path:**/staging/**")));
    assert!(node_matches_any_selector(
        &node,
        &sel("path:models/**/stg_orders.sql")
    ));
    assert!(!node_matches_any_selector(&node, &sel("path:**/marts/**")));
}

#[test]
fn test_node_matches_any_selector_path_glob_star() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        Some(PathBuf::from("models/staging/stg_orders.sql")),
        vec![],
    );
    assert!(node_matches_any_selector(
        &node,
        &sel("path:models/staging/*.sql")
    ));
    assert!(!node_matches_any_selector(&node, &sel("path:models/*.sql")));
}

#[test]
fn test_node_matches_any_selector_path_glob_question() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        Some(PathBuf::from("models/staging/stg_orders.sql")),
        vec![],
    );
    assert!(node_matches_any_selector(
        &node,
        &sel("path:models/staging/stg_order?.sql")
    ));
    assert!(!node_matches_any_selector(
        &node,
        &sel("path:models/staging/stg_order??.sql")
    ));
}

#[test]
fn test_node_matches_any_selector_path_glob_invalid_pattern() {
    let node = make_node(
        "model.x",
        "x",
        NodeType::Model,
        Some(PathBuf::from("models/x.sql")),
        vec![],
    );
    // Invalid glob pattern should not match (not panic)
    assert!(!node_matches_any_selector(&node, &sel("path:[invalid")));
}

#[test]
fn test_node_matches_any_selector_path_none() {
    let node = make_node("exposure.x", "x", NodeType::Exposure, None, vec![]);
    assert!(!node_matches_any_selector(&node, &sel("path:models")));
}

#[test]
fn test_node_matches_any_selector_model_name() {
    let node = make_node("model.orders", "orders", NodeType::Model, None, vec![]);
    assert!(node_matches_any_selector(&node, &sel("orders")));
    assert!(!node_matches_any_selector(&node, &sel("customers")));
}

#[test]
fn test_selector_alias_spellings_follow_focus_lookup_policy() {
    let mut simple_model = make_node(
        "model.project.orders",
        "display",
        NodeType::Model,
        None,
        vec![],
    );
    simple_model.aliases.push("model.orders".to_string());
    assert!(node_matches_any_selector(&simple_model, &sel("orders")));
    assert!(node_matches_any_selector(&simple_model, &sel("ord*")));

    let mut qualified_model = make_node(
        "model.project.metric_model",
        "model",
        NodeType::Model,
        None,
        vec![],
    );
    qualified_model
        .aliases
        .push("model.metric.revenue".to_string());
    let mut metric = make_node(
        "metric.project.revenue",
        "revenue",
        NodeType::Metric,
        None,
        vec![],
    );
    metric.aliases.push("metric.revenue".to_string());

    assert!(node_matches_any_selector(&metric, &sel("metric.revenue")));
    assert!(!node_matches_any_selector(
        &qualified_model,
        &sel("metric.revenue")
    ));
    assert!(node_matches_any_selector(&metric, &sel("metric.*")));
    assert!(!node_matches_any_selector(
        &qualified_model,
        &sel("metric.*")
    ));
}
