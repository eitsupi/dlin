use anyhow::Result;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use petgraph::Direction;
use std::collections::{HashSet, VecDeque};

use crate::error::DbtLineageError;

use super::types::*;

/// Result of a node name lookup.
enum NodeLookupResult {
    /// No matching node found.
    NotFound,
    /// Exactly one node matched (exact or suffix).
    Found(NodeIndex),
    /// Multiple nodes matched the suffix fallback. The first is used,
    /// and all matching unique_ids are returned for caller-side warnings.
    Ambiguous(NodeIndex, Vec<String>),
}

/// Find a node by name, using a two-pass approach:
/// 1. Exact match on label or unique_id
/// 2. Suffix match on unique_id (`.{name}`)
fn find_node_by_name(graph: &LineageGraph, name: &str) -> NodeLookupResult {
    // Pass 1: exact label or unique_id
    let exact = graph.node_indices().find(|&idx| {
        let node = &graph[idx];
        node.label == name || node.unique_id == name
    });
    if let Some(idx) = exact {
        return NodeLookupResult::Found(idx);
    }

    // Pass 2: suffix match
    let suffix = format!(".{}", name);
    let matches: Vec<NodeIndex> = graph
        .node_indices()
        .filter(|&idx| graph[idx].unique_id.ends_with(&suffix))
        .collect();

    match matches.len() {
        0 => NodeLookupResult::NotFound,
        1 => NodeLookupResult::Found(matches[0]),
        _ => {
            let ids = matches.iter().map(|&idx| graph[idx].unique_id.clone()).collect();
            NodeLookupResult::Ambiguous(matches[0], ids)
        }
    }
}

/// Resolve a node by name, returning the node index or an error.
/// Warns to stderr when the suffix fallback matches multiple nodes.
pub fn resolve_node_by_name(graph: &LineageGraph, name: &str) -> Result<NodeIndex> {
    match find_node_by_name(graph, name) {
        NodeLookupResult::Found(idx) => Ok(idx),
        NodeLookupResult::Ambiguous(idx, ids) => {
            eprintln!(
                "Warning: '{}' matched multiple nodes: {}. Using the first match.",
                name,
                ids.join(", ")
            );
            Ok(idx)
        }
        NodeLookupResult::NotFound => {
            Err(DbtLineageError::ModelNotFound(name.to_string()).into())
        }
    }
}

/// Resolve a node by name, returning `Some(index)` or `None` with a warning.
/// Unlike [`resolve_node_by_name`], this does not return an error for missing
/// nodes, making it suitable for batch lookups where skipping is preferred.
pub fn try_resolve_node(graph: &LineageGraph, name: &str) -> Option<NodeIndex> {
    match find_node_by_name(graph, name) {
        NodeLookupResult::Found(idx) => Some(idx),
        NodeLookupResult::Ambiguous(idx, ids) => {
            eprintln!(
                "Warning: '{}' matched multiple nodes: {}. Using the first match.",
                name,
                ids.join(", ")
            );
            Some(idx)
        }
        NodeLookupResult::NotFound => {
            eprintln!("Warning: '{}' not found in the graph, skipping.", name);
            None
        }
    }
}

/// Configuration for which node types to include
pub struct NodeTypeFilter {
    pub include_tests: bool,
    pub include_seeds: bool,
    pub include_snapshots: bool,
    pub include_exposures: bool,
}

/// A parsed selector expression
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// Match nodes whose tags contain the given value
    Tag(String),
    /// Match nodes whose file_path starts with the given path prefix
    Path(String),
    /// Match nodes whose label equals the given model name
    ModelName(String),
}

/// Parse a comma-separated selector string into a list of `Selector` values.
///
/// Syntax:
/// - `tag:nightly` -> `Selector::Tag("nightly")`
/// - `path:models/staging` -> `Selector::Path("models/staging")`
/// - `orders` -> `Selector::ModelName("orders")`
pub fn parse_selectors(input: &str) -> Vec<Selector> {
    input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(tag) = s.strip_prefix("tag:") {
                Selector::Tag(tag.to_string())
            } else if let Some(path) = s.strip_prefix("path:") {
                Selector::Path(path.to_string())
            } else {
                Selector::ModelName(s.to_string())
            }
        })
        .collect()
}

