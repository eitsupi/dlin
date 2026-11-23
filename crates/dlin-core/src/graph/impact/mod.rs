use std::collections::{HashMap, HashSet, VecDeque};

use path_slash::PathExt as _;
use petgraph::Direction;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use serde::Serialize;

use super::builder::GraphBuild;
use super::dependency::DependencyAnalysis;
use super::types::*;

mod exploration;

pub use exploration::{
    DependencyPolicy, Exploration, ExplorationGap, ExplorationKind, ExplorationStatus,
    NamespaceMatch, ReasonRule,
};

/// Severity level of impact
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ImpactSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl ImpactSeverity {
    pub fn label(&self) -> &'static str {
        match self {
            ImpactSeverity::Low => "low",
            ImpactSeverity::Medium => "medium",
            ImpactSeverity::High => "high",
            ImpactSeverity::Critical => "critical",
        }
    }
}

/// Path from source model to an exposure
#[derive(Debug, Clone, Serialize)]
pub struct ExposurePath {
    #[serde(skip)]
    pub exposure_id: String,
    pub exposure: String,
    pub path: Vec<String>,
}

/// A single impacted node with its severity
#[derive(Debug, Clone, Serialize)]
pub struct ImpactedNode {
    pub unique_id: String,
    pub label: String,
    pub node_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    pub severity: ImpactSeverity,
    /// Number of edges from the source model (also known as "degree" in dbt terminology)
    pub distance: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sql_content: Option<String>,
    /// How completely the node's own dependencies were analysed.
    pub dependency_analysis: DependencyAnalysis,
}

/// The node an impact search started from.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactSource {
    pub unique_id: String,
    pub dependency_analysis: DependencyAnalysis,
}

/// Full impact analysis report
#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub source_model: String,
    node_types: Vec<&'static str>,
    pub overall_severity: Option<ImpactSeverity>,
    pub affected_models: Option<usize>,
    pub affected_tests: Option<usize>,
    pub affected_exposures: Option<usize>,
    pub exposure_paths: Vec<ExposurePath>,
    /// True if any exposure had more paths than the enumeration cap
    pub exposure_paths_truncated: bool,
    /// The selected nodes reached from the source through the edges of the graph.
    /// The set may contain nodes reached through an edge that is only a
    /// guess; `exploration` states whether it can miss a node.
    ///
    /// This field, `source_node` and `exploration` are what the judgements
    /// rely on, so they are read-only: a copy reordered or cut for display
    /// cannot turn into a proof.
    impacted_nodes: Vec<ImpactedNode>,
    source_node: ImpactSource,
    /// Whether the search may have missed a node, per dependency kind.
    /// Empty when the source is not a node the search can start from, such
    /// as a phantom node standing for an unresolved reference. Such a report
    /// still lists the nodes reached through the phantom's edges (the nodes
    /// that reference it), but claims nothing about them: neither
    /// [`ImpactReport::proves_no_impact`] nor
    /// [`ImpactReport::is_acceptable_under`] holds for it.
    exploration: Vec<Exploration>,
}

/// Every field of an [`ImpactReport`], for building one in tests.
#[cfg(test)]
pub(crate) struct ImpactReportParts {
    pub source_model: String,
    pub overall_severity: Option<ImpactSeverity>,
    pub affected_models: Option<usize>,
    pub affected_tests: Option<usize>,
    pub affected_exposures: Option<usize>,
    pub exposure_paths: Vec<ExposurePath>,
    pub exposure_paths_truncated: bool,
    pub impacted_nodes: Vec<ImpactedNode>,
    pub source_node: ImpactSource,
    pub exploration: Vec<Exploration>,
}

#[cfg(test)]
impl From<ImpactReportParts> for ImpactReport {
    fn from(parts: ImpactReportParts) -> Self {
        Self {
            source_model: parts.source_model,
            node_types: NodeType::RESOURCE_TYPES
                .iter()
                .map(NodeType::label)
                .collect(),
            overall_severity: parts.overall_severity,
            affected_models: parts.affected_models,
            affected_tests: parts.affected_tests,
            affected_exposures: parts.affected_exposures,
            exposure_paths: parts.exposure_paths,
            exposure_paths_truncated: parts.exposure_paths_truncated,
            impacted_nodes: parts.impacted_nodes,
            source_node: parts.source_node,
            exploration: parts.exploration,
        }
    }
}

impl ImpactReport {
    /// The resource types covered by the result and its exploration.
    pub fn node_types(&self) -> &[&'static str] {
        &self.node_types
    }

