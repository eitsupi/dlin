use anyhow::Result;
use petgraph::Direction;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use std::collections::{HashMap, HashSet, VecDeque};

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
            let ids = matches
                .iter()
                .map(|&idx| graph[idx].unique_id.clone())
                .collect();
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
            crate::warn!(
                "'{}' matched multiple nodes: {}. Using the first match.",
                name,
                ids.join(", ")
            );
            Ok(idx)
        }
        NodeLookupResult::NotFound => Err(DbtLineageError::ModelNotFound(name.to_string()).into()),
    }
}

/// Resolve a node by name, returning `Some(index)` or `None` with a warning.
/// Unlike [`resolve_node_by_name`], this does not return an error for missing
/// nodes, making it suitable for batch lookups where skipping is preferred.
pub fn try_resolve_node(graph: &LineageGraph, name: &str) -> Option<NodeIndex> {
    match resolve_node_by_name(graph, name) {
        Ok(idx) => Some(idx),
        Err(e) => {
            crate::warn!("{}, skipping.", e);
            None
        }
    }
}

/// A compiled pattern matcher: either a pre-compiled glob or a plain string.
///
/// This is an implementation detail of [`Selector`]; use [`parse_selectors`] to construct.
#[derive(Debug, Clone)]
pub enum CompiledPattern {
    /// Exact match (for tag / model name without metacharacters)
    Exact(String),
    /// Prefix match (for path without metacharacters)
    Prefix(String),
    /// Pre-compiled glob matcher
    Glob(globset::GlobMatcher),
}

impl CompiledPattern {
    fn matches(&self, value: &str) -> bool {
        match self {
            CompiledPattern::Exact(s) => value == s,
            CompiledPattern::Prefix(s) => value.starts_with(s.as_str()),
            CompiledPattern::Glob(m) => m.is_match(value),
        }
    }
}

/// Compile a glob pattern string, or return `None` if the pattern is invalid.
fn compile_glob(pattern: &str) -> Option<globset::GlobMatcher> {
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .ok()
        .map(|g| g.compile_matcher())
}

/// Return `true` if the pattern contains glob metacharacters.
fn is_glob_pattern(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// A parsed selector expression with pre-compiled matchers.
///
/// Constructed via [`parse_selectors`]; internal fields are opaque.
#[derive(Debug, Clone)]
pub enum Selector {
    /// Match nodes whose tags contain the given value (glob supported)
    Tag(CompiledPattern),
    /// Match nodes whose file_path starts with the given prefix,
    /// or matches a glob pattern when the value contains `*`, `?`, or `[`.
    Path(CompiledPattern),
    /// Match nodes whose label equals the given model name (glob supported)
    ModelName(CompiledPattern),
}

/// Parse a comma-separated selector string into a list of `Selector` values.
///
/// Syntax:
/// - `tag:nightly` -> `Selector::Tag(..)`
/// - `path:models/staging` -> `Selector::Path(..)`
/// - `orders` -> `Selector::ModelName(..)`
///
/// All selectors support glob patterns (`*`, `**`, `?`, `[...]`).
pub fn parse_selectors(input: &str) -> Vec<Selector> {
    input
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| {
            if let Some(tag) = s.strip_prefix("tag:") {
                Selector::Tag(compile_exact_or_glob(tag))
            } else if let Some(path) = s.strip_prefix("path:") {
                Selector::Path(compile_prefix_or_glob(path))
            } else {
                Selector::ModelName(compile_exact_or_glob(s))
            }
        })
        .collect()
}

/// Compile a pattern for tag/model name: glob if metacharacters present, else exact.
fn compile_exact_or_glob(pattern: &str) -> CompiledPattern {
    if is_glob_pattern(pattern) {
        match compile_glob(pattern) {
            Some(m) => CompiledPattern::Glob(m),
            None => CompiledPattern::Exact(pattern.to_string()),
        }
    } else {
        CompiledPattern::Exact(pattern.to_string())
    }
}

/// Compile a pattern for path: glob if metacharacters present, else prefix.
fn compile_prefix_or_glob(pattern: &str) -> CompiledPattern {
    if is_glob_pattern(pattern) {
        match compile_glob(pattern) {
            Some(m) => CompiledPattern::Glob(m),
            None => CompiledPattern::Prefix(pattern.to_string()),
        }
    } else {
        CompiledPattern::Prefix(pattern.to_string())
    }
}

