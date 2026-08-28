use anyhow::Result;
use petgraph::Direction;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::DbtLineageError;

use super::types::*;

/// Result of a node name lookup.
#[derive(Debug)]
enum NodeLookupResult {
    /// No matching node found.
    NotFound,
    /// Exactly one node matched at the highest-priority lookup stage.
    Found(NodeIndex),
    /// Multiple nodes matched at the highest-priority lookup stage. The first
    /// is selected deterministically, and all matching canonical unique_ids
    /// are returned for caller-side warnings.
    Ambiguous(NodeIndex, Vec<String>),
}

/// Find a node by name using the compatibility lookup precedence:
/// canonical ID, alias (including the bare model-name shorthand), display
/// label, then canonical ID suffix. Each stage is evaluated independently and
/// a lower-priority stage is not consulted once a higher stage has matches.
fn find_node_by_name(graph: &LineageGraph, name: &str) -> NodeLookupResult {
    let canonical = collect_matches(graph, |node| node.unique_id == name);
    if !canonical.is_empty() {
        return lookup_result(graph, canonical);
    }

    let name_prefixed = format!("model.{name}");
    let aliases = collect_matches(graph, |node| {
        node.aliases
            .iter()
            .any(|alias| alias == name || (!name.starts_with("model.") && alias == &name_prefixed))
    });
    if !aliases.is_empty() {
        return lookup_result(graph, aliases);
    }

    let labels = collect_matches(graph, |node| node.label == name);
    if !labels.is_empty() {
        return lookup_result(graph, labels);
    }

    let suffix = format!(".{name}");
    let suffix_matches = collect_matches(graph, |node| {
        node.node_type != NodeType::Phantom && node.unique_id.ends_with(&suffix)
    });
    lookup_result(graph, suffix_matches)
}

fn collect_matches(
    graph: &LineageGraph,
    mut predicate: impl FnMut(&NodeData) -> bool,
) -> Vec<NodeIndex> {
    let mut matches: Vec<NodeIndex> = graph
        .node_indices()
        .filter(|&idx| predicate(&graph[idx]))
        .collect();
    matches.sort_by(|left, right| {
        graph[*left]
            .unique_id
            .cmp(&graph[*right].unique_id)
            .then_with(|| left.index().cmp(&right.index()))
    });
    matches
}

fn lookup_result(graph: &LineageGraph, matches: Vec<NodeIndex>) -> NodeLookupResult {
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
/// Warns to stderr when the highest-priority lookup stage matches multiple nodes.
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

/// Silently resolve a node by name, returning `Some(index)` or `None`.
///
/// Unlike [`try_resolve_node`], this does not emit any warnings, making it
/// suitable for best-effort lookups where warnings have already been issued.
pub fn try_resolve_node_quiet(graph: &LineageGraph, name: &str) -> Option<NodeIndex> {
    match find_node_by_name(graph, name) {
        NodeLookupResult::Found(idx) | NodeLookupResult::Ambiguous(idx, _) => Some(idx),
        NodeLookupResult::NotFound => None,
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
        Selector::ModelName(pat) => {
            pat.matches(&node.label)
                || pat.matches(&node.unique_id)
                || node.aliases.iter().any(|a| {
                    pat.matches(a)
                        || a.strip_prefix("model.")
                            .is_some_and(|bare| pat.matches(bare))
                })
        }
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
pub const KNOWN_NODE_TYPE_LABELS: &[&str] = &[
    "model",
    "source",
    "seed",
    "snapshot",
    "test",
    "exposure",
    "semantic_model",
    "metric",
    "saved_query",
];

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

/// Collapse intermediate nodes, keeping only endpoints and explicitly
/// preserved nodes (e.g. focus models from positional CLI arguments).
///
/// In `Endpoints` mode, an "endpoint" is a node with in-degree=0 or
/// out-degree=0 in the graph.
///
/// In `Focal` mode, only Source and Exposure nodes are kept (regardless
/// of topology). This ignores BFS window boundaries that create
/// pseudo-endpoints.
///
/// Removed intermediate nodes become transitive edges via
/// [`build_subgraph_with_transitive`].
///
/// Indices in `preserve` that are out of bounds for `graph` are ignored
/// and cause a warning to be logged; callers must ensure that all indices
/// originate from the same graph.
pub fn collapse_intermediate(
    graph: &LineageGraph,
    mode: crate::CollapseMode,
    preserve: &HashSet<NodeIndex>,
) -> LineageGraph {
    let mut keep: HashSet<_> = match mode {
        crate::CollapseMode::Endpoints => graph
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
            .collect(),
        crate::CollapseMode::Focal => graph
            .node_indices()
            .filter(|&idx| matches!(graph[idx].node_type, NodeType::Source | NodeType::Exposure))
            .collect(),
    };

    // Always keep explicitly specified focus models (warn and skip invalid indices)
    for &idx in preserve {
        if graph.node_weight(idx).is_some() {
            keep.insert(idx);
        } else {
            crate::warn!("preserve index {:?} not found in graph, skipping", idx);
        }
    }

    build_subgraph_with_transitive(graph, &keep)
}

/// Filter graph to nodes matching all of the given regex patterns (AND logic).
///
/// Each pattern is tested against the node's label and description (OR within a single pattern).
/// A node is kept only if every pattern matches at least one of those fields.
/// An empty slice returns the graph unchanged.
pub fn filter_by_search(graph: &LineageGraph, patterns: &[regex::Regex]) -> LineageGraph {
    if patterns.is_empty() {
        return graph.clone();
    }
    let keep: HashSet<NodeIndex> = graph
        .node_indices()
        .filter(|&idx| {
            let node = &graph[idx];
            patterns.iter().all(|re| {
                re.is_match(&node.label)
                    || node.description.as_deref().is_some_and(|d| re.is_match(d))
            })
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
mod tests;