    /// The nodes reached from the source, most severe first.
    pub fn impacted_nodes(&self) -> &[ImpactedNode] {
        &self.impacted_nodes
    }

    /// The node the search started from.
    pub fn source_node(&self) -> &ImpactSource {
        &self.source_node
    }

    /// Whether the search may have missed a node, per dependency kind.
    pub fn exploration(&self) -> &[Exploration] {
        &self.exploration
    }

    #[cfg(test)]
    pub(crate) fn set_exploration(&mut self, exploration: Vec<Exploration>) {
        self.exploration = exploration;
    }

    /// Whether the report proves that the source affects no selected node: nothing
    /// was reached and the model dependency exploration is complete.
    ///
    /// False when the search did not run, as for a phantom source. Accepting
    /// gaps with [`ImpactReport::is_acceptable_under`] does not change this.
    pub fn proves_no_impact(&self) -> bool {
        self.impacted_nodes.is_empty() && self.model_explorations_satisfy(|_| false)
    }

    /// Whether every gap of the model dependency exploration is accepted by
    /// `policy`. A complete exploration is acceptable under any policy.
    ///
    /// This judges the exploration only, not the impacted nodes. It is
    /// false when the search did not run.
    pub fn is_acceptable_under(&self, policy: &DependencyPolicy) -> bool {
        self.model_explorations_satisfy(|gap| policy.accepts_gap(gap))
    }

    /// At least one model dependency exploration exists and every gap of
    /// every one of them satisfies `accept`.
    fn model_explorations_satisfy(&self, accept: impl Fn(&ExplorationGap) -> bool) -> bool {
        let mut explorations = self
            .exploration
            .iter()
            .filter(|exploration| exploration.kind() == ExplorationKind::ModelDependency)
            .peekable();
        explorations.peek().is_some()
            && explorations.all(|exploration| exploration.gaps().iter().all(&accept))
    }
}

/// Classify the severity of a single node
pub fn classify_severity(node: &NodeData) -> ImpactSeverity {
    match node.node_type {
        NodeType::Exposure => ImpactSeverity::Critical,
        NodeType::Test => ImpactSeverity::Low,
        NodeType::Model => {
            // Check for mart-like indicators
            let is_mart = node
                .materialization
                .as_deref()
                .is_some_and(|m| m == "table" || m == "incremental")
                || node
                    .file_path
                    .as_ref()
                    .is_some_and(|p| p.to_string_lossy().contains("mart"));

            if is_mart {
                return ImpactSeverity::High;
            }

            ImpactSeverity::Medium
        }
        _ => ImpactSeverity::Medium,
    }
}

/// A nonempty selection of public resource types for an impact search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImpactScope(Vec<NodeType>);

impl ImpactScope {
    /// Select resource types in canonical order. Phantom nodes are internal.
    /// Returns `None` if no public resource types were selected.
    pub fn new(types: impl IntoIterator<Item = NodeType>) -> Option<Self> {
        let selected: HashSet<_> = types.into_iter().collect();
        let types: Vec<_> = NodeType::RESOURCE_TYPES
            .into_iter()
            .filter(|kind| selected.contains(kind))
            .collect();
        (!types.is_empty()).then_some(Self(types))
    }
}

impl Default for ImpactScope {
    fn default() -> Self {
        Self(
            NodeType::RESOURCE_TYPES
                .into_iter()
                .filter(|kind| *kind != NodeType::Test)
                .collect(),
        )
    }
}

