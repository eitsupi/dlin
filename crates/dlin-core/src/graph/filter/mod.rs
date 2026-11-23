use anyhow::Result;
use petgraph::Direction;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::{EdgeRef, IntoEdgeReferences};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::error::DbtLineageError;

use super::dependency::{DependencyReason, NotAssessedScope};
use super::exploration::{Exploration, ExplorationGap, ExplorationKind};
use super::types::*;

/// Resolve a name with canonical ID, alias, label, and ID suffix precedence.
/// At the first matching stage, a bare name prefers model candidates.
pub fn find_node_by_name(graph: &LineageGraph, name: &str) -> Result<Option<NodeIndex>> {
    let canonical = collect_matches(graph, |node| node.unique_id == name);
    if !canonical.is_empty() {
        return lookup_result(graph, name, canonical, false);
    }

    let aliases = collect_matches(graph, |node| {
        node.aliases
            .iter()
            .flat_map(|alias| alias_spellings(alias))
            .any(|spelling| spelling == name)
    });
    let prefer_model = !name.contains('.');
    if !aliases.is_empty() {
        return lookup_result(graph, name, aliases, prefer_model);
    }

    let labels = collect_matches(graph, |node| node.label == name);
    if !labels.is_empty() {
        return lookup_result(graph, name, labels, prefer_model);
    }

    let suffix = format!(".{name}");
    let suffix_matches = collect_matches(graph, |node| {
        node.node_type != NodeType::Phantom && node.unique_id.ends_with(&suffix)
    });
    lookup_result(graph, name, suffix_matches, prefer_model)
}

fn alias_spellings(alias: &str) -> impl Iterator<Item = &str> {
    std::iter::once(alias).chain(
        alias
            .strip_prefix("model.")
            .filter(|bare| !bare.contains('.')),
    )
}

fn collect_matches(
    graph: &LineageGraph,
    mut predicate: impl FnMut(&NodeData) -> bool,
) -> Vec<NodeIndex> {
    graph
        .node_indices()
        .filter(|&idx| predicate(&graph[idx]))
        .collect()
}

fn lookup_result(
    graph: &LineageGraph,
    name: &str,
    mut matches: Vec<NodeIndex>,
    prefer_model: bool,
) -> Result<Option<NodeIndex>> {
    if prefer_model
        && matches
            .iter()
            .any(|&idx| graph[idx].node_type == NodeType::Model)
    {
        matches.retain(|&idx| graph[idx].node_type == NodeType::Model);
    }

    match matches.as_slice() {
        [] => Ok(None),
        [idx] => Ok(Some(*idx)),
        _ => {
            let mut candidates: Vec<_> = matches
                .iter()
                .map(|&idx| graph[idx].unique_id.clone())
                .collect();
            candidates.sort_unstable();
            Err(DbtLineageError::AmbiguousNodeName {
                name: name.to_string(),
                candidates,
            }
            .into())
        }
    }
}

/// Resolve a required node name, returning an error for a missing or ambiguous name.
pub fn resolve_node_by_name(graph: &LineageGraph, name: &str) -> Result<NodeIndex> {
    find_node_by_name(graph, name)?
        .ok_or_else(|| DbtLineageError::ModelNotFound(name.to_string()).into())
}

/// Resolve a batch of focus names. Warn for missing names and reject any ambiguity.
pub fn resolve_focus_nodes(graph: &LineageGraph, names: &[String]) -> Result<Vec<NodeIndex>> {
    let mut indices = Vec::new();
    for name in names {
        if let Some(idx) = find_node_by_name(graph, name)? {
            indices.push(idx);
        } else {
            crate::warn!(
                "{}, skipping.",
                DbtLineageError::ModelNotFound(name.clone())
            );
        }
    }
    if indices.is_empty() && !names.is_empty() {
        return Err(DbtLineageError::ModelNotFound(names.join(", ")).into());
    }
    Ok(indices)
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
                || node
                    .aliases
                    .iter()
                    .flat_map(|alias| alias_spellings(alias))
                    .any(|spelling| pat.matches(spelling))
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
    let focus_nodes = resolve_focus_nodes(graph, focus_models)?;
    filter_graph_by_indices(
        graph,
        &focus_nodes,
        upstream,
        downstream,
        selectors,
        transitive,
    )
    .map(|(graph, _)| graph)
}