/// Check if a single node matches any of the given selectors (union / OR logic).
fn node_matches_any_selector(node: &NodeData, selectors: &[Selector]) -> bool {
    selectors.iter().any(|sel| match sel {
        Selector::Tag(tag) => node.tags.contains(tag),
        Selector::Path(prefix) => node
            .file_path
            .as_ref()
            .map(|fp| fp.to_string_lossy().starts_with(prefix.as_str()))
            .unwrap_or(false),
        Selector::ModelName(name) => node.label == *name,
    })
}

/// Return the set of node indices that match any of the given selectors.
pub fn apply_selectors(graph: &LineageGraph, selectors: &[Selector]) -> HashSet<NodeIndex> {
    graph
        .node_indices()
        .filter(|&idx| node_matches_any_selector(&graph[idx], selectors))
        .collect()
}

/// Filter the graph based on focus models, distance, selectors, and node types
pub fn filter_graph(
    graph: &LineageGraph,
    focus_models: &[String],
    upstream: Option<usize>,
    downstream: Option<usize>,
    type_filter: &NodeTypeFilter,
    selectors: &[Selector],
) -> Result<LineageGraph> {
    // Check for cycles
    if petgraph::algo::is_cyclic_directed(graph) {
        return Err(DbtLineageError::CycleDetected.into());
    }

    let mut keep_nodes: HashSet<NodeIndex> = HashSet::new();

    if !focus_models.is_empty() {
        for model_name in focus_models {
            let Some(focus_idx) = try_resolve_node(graph, model_name) else {
                continue;
            };

            keep_nodes.insert(focus_idx);

            // BFS upstream (predecessors)
            bfs_collect(
                graph,
                focus_idx,
                Direction::Incoming,
                upstream,
                &mut keep_nodes,
            );

            // BFS downstream (successors)
            bfs_collect(
                graph,
                focus_idx,
                Direction::Outgoing,
                downstream,
                &mut keep_nodes,
            );
        }
    } else {
        // No focus models -- keep all nodes
        keep_nodes.extend(graph.node_indices());
    }

    // Apply selector filter: intersect with BFS results (or use as base set)
    if !selectors.is_empty() {
        let selector_matches = apply_selectors(graph, selectors);
        if !focus_models.is_empty() {
            // Intersect: keep only nodes that match both BFS and selectors
            keep_nodes = keep_nodes
                .intersection(&selector_matches)
                .copied()
                .collect();
        } else {
            // No focus models: use selectors as the base set
            keep_nodes = selector_matches;
        }
    }

    let keep_nodes = apply_type_filter(graph, keep_nodes, type_filter);

    Ok(build_subgraph(graph, &keep_nodes))
}

/// Filter a set of node indices by node type
fn apply_type_filter(
    graph: &LineageGraph,
    nodes: HashSet<NodeIndex>,
    type_filter: &NodeTypeFilter,
) -> HashSet<NodeIndex> {
    nodes
        .into_iter()
        .filter(|&idx| {
            let node = &graph[idx];
            match node.node_type {
                NodeType::Test => type_filter.include_tests,
                NodeType::Seed => type_filter.include_seeds,
                NodeType::Snapshot => type_filter.include_snapshots,
                NodeType::Exposure => type_filter.include_exposures,
                NodeType::Model | NodeType::Source | NodeType::Phantom => true,
            }
        })
        .collect()
}

/// Build a new graph containing only the specified nodes and their interconnecting edges
fn build_subgraph(graph: &LineageGraph, keep_nodes: &HashSet<NodeIndex>) -> LineageGraph {
    let mut new_graph = LineageGraph::new();
    let mut index_map: std::collections::HashMap<NodeIndex, NodeIndex> =
        std::collections::HashMap::new();

    for &old_idx in keep_nodes {
        let node = graph[old_idx].clone();
        let new_idx = new_graph.add_node(node);
        index_map.insert(old_idx, new_idx);
    }

    for edge in graph.edge_references() {
        let source = edge.source();
        let target = edge.target();
        if let (Some(&new_source), Some(&new_target)) =
            (index_map.get(&source), index_map.get(&target))
        {
            new_graph.add_edge(new_source, new_target, edge.weight().clone());
        }
    }

    new_graph
}

/// Known node type labels for validation.
pub const KNOWN_NODE_TYPE_LABELS: &[&str] = &["model", "source", "seed", "snapshot", "test", "exposure"];

/// Return unrecognized node type names from the given list.
pub fn validate_node_type_names(type_names: &[String]) -> Vec<String> {
    type_names
        .iter()
        .filter(|t| !KNOWN_NODE_TYPE_LABELS.iter().any(|k| k.eq_ignore_ascii_case(t)))
        .cloned()
        .collect()
}