/// Check if a single node matches any of the given selectors (union / OR logic).
fn node_matches_any_selector(node: &NodeData, selectors: &[Selector]) -> bool {
    selectors.iter().any(|sel| match sel {
        Selector::Tag(pat) => node.tags.iter().any(|t| pat.matches(t)),
        Selector::Path(pat) => node
            .file_path
            .as_ref()
            .map(|fp| pat.matches(&fp.to_string_lossy()))
            .unwrap_or(false),
        Selector::ModelName(pat) => pat.matches(&node.label),
    })
}

/// Return the set of node indices that match any of the given selectors.
pub fn apply_selectors(graph: &LineageGraph, selectors: &[Selector]) -> HashSet<NodeIndex> {
    graph
        .node_indices()
        .filter(|&idx| node_matches_any_selector(&graph[idx], selectors))
        .collect()
}

/// Filter the graph based on focus models, distance, and selectors.
///
/// Node type filtering is handled separately by [`filter_output_node_types`].
pub fn filter_graph(
    graph: &LineageGraph,
    focus_models: &[String],
    upstream: Option<usize>,
    downstream: Option<usize>,
    selectors: &[Selector],
    transitive: bool,
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

        // All specified models were not found
        if keep_nodes.is_empty() {
            let names = focus_models
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(DbtLineageError::ModelNotFound(names).into());
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

    if transitive {
        Ok(build_subgraph_with_transitive(graph, &keep_nodes))
    } else {
        Ok(build_subgraph(graph, &keep_nodes))
    }
}

/// Build a new graph containing only the specified nodes and their interconnecting edges.
///
/// Edges between kept nodes that pass through filtered-out intermediaries are dropped.
fn build_subgraph(graph: &LineageGraph, keep_nodes: &HashSet<NodeIndex>) -> LineageGraph {
    let mut new_graph = LineageGraph::new();
    let mut index_map: HashMap<NodeIndex, NodeIndex> = HashMap::new();

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

/// Build a subgraph with transitive edges: when intermediate nodes are removed,
/// edges are added to preserve reachability between kept nodes.
///
/// For each kept node, BFS traverses forward through removed nodes. When another
/// kept node is reached, a transitive edge is added with `collapsed_through` set
/// to the number of intermediate nodes traversed. The `edge_type` is the maximum
/// (most specialized) type along the path.
fn build_subgraph_with_transitive(
    graph: &LineageGraph,
    keep_nodes: &HashSet<NodeIndex>,
) -> LineageGraph {
    let mut new_graph = LineageGraph::new();
    let mut index_map: HashMap<NodeIndex, NodeIndex> = HashMap::new();

    // Add kept nodes
    for &old_idx in keep_nodes {
        let node = graph[old_idx].clone();
        let new_idx = new_graph.add_node(node);
        index_map.insert(old_idx, new_idx);
    }

    // Add direct edges between kept nodes
    for edge in graph.edge_references() {
        let source = edge.source();
        let target = edge.target();
        if let (Some(&new_source), Some(&new_target)) =
            (index_map.get(&source), index_map.get(&target))
        {
            new_graph.add_edge(new_source, new_target, edge.weight().clone());
        }
    }

    // Add transitive edges through removed nodes
    for &start in keep_nodes {
        // Pre-fill with directly connected kept nodes to avoid redundant transitive edges
        let mut connected: HashSet<NodeIndex> = HashSet::new();
        for edge in graph.edges_directed(start, Direction::Outgoing) {
            if keep_nodes.contains(&edge.target()) {
                connected.insert(edge.target());
            }
        }

        let mut queue: VecDeque<(NodeIndex, usize, EdgeType)> = VecDeque::new();
        let mut visited: HashSet<NodeIndex> = HashSet::new();

        // Seed: outgoing edges from start to removed nodes
        for edge in graph.edges_directed(start, Direction::Outgoing) {
            let neighbor = edge.target();
            if !keep_nodes.contains(&neighbor) && visited.insert(neighbor) {
                queue.push_back((neighbor, 1, edge.weight().edge_type));
            }
        }

        // BFS through removed nodes
        while let Some((current, depth, max_edge_type)) = queue.pop_front() {
            for edge in graph.edges_directed(current, Direction::Outgoing) {
                let neighbor = edge.target();
                let new_max = max_edge_type.max(edge.weight().edge_type);

                if keep_nodes.contains(&neighbor) {
                    // Found a kept node — add transitive edge if not already connected
                    if connected.insert(neighbor) {
                        let new_source = index_map[&start];
                        let new_target = index_map[&neighbor];
                        new_graph.add_edge(
                            new_source,
                            new_target,
                            EdgeData::transitive(new_max, depth),
                        );
                    }
                } else if visited.insert(neighbor) {
                    queue.push_back((neighbor, depth + 1, new_max));
                }
            }
        }
    }

    new_graph
}

/// Known node type labels for validation.
pub const KNOWN_NODE_TYPE_LABELS: &[&str] =
    &["model", "source", "seed", "snapshot", "test", "exposure"];

/// Resolve the effective node type names from CLI arguments.
///
/// Returns `explicit` if provided, otherwise all known node types.
pub fn resolve_node_types(explicit: Option<Vec<String>>) -> Vec<String> {
    explicit.unwrap_or_else(|| {
        KNOWN_NODE_TYPE_LABELS
            .iter()
            .map(|s| s.to_string())
            .collect()
    })
}

/// Return unrecognized node type names from the given list.
pub fn validate_node_type_names(type_names: &[String]) -> Vec<String> {
    type_names
        .iter()
        .filter(|t| {
            !KNOWN_NODE_TYPE_LABELS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(t))
        })
        .cloned()
        .collect()
}

/// Filter graph to keep only nodes whose type label matches one of the given names.
/// If `type_names` is empty, the graph is returned unchanged.
/// Comparison is case-insensitive.
///
/// When `transitive` is true, edges through filtered-out intermediate nodes are
/// preserved as transitive edges with `collapsed_through` metadata.
/// When false, such edges are dropped (legacy behavior).
pub fn filter_output_node_types(
    graph: &LineageGraph,
    type_names: &[String],
    transitive: bool,
) -> LineageGraph {
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
    if transitive {
        build_subgraph_with_transitive(graph, &keep)
    } else {
        build_subgraph(graph, &keep)
    }
}

/// Collapse intermediate nodes, keeping only endpoints.
///
/// An "endpoint" is a node that has no predecessors or no successors in the graph.
/// When `group_by` is provided, endpoints are determined per group: a node is an
/// endpoint if it has no same-group predecessors, no same-group successors, or
/// connects to at least one node outside its group.
///
/// Removed intermediate nodes become transitive edges via [`build_subgraph_with_transitive`].
pub fn collapse_intermediate(
    graph: &LineageGraph,
    group_by: Option<crate::cli::GroupBy>,
) -> LineageGraph {
    let keep = match group_by {
        None => {
            // Global endpoints: in-degree=0 or out-degree=0
            graph
                .node_indices()
                .filter(|&idx| {
                    graph
                        .neighbors_directed(idx, Direction::Incoming)
                        .next()
                        .is_none()
                        || graph
                            .neighbors_directed(idx, Direction::Outgoing)
                            .next()
                            .is_none()
                })
                .collect::<HashSet<_>>()
        }
        Some(group_by) => {
            // Compute group key for each node
            let group_of: HashMap<NodeIndex, String> = graph
                .node_indices()
                .map(|idx| {
                    let node = &graph[idx];
                    let key = match group_by {
                        crate::cli::GroupBy::NodeType => node.node_type.label().to_string(),
                        crate::cli::GroupBy::Directory => crate::render::directory_label(node),
                    };
                    (idx, key)
                })
                .collect();

            graph
                .node_indices()
                .filter(|&idx| {
                    let my_group = &group_of[&idx];

                    // Single pass over incoming neighbors
                    let (has_external_pred, has_group_pred) = graph
                        .neighbors_directed(idx, Direction::Incoming)
                        .fold((false, false), |(ext, grp), n| {
                            if group_of.get(&n) != Some(my_group) {
                                (true, grp)
                            } else {
                                (ext, true)
                            }
                        });

                    // Single pass over outgoing neighbors
                    let (has_external_succ, has_group_succ) = graph
                        .neighbors_directed(idx, Direction::Outgoing)
                        .fold((false, false), |(ext, grp), n| {
                            if group_of.get(&n) != Some(my_group) {
                                (true, grp)
                            } else {
                                (ext, true)
                            }
                        });

                    // Keep if: endpoint within group, or connects to outside
                    !has_group_pred || !has_group_succ || has_external_pred || has_external_succ
                })
                .collect::<HashSet<_>>()
        }
    };

    build_subgraph_with_transitive(graph, &keep)
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
            exposure: None,
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
        let filtered =
            filter_output_node_types(&filtered, &["model".into(), "source".into()], false);
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

    // -- Selector parsing tests -----------------------------------------------

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

        g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
        g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
        g.add_edge(c, d, EdgeData::direct(EdgeType::Exposure));
        g
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

    // -- Collapse intermediate tests -------------------------------------------

    #[test]
    fn test_collapse_global_endpoints() {
        // A(source) -> B(model) -> C(model) -> D(exposure)
        // Global endpoints: A (in-degree=0), D (out-degree=0)
        // B and C are intermediate
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

        let collapsed = collapse_intermediate(&g, None);
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
    fn test_collapse_global_fan_out() {
        // A -> B -> C, A -> B -> D
        // A: in-degree=0 (endpoint), B: intermediate, C,D: out-degree=0 (endpoints)
        let mut g = LineageGraph::new();
        let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
        let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
        let c = g.add_node(make_node("model.c", "c", NodeType::Model, None, vec![]));
        let d = g.add_node(make_node("model.d", "d", NodeType::Model, None, vec![]));
        g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));
        g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
        g.add_edge(b, d, EdgeData::direct(EdgeType::Ref));

        let collapsed = collapse_intermediate(&g, None);
        assert_eq!(collapsed.node_count(), 3); // a, c, d
        assert_eq!(collapsed.edge_count(), 2); // a->c, a->d (via 1)

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
    fn test_collapse_all_endpoints_keeps_all() {
        // A -> B (both are endpoints: A has in-degree=0, B has out-degree=0)
        let mut g = LineageGraph::new();
        let a = g.add_node(make_node("model.a", "a", NodeType::Model, None, vec![]));
        let b = g.add_node(make_node("model.b", "b", NodeType::Model, None, vec![]));
        g.add_edge(a, b, EdgeData::direct(EdgeType::Ref));

        let collapsed = collapse_intermediate(&g, None);
        assert_eq!(collapsed.node_count(), 2);
        assert_eq!(collapsed.edge_count(), 1);
    }

    #[test]
    fn test_collapse_by_node_type() {
        // source -> model_a -> model_b -> model_c -> exposure
        // With group-by=node-type:
        //   source group: source (no same-type neighbors) -> keep
        //   model group: model_a (entry, external predecessor), model_b (internal), model_c (exit, external successor)
        //   exposure group: exposure (no same-type neighbors) -> keep
        // model_b is the only one collapsed
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

        let collapsed = collapse_intermediate(&g, Some(crate::cli::GroupBy::NodeType));
        assert_eq!(collapsed.node_count(), 4); // src, a, c, exp
        let labels: HashSet<String> = collapsed
            .node_indices()
            .map(|i| collapsed[i].label.clone())
            .collect();
        assert!(labels.contains("x"));
        assert!(labels.contains("a"));
        assert!(!labels.contains("b")); // intermediate model collapsed
        assert!(labels.contains("c"));
        assert!(labels.contains("dash"));
    }

    #[test]
    fn test_collapse_by_directory() {
        // staging/stg_a -> marts/int_a -> marts/int_b -> marts/final_a
        // With group-by=directory:
        //   staging group: stg_a (has external successor) -> keep
        //   marts group: int_a (entry from external), int_b (internal), final_a (exit, no outgoing)
        // int_b is collapsed
        let mut g = LineageGraph::new();
        let stg = g.add_node(make_node(
            "model.stg_a",
            "stg_a",
            NodeType::Model,
            Some(std::path::PathBuf::from("models/staging/stg_a.sql")),
            vec![],
        ));
        let int_a = g.add_node(make_node(
            "model.int_a",
            "int_a",
            NodeType::Model,
            Some(std::path::PathBuf::from("models/marts/int_a.sql")),
            vec![],
        ));
        let int_b = g.add_node(make_node(
            "model.int_b",
            "int_b",
            NodeType::Model,
            Some(std::path::PathBuf::from("models/marts/int_b.sql")),
            vec![],
        ));
        let final_a = g.add_node(make_node(
            "model.final_a",
            "final_a",
            NodeType::Model,
            Some(std::path::PathBuf::from("models/marts/final_a.sql")),
            vec![],
        ));
        g.add_edge(stg, int_a, EdgeData::direct(EdgeType::Ref));
        g.add_edge(int_a, int_b, EdgeData::direct(EdgeType::Ref));
        g.add_edge(int_b, final_a, EdgeData::direct(EdgeType::Ref));

        let collapsed = collapse_intermediate(&g, Some(crate::cli::GroupBy::Directory));
        assert_eq!(collapsed.node_count(), 3); // stg_a, int_a, final_a
        let labels: HashSet<String> = collapsed
            .node_indices()
            .map(|i| collapsed[i].label.clone())
            .collect();
        assert!(labels.contains("stg_a"));
        assert!(labels.contains("int_a"));
        assert!(!labels.contains("int_b")); // collapsed
        assert!(labels.contains("final_a"));
    }

    #[test]
    fn test_collapse_by_node_type_mixed_edges_retained() {
        // Node with both same-group and cross-group neighbors should be retained.
        // source -> model_a -> model_b -> model_c -> exposure
        //                  \-> model_d (leaf)
        // model_b has: same-group pred (model_a), same-group succ (model_c),
        //              no external edges → purely internal → collapsed
        // model_a has: external pred (source) → retained
        // model_c has: external succ (exposure) → retained
        // model_d has: no same-group succ → retained (group endpoint)
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

        let collapsed = collapse_intermediate(&g, Some(crate::cli::GroupBy::NodeType));
        let labels: HashSet<String> = collapsed
            .node_indices()
            .map(|i| collapsed[i].label.clone())
            .collect();
        // model_a: external predecessor (source) → kept
        assert!(
            labels.contains("a"),
            "model_a has external pred, should be kept"
        );
        // model_b: purely internal (only model neighbors, both in and out) → collapsed
        assert!(
            !labels.contains("b"),
            "model_b is purely internal, should be collapsed"
        );
        // model_c: external successor (exposure) → kept
        assert!(
            labels.contains("c"),
            "model_c has external succ, should be kept"
        );
        // model_d: no same-group successor → kept (group endpoint)
        assert!(
            labels.contains("d"),
            "model_d is a group endpoint, should be kept"
        );
        // source and exposure always kept
        assert!(labels.contains("x"));
        assert!(labels.contains("dash"));
        assert_eq!(collapsed.node_count(), 5);
    }

    #[test]
    fn test_collapse_snapshot_global() {
        // Snapshot test for mermaid rendering of global collapse
        let mut g = LineageGraph::new();
        let a = g.add_node(make_node(
            "source.raw.orders",
            "orders",
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
            "model.int_orders",
            "int_orders",
            NodeType::Model,
            None,
            vec![],
        ));
        let d = g.add_node(make_node(
            "model.orders",
            "orders",
            NodeType::Model,
            None,
            vec![],
        ));
        g.add_edge(a, b, EdgeData::direct(EdgeType::Source));
        g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
        g.add_edge(c, d, EdgeData::direct(EdgeType::Ref));

        let collapsed = collapse_intermediate(&g, None);
        insta::assert_snapshot!(render_mermaid(&collapsed));
    }

    #[test]
    fn test_collapse_snapshot_by_node_type() {
        // Snapshot test: source -> stg -> int -> final -> exposure
        // With node-type grouping: stg and final are boundary, int is collapsed
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

        let collapsed = collapse_intermediate(&g, Some(crate::cli::GroupBy::NodeType));
        insta::assert_snapshot!(render_mermaid(&collapsed));
    }

    fn render_mermaid(graph: &LineageGraph) -> String {
        let mut buf = Vec::new();
        crate::render::mermaid::render_mermaid_to_writer(
            graph,
            &mut buf,
            None,
            crate::cli::Direction::LR,
        )
        .unwrap();
        String::from_utf8(buf).unwrap()
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
}
