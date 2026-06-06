use super::*;

#[test]
fn test_search_by_label() {
    let g = make_search_graph();
    let result = filter_by_search(&g, &[re("order")]);
    let labels: HashSet<String> = result
        .node_indices()
        .map(|i| result[i].label.clone())
        .collect();
    assert!(labels.contains("stg_orders"));
    assert!(labels.contains("order_summary"));
    assert!(!labels.contains("stg_customers"));
    assert!(!labels.contains("payments"));
}

#[test]
fn test_search_by_description() {
    let g = make_search_graph();
    // "customer" only appears in stg_customers description
    let result = filter_by_search(&g, &[re("customer")]);
    let labels: HashSet<String> = result
        .node_indices()
        .map(|i| result[i].label.clone())
        .collect();
    assert!(labels.contains("stg_customers"));
    assert_eq!(result.node_count(), 1);
}

#[test]
fn test_search_case_insensitive() {
    let g = make_search_graph();
    let result_lower = filter_by_search(&g, &[re("staging")]);
    let result_upper = filter_by_search(&g, &[re("STAGING")]);
    let result_mixed = filter_by_search(&g, &[re("Staging")]);
    assert_eq!(result_lower.node_count(), result_upper.node_count());
    assert_eq!(result_lower.node_count(), result_mixed.node_count());
    assert_eq!(result_lower.node_count(), 2); // stg_orders and stg_customers descriptions
}

#[test]
fn test_search_no_match() {
    let g = make_search_graph();
    let result = filter_by_search(&g, &[re("nonexistent_xyz")]);
    assert_eq!(result.node_count(), 0);
}

#[test]
fn test_search_empty_patterns_returns_all() {
    let g = make_search_graph();
    let result = filter_by_search(&g, &[]);
    assert_eq!(result.node_count(), g.node_count());
}

#[test]
fn test_search_multiple_patterns_and_logic() {
    let g = make_search_graph();
    // Both "stg" AND "order" must match — only stg_orders qualifies
    let result = filter_by_search(&g, &[re("stg"), re("order")]);
    let labels: HashSet<String> = result
        .node_indices()
        .map(|i| result[i].label.clone())
        .collect();
    assert_eq!(result.node_count(), 1);
    assert!(labels.contains("stg_orders"));
}

#[test]
fn test_search_regex_alternation() {
    let g = make_search_graph();
    // OR via regex alternation: matches stg_orders, stg_customers, payments
    let result = filter_by_search(&g, &[re("customer|payment")]);
    let labels: HashSet<String> = result
        .node_indices()
        .map(|i| result[i].label.clone())
        .collect();
    assert!(labels.contains("stg_customers"));
    assert!(labels.contains("payments"));
    assert!(!labels.contains("stg_orders"));
    assert!(!labels.contains("order_summary"));
}

#[test]
fn test_search_regex_pattern() {
    let g = make_search_graph();
    // Regex: labels starting with "stg_"
    let result = filter_by_search(&g, &[re("^stg_")]);
    let labels: HashSet<String> = result
        .node_indices()
        .map(|i| result[i].label.clone())
        .collect();
    assert!(labels.contains("stg_orders"));
    assert!(labels.contains("stg_customers"));
    assert_eq!(result.node_count(), 2);
}