/// Compute the impact report of the node at `source_idx`, or `None` when
/// the graph has no such node.
///
/// The build's project gaps, which are not attributed to a node, each make
/// the exploration incomplete.
pub fn compute_impact(
    build: &GraphBuild,
    source_idx: NodeIndex,
    scope: &ImpactScope,
) -> Option<ImpactReport> {
    let node_types = &scope.0;
    let graph = build.graph();
    let project_gaps = build.project_gaps();
    let source_node = graph.node_weight(source_idx)?;
    let source_model = source_node.label.clone();

    // BFS downstream to find all impacted nodes with distances
    let mut visited: HashSet<NodeIndex> = HashSet::new();
    let mut queue: VecDeque<(NodeIndex, usize)> = VecDeque::new();
    visited.insert(source_idx);
    queue.push_back((source_idx, 0));

    let mut impacted_nodes: Vec<ImpactedNode> = Vec::new();
    let mut affected_models = 0usize;
    let mut affected_tests = 0usize;
    let mut affected_exposures = 0usize;
    let mut exposure_indices: Vec<NodeIndex> = Vec::new();

    while let Some((current, distance)) = queue.pop_front() {
        for edge in graph.edges_directed(current, Direction::Outgoing) {
            let neighbor = edge.target();
            if visited.insert(neighbor) {
                let node = &graph[neighbor];
                let next_distance = distance + 1;
                queue.push_back((neighbor, next_distance));
                if !node_types.contains(&node.node_type) {
                    continue;
                }
                let severity = classify_severity(node);

                match node.node_type {
                    NodeType::Model => affected_models += 1,
                    NodeType::Test => affected_tests += 1,
                    NodeType::Exposure => {
                        affected_exposures += 1;
                        exposure_indices.push(neighbor);
                    }
                    _ => {}
                }

                impacted_nodes.push(ImpactedNode {
                    unique_id: node.unique_id.clone(),
                    label: node.label.clone(),
                    node_type: node.node_type.label().to_string(),
                    file_path: node
                        .file_path
                        .as_ref()
                        .map(|p| p.to_slash_lossy().into_owned()),
                    severity,
                    distance: next_distance,
                    sql_content: None,
                    dependency_analysis: node.dependency_analysis.clone(),
                });
            }
        }
    }

    // Find all simple paths to exposures via single DFS (capped per exposure)
    const MAX_PATHS_PER_EXPOSURE: usize = 10;
    let exposure_set: HashSet<NodeIndex> = exposure_indices.iter().copied().collect();
    let mut exposure_paths: Vec<ExposurePath> = Vec::new();
    let mut path_counts: HashMap<NodeIndex, usize> = HashMap::new();

    if !exposure_set.is_empty() {
        let mut stack: Vec<(NodeIndex, Vec<NodeIndex>, HashSet<NodeIndex>)> =
            vec![(source_idx, vec![source_idx], HashSet::from([source_idx]))];
        while let Some((current, path, path_set)) = stack.pop() {
            // Early termination: stop when all exposures have reached the cap
            if path_counts.len() == exposure_set.len()
                && path_counts.values().all(|&c| c >= MAX_PATHS_PER_EXPOSURE)
            {
                break;
            }
            if exposure_set.contains(&current) {
                let count = path_counts.entry(current).or_insert(0);
                if *count < MAX_PATHS_PER_EXPOSURE {
                    *count += 1;
                    exposure_paths.push(ExposurePath {
                        exposure_id: graph[current].unique_id.clone(),
                        exposure: graph[current].label.clone(),
                        path: path.iter().map(|&idx| graph[idx].label.clone()).collect(),
                    });
                }
                // Exposures are leaf nodes in dbt; don't traverse beyond them
                continue;
            }
            for edge in graph.edges_directed(current, Direction::Outgoing) {
                let neighbor = edge.target();
                if !path_set.contains(&neighbor) {
                    let mut new_path = path.clone();
                    new_path.push(neighbor);
                    let mut new_set = path_set.clone();
                    new_set.insert(neighbor);
                    stack.push((neighbor, new_path, new_set));
                }
            }
        }
    }
    let exposure_paths_truncated = path_counts
        .values()
        .any(|&count| count >= MAX_PATHS_PER_EXPOSURE);
    exposure_paths.sort_by(|a, b| a.exposure.cmp(&b.exposure).then(a.path.cmp(&b.path)));

    // Sort by severity (descending), then distance
    impacted_nodes.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(a.distance.cmp(&b.distance))
    });

    let overall_severity = impacted_nodes.iter().map(|n| n.severity).max();

    // A phantom node stands for a reference that resolved to nothing; the
    // search from it is not a search from a node of the project. Its reached
    // nodes are kept as before, without an exploration.
    let exploration = if source_node.node_type == NodeType::Phantom {
        Vec::new()
    } else {
        vec![exploration::explore_model_dependencies(
            graph,
            &visited,
            node_types,
            project_gaps,
        )]
    };

    Some(ImpactReport {
        source_model,
        node_types: node_types.iter().map(NodeType::label).collect(),
        overall_severity,
        affected_models: node_types
            .contains(&NodeType::Model)
            .then_some(affected_models),
        affected_tests: node_types
            .contains(&NodeType::Test)
            .then_some(affected_tests),
        affected_exposures: node_types
            .contains(&NodeType::Exposure)
            .then_some(affected_exposures),
        exposure_paths,
        exposure_paths_truncated,
        impacted_nodes,
        source_node: ImpactSource {
            unique_id: source_node.unique_id.clone(),
            dependency_analysis: source_node.dependency_analysis.clone(),
        },
        exploration,
    })
}

#[cfg(test)]
mod tests;