/// Filter a graph using focus indices already resolved in that graph.
pub fn filter_graph_by_indices(
    graph: &LineageGraph,
    focus_nodes: &[NodeIndex],
    upstream: Option<usize>,
    downstream: Option<usize>,
    selectors: &[Selector],
    transitive: bool,
) -> Result<(LineageGraph, Option<Exploration>)> {
    // Check for cycles
    if petgraph::algo::is_cyclic_directed(graph) {
        let cycle = find_directed_cycle(graph)
            .expect("petgraph reported a cycle but cycle path extraction found none");
        return Err(
            anyhow::Error::new(DbtLineageError::CycleDetected).context(format!(
                "cycle detected in lineage graph: {}",
                cycle.join(" -> ")
            )),
        );
    }

    let mut keep_nodes = HashSet::new();
    let mut gaps = Vec::new();

    if focus_nodes.is_empty() {
        keep_nodes.extend(graph.node_indices());
    } else {
        bfs_collect(
            graph,
            focus_nodes,
            Direction::Incoming,
            upstream,
            |idx, depth| {
                keep_nodes.insert(idx);
                if upstream != Some(depth) {
                    gaps.extend(upstream_gap(&graph[idx], depth));
                }
            },
        );
        bfs_collect(
            graph,
            focus_nodes,
            Direction::Outgoing,
            downstream,
            |idx, _| {
                keep_nodes.insert(idx);
            },
        );
    }

    // Apply selector filter: intersect with BFS results (or use as base set)
    if !selectors.is_empty() {
        let selector_matches = apply_selectors(graph, selectors);
        if !focus_nodes.is_empty() {
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

    let filtered = if transitive {
        build_subgraph_with_transitive(graph, &keep_nodes)
    } else {
        build_subgraph(graph, &keep_nodes)
    };
    Ok((filtered, upstream_exploration(gaps)))
}

fn upstream_gap(node: &NodeData, depth: usize) -> Option<ExplorationGap> {
    let reasons: Vec<_> = node
        .dependency_analysis
        .unresolved()
        .iter()
        .filter(|reason| {
            !matches!(
                reason,
                DependencyReason::NotAssessed {
                    scope: NotAssessedScope::ManifestDependencies
                        | NotAssessedScope::PhantomDependencies,
                }
            )
        })
        .cloned()
        .collect();
    (!reasons.is_empty()).then(|| ExplorationGap::UnresolvedDependencies {
        unique_id: node.unique_id.clone(),
        depth: Some(depth),
        reasons,
    })
}

fn upstream_exploration(gaps: Vec<ExplorationGap>) -> Option<Exploration> {
    (!gaps.is_empty()).then(|| Exploration::new(ExplorationKind::UpstreamModelDependency, gaps))
}

/// Find one real directed cycle, with its first node repeated at the end.
///
/// This runs only after the linear-time acyclic check has found a cycle. The
/// iterative DFS avoids recursion depth limits on large lineage graphs, and
/// sorted roots/neighbors make the reported path deterministic.
fn find_directed_cycle(graph: &LineageGraph) -> Option<Vec<String>> {
    struct Frame {
        node: NodeIndex,
        neighbors: Vec<NodeIndex>,
        next_neighbor: usize,
    }

    let mut roots: Vec<_> = graph.node_indices().collect();
    roots.sort_by(|left, right| {
        graph[*left]
            .unique_id
            .cmp(&graph[*right].unique_id)
            .then_with(|| left.index().cmp(&right.index()))
    });

    let mut color = HashMap::<NodeIndex, u8>::new();
    let mut parent = HashMap::<NodeIndex, NodeIndex>::new();

    for root in roots {
        if color.get(&root).copied().unwrap_or(0) != 0 {
            continue;
        }
        color.insert(root, 1);
        let mut stack = vec![Frame {
            node: root,
            neighbors: sorted_successors(graph, root),
            next_neighbor: 0,
        }];

        while let Some(frame) = stack.last_mut() {
            if frame.next_neighbor == frame.neighbors.len() {
                color.insert(frame.node, 2);
                stack.pop();
                continue;
            }

            let node = frame.node;
            let next = frame.neighbors[frame.next_neighbor];
            frame.next_neighbor += 1;
            match color.get(&next).copied().unwrap_or(0) {
                0 => {
                    parent.insert(next, node);
                    color.insert(next, 1);
                    stack.push(Frame {
                        node: next,
                        neighbors: sorted_successors(graph, next),
                        next_neighbor: 0,
                    });
                }
                1 => {
                    let mut cycle = vec![node];
                    let mut cursor = node;
                    while cursor != next {
                        cursor = *parent.get(&cursor)?;
                        cycle.push(cursor);
                    }
                    cycle.reverse();
                    cycle.push(next);
                    return Some(
                        cycle
                            .into_iter()
                            .map(|index| graph[index].unique_id.clone())
                            .collect(),
                    );
                }
                _ => {}
            }
        }
    }

    None
}

fn sorted_successors(graph: &LineageGraph, node: NodeIndex) -> Vec<NodeIndex> {
    let mut successors: Vec<_> = graph
        .neighbors_directed(node, Direction::Outgoing)
        .collect();
    successors.sort_by(|left, right| {
        graph[*left]
            .unique_id
            .cmp(&graph[*right].unique_id)
            .then_with(|| left.index().cmp(&right.index()))
    });
    successors
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
pub const KNOWN_NODE_TYPE_LABELS: &[&str] = &{
    let types = NodeType::RESOURCE_TYPES;
    let mut labels = [""; NodeType::RESOURCE_TYPES.len()];
    let mut i = 0;
    while i < types.len() {
        labels[i] = types[i].label();
        i += 1;
    }
    labels
};

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
        .filter(|t| NodeType::from_resource_label(t).is_none())
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
    starts: &[NodeIndex],
    direction: Direction,
    max_depth: Option<usize>,
    mut visit: impl FnMut(NodeIndex, usize),
) {
    let mut visited = HashSet::new();
    let mut queue: VecDeque<_> = starts
        .iter()
        .copied()
        .filter(|&idx| visited.insert(idx))
        .map(|idx| (idx, 0))
        .collect();

    while let Some((node, depth)) = queue.pop_front() {
        visit(node, depth);
        if max_depth == Some(depth) {
            continue;
        }
        for e in graph.edges_directed(node, direction) {
            let neighbor = match direction {
                Direction::Incoming => e.source(),
                Direction::Outgoing => e.target(),
            };
            if visited.insert(neighbor) {
                queue.push_back((neighbor, depth + 1));
            }
        }
    }
}

#[cfg(test)]
mod tests;