/// Filter graph to keep only nodes whose type label matches one of the given names.
/// If `type_names` is empty, the graph is returned unchanged.
/// Comparison is case-insensitive.
///
/// Note: edges between kept nodes that pass through filtered-out intermediaries
/// are dropped. For example, `A → B → C` filtered to only A and C will show them
/// as disconnected nodes.
pub fn filter_output_node_types(graph: &LineageGraph, type_names: &[String]) -> LineageGraph {
    if type_names.is_empty() {
        return graph.clone();
    }
    let keep: HashSet<NodeIndex> = graph
        .node_indices()
        .filter(|&idx| {
            let label = graph[idx].node_type.label();
            type_names.iter().any(|t| t.eq_ignore_ascii_case(label))
        })
        .collect();
    build_subgraph(graph, &keep)
}

/// BFS traversal collecting nodes up to max_depth levels away
fn bfs_collect(
    graph: &LineageGraph,
    start: NodeIndex,
    direction: Direction,
    max_depth: Option<usize>,
    collected: &mut HashSet<NodeIndex>,
) {
    let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
    queue.push_back((start, 0));
    let mut visited: HashSet<NodeIndex> = HashSet::new();
    visited.insert(start);

    while let Some((node, depth)) = queue.pop_front() {
        // Skip expansion if at max depth
        if max_depth.is_some_and(|max| depth >= max) {
            continue;
        }

        for e in graph.edges_directed(node, direction) {
            let neighbor = match direction {
                Direction::Incoming => e.source(),
                Direction::Outgoing => e.target(),
            };
            if visited.insert(neighbor) {
                collected.insert(neighbor);
                queue.push_back((neighbor, depth + 1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        g.add_edge(
            a,
            b,
            EdgeData {
                edge_type: EdgeType::Source,
            },
        );
        g.add_edge(
            b,
            c,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        g.add_edge(
            c,
            d,
            EdgeData {
                edge_type: EdgeType::Exposure,
            },
        );
        g
    }

    #[test]
    fn test_filter_no_focus() {
        let g = make_test_graph();
        let filter = NodeTypeFilter {
            include_tests: false,
            include_seeds: false,
            include_snapshots: false,
            include_exposures: true,
        };
        let filtered = filter_graph(&g, &[], None, None, &filter, &[]).unwrap();
        assert_eq!(filtered.node_count(), 4);
    }

    #[test]
    fn test_filter_focus_upstream_1() {
        let g = make_test_graph();
        let filter = NodeTypeFilter {
            include_tests: false,
            include_seeds: false,
            include_snapshots: false,
            include_exposures: true,
        };
        // Focus on "orders" with 1 upstream, 0 downstream
        let filtered = filter_graph(&g, &["orders".into()], Some(1), Some(0), &filter, &[]).unwrap();
        // Should have: orders + stg_orders (1 upstream)
        assert_eq!(filtered.node_count(), 2);
    }

    #[test]
    fn test_filter_excludes_exposures() {
        let g = make_test_graph();
        let filter = NodeTypeFilter {
            include_tests: false,
            include_seeds: false,
            include_snapshots: false,
            include_exposures: false,
        };
        let filtered = filter_graph(&g, &[], None, None, &filter, &[]).unwrap();
        // Exposure should be excluded
        assert_eq!(filtered.node_count(), 3);
    }

    #[test]
    fn test_filter_model_not_found_skips_with_warning() {
        let g = make_test_graph();
        let filter = NodeTypeFilter {
            include_tests: false,
            include_seeds: false,
            include_snapshots: false,
            include_exposures: true,
        };
        // Not-found models are skipped, resulting in an empty graph
        let filtered =
            filter_graph(&g, &["nonexistent".into()], None, None, &filter, &[]).unwrap();
        assert_eq!(filtered.node_count(), 0);
    }

    #[test]
    fn test_filter_focus_source_by_label() {
        let g = make_test_graph();
        // Focus on source node using its label "raw.orders"
        let filtered =
            filter_graph(&g, &["raw.orders".into()], None, Some(1), &default_type_filter(), &[])
                .unwrap();
        // raw.orders + stg_orders (1 downstream)
        assert_eq!(filtered.node_count(), 2);
    }

    #[test]
    fn test_filter_focus_source_by_unique_id() {
        let g = make_test_graph();
        // Focus on source node using full unique_id
        let filtered = filter_graph(
            &g,
            &["source.raw.orders".into()],
            None,
            Some(1),
            &default_type_filter(),
            &[],
        )
        .unwrap();
        // source.raw.orders + stg_orders (1 downstream)
        assert_eq!(filtered.node_count(), 2);
    }

    #[test]
    fn test_filter_focus_exposure_by_label() {
        let g = make_test_graph();
        let filtered =
            filter_graph(&g, &["dashboard".into()], Some(1), None, &default_type_filter(), &[])
                .unwrap();
        // dashboard + orders (1 upstream)
        assert_eq!(filtered.node_count(), 2);
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
            &default_type_filter(),
            &[],
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
            &default_type_filter(),
            &[],
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
            &default_type_filter(),
            &[],
        )
        .unwrap();
        // Only "orders" should remain
        assert_eq!(filtered.node_count(), 1);
        assert_eq!(filtered[filtered.node_indices().next().unwrap()].label, "orders");
    }

    #[test]
    fn test_filter_multiple_focus_all_invalid() {
        let g = make_test_graph();
        let filtered = filter_graph(
            &g,
            &["no_such_a".into(), "no_such_b".into()],
            None,
            None,
            &default_type_filter(),
            &[],
        )
        .unwrap();
        assert_eq!(filtered.node_count(), 0);
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
            &default_type_filter(),
            &[],
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

    // -- Selector parsing tests -----------------------------------------------

    #[test]
    fn test_parse_selectors_tag() {
        let selectors = parse_selectors("tag:nightly");
        assert_eq!(selectors, vec![Selector::Tag("nightly".into())]);
    }

    #[test]
    fn test_parse_selectors_path() {
        let selectors = parse_selectors("path:models/staging");
        assert_eq!(selectors, vec![Selector::Path("models/staging".into())]);
    }

    #[test]
    fn test_parse_selectors_model_name() {
        let selectors = parse_selectors("orders");
        assert_eq!(selectors, vec![Selector::ModelName("orders".into())]);
    }

    #[test]
    fn test_parse_selectors_multiple() {
        let selectors = parse_selectors("tag:nightly,path:models/staging,orders");
        assert_eq!(
            selectors,
            vec![
                Selector::Tag("nightly".into()),
                Selector::Path("models/staging".into()),
                Selector::ModelName("orders".into()),
            ]
        );
    }

    #[test]
    fn test_parse_selectors_whitespace_handling() {
        let selectors = parse_selectors(" tag:nightly , path:models/staging , orders ");
        assert_eq!(
            selectors,
            vec![
                Selector::Tag("nightly".into()),
                Selector::Path("models/staging".into()),
                Selector::ModelName("orders".into()),
            ]
        );
    }

    #[test]
    fn test_parse_selectors_empty_string() {
        let selectors = parse_selectors("");
        assert!(selectors.is_empty());
    }

    #[test]
    fn test_parse_selectors_trailing_comma() {
        let selectors = parse_selectors("orders,");
        assert_eq!(selectors, vec![Selector::ModelName("orders".into())]);
    }

    // -- Selector-based graph filtering tests ---------------------------------

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

        g.add_edge(
            a,
            b,
            EdgeData {
                edge_type: EdgeType::Source,
            },
        );
        g.add_edge(
            b,
            c,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        g.add_edge(
            c,
            d,
            EdgeData {
                edge_type: EdgeType::Exposure,
            },
        );
        g
    }

    fn default_type_filter() -> NodeTypeFilter {
        NodeTypeFilter {
            include_tests: true,
            include_seeds: true,
            include_snapshots: true,
            include_exposures: true,
        }
    }

    #[test]
    fn test_selector_by_tag() {
        let g = make_tagged_graph();
        let selectors = parse_selectors("tag:nightly");
        let filtered =
            filter_graph(&g, &[], None, None, &default_type_filter(), &selectors).unwrap();
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
        let filtered =
            filter_graph(&g, &[], None, None, &default_type_filter(), &selectors).unwrap();
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
    fn test_selector_by_model_name() {
        let g = make_tagged_graph();
        let selectors = parse_selectors("orders");
        let filtered =
            filter_graph(&g, &[], None, None, &default_type_filter(), &selectors).unwrap();
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
        let filtered =
            filter_graph(&g, &[], None, None, &default_type_filter(), &selectors).unwrap();
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
        let filtered =
            filter_graph(&g, &[], None, None, &default_type_filter(), &selectors).unwrap();
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
        let filtered = filter_graph(
            &g,
            &["orders".into()],
            None,
            None,
            &default_type_filter(),
            &selectors,
        )
        .unwrap();
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
        let filtered =
            filter_graph(&g, &[], None, None, &default_type_filter(), &no_selectors).unwrap();
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

    #[test]
    fn test_node_matches_any_selector_tag() {
        let node = make_node(
            "model.x",
            "x",
            NodeType::Model,
            Some(PathBuf::from("models/x.sql")),
            vec!["nightly".into(), "daily".into()],
        );
        assert!(node_matches_any_selector(
            &node,
            &[Selector::Tag("nightly".into())]
        ));
        assert!(node_matches_any_selector(
            &node,
            &[Selector::Tag("daily".into())]
        ));
        assert!(!node_matches_any_selector(
            &node,
            &[Selector::Tag("weekly".into())]
        ));
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
            &[Selector::Path("models/staging".into())]
        ));
        assert!(node_matches_any_selector(
            &node,
            &[Selector::Path("models".into())]
        ));
        assert!(!node_matches_any_selector(
            &node,
            &[Selector::Path("tests".into())]
        ));
    }

    #[test]
    fn test_node_matches_any_selector_path_none() {
        let node = make_node("exposure.x", "x", NodeType::Exposure, None, vec![]);
        assert!(!node_matches_any_selector(
            &node,
            &[Selector::Path("models".into())]
        ));
    }

    #[test]
    fn test_node_matches_any_selector_model_name() {
        let node = make_node("model.orders", "orders", NodeType::Model, None, vec![]);
        assert!(node_matches_any_selector(
            &node,
            &[Selector::ModelName("orders".into())]
        ));
        assert!(!node_matches_any_selector(
            &node,
            &[Selector::ModelName("customers".into())]
        ));
    }

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
        g.add_edge(
            model,
            test,
            EdgeData {
                edge_type: EdgeType::Test,
            },
        );
        g.add_edge(
            seed,
            model,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        g.add_edge(
            model,
            snap,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );

        // Exclude all optional types
        let filter = NodeTypeFilter {
            include_tests: false,
            include_seeds: false,
            include_snapshots: false,
            include_exposures: false,
        };
        let filtered = filter_graph(&g, &[], None, None, &filter, &[]).unwrap();
        assert_eq!(filtered.node_count(), 1); // Only the model remains
        let labels: Vec<String> = filtered
            .node_indices()
            .map(|i| filtered[i].label.clone())
            .collect();
        assert!(labels.contains(&"orders".to_string()));

        // Include tests only
        let filter2 = NodeTypeFilter {
            include_tests: true,
            include_seeds: false,
            include_snapshots: false,
            include_exposures: false,
        };
        let filtered2 = filter_graph(&g, &[], None, None, &filter2, &[]).unwrap();
        assert_eq!(filtered2.node_count(), 2); // model + test
    }

    // -- Output node-type filter tests -----------------------------------------

    #[test]
    fn test_filter_output_node_types_empty_returns_all() {
        let g = make_test_graph();
        let filtered = filter_output_node_types(&g, &[]);
        assert_eq!(filtered.node_count(), g.node_count());
    }

    #[test]
    fn test_filter_output_node_types_model_only() {
        let g = make_test_graph();
        let filtered = filter_output_node_types(&g, &["model".into()]);
        assert_eq!(filtered.node_count(), 2);
        for idx in filtered.node_indices() {
            assert_eq!(filtered[idx].node_type, NodeType::Model);
        }
    }

    #[test]
    fn test_filter_output_node_types_multiple() {
        let g = make_test_graph();
        let filtered = filter_output_node_types(&g, &["model".into(), "source".into()]);
        assert_eq!(filtered.node_count(), 3);
    }

    #[test]
    fn test_filter_output_node_types_no_match() {
        let g = make_test_graph();
        let filtered = filter_output_node_types(&g, &["test".into()]);
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
        let filtered = filter_output_node_types(&g, &["Model".into()]);
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
        g.add_edge(
            a,
            b,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );
        g.add_edge(
            b,
            a,
            EdgeData {
                edge_type: EdgeType::Ref,
            },
        );

        let result = filter_graph(&g, &[], None, None, &default_type_filter(), &[]);
        assert!(result.is_err());
    }
}
