//! How completely an impact search explored the model dependencies, and the
//! policy a caller can use to accept some of the gaps.
//!
//! The exploration of an impact report states whether the set of nodes the
//! search reached can be missing a node. It does not state that each reached
//! node is affected, nor that distances or paths are exact: a reached node
//! may have been reached through an edge that is only a guess.

use std::collections::{BTreeSet, HashSet};

use petgraph::Direction;
use petgraph::stable_graph::NodeIndex;
use serde::Serialize;

use crate::graph::dependency::{
    DependencyReason, DependencyReasonCode, InputFailure, InputKind, InputLocation, InputStage,
    NodeDependencyReason, ProjectGap, ProjectScope,
};
use crate::graph::types::{LineageGraph, NodeType};

/// The kind of dependency an exploration covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationKind {
    /// Model-level dependencies: `ref()`, `source()`, tests and exposures.
    ModelDependency,
    /// The upstream model dependencies of a focused query.
    UpstreamModelDependency,
}

/// Whether an exploration may have missed a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplorationStatus {
    /// No gap: the reached set misses no node.
    Complete,
    /// At least one gap.
    Incomplete,
}

/// Something that may hide a node from the reached set.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExplorationGap {
    /// A consumer whose unresolved incoming dependencies can hide nodes.
    /// In an upstream query, `depth` is its distance from the closest focus.
    /// In an impact query the consumer is outside the reached set.
    UnresolvedDependencies {
        unique_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        depth: Option<usize>,
        reasons: Vec<NodeDependencyReason>,
    },
    /// An input was skipped while building the graph. It does not belong to
    /// a node, so it can affect any result.
    InputSkipped {
        location: InputLocation,
        kind: InputKind,
        stage: InputStage,
        failure: InputFailure,
    },
    /// The dependencies of a whole scope were not assessed.
    NotAssessed { scope: ProjectScope },
}

impl From<ProjectGap> for ExplorationGap {
    fn from(gap: ProjectGap) -> Self {
        match gap {
            ProjectGap::InputSkipped {
                location,
                kind,
                stage,
                failure,
            } => Self::InputSkipped {
                location,
                kind,
                stage,
                failure,
            },
            ProjectGap::NotAssessed { scope } => Self::NotAssessed { scope },
        }
    }
}

impl ExplorationGap {
    /// The `type` tag.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnresolvedDependencies { .. } => "unresolved_dependencies",
            Self::InputSkipped { .. } => "input_skipped",
            Self::NotAssessed { .. } => "not_assessed",
        }
    }
}

/// The completeness of the search for one dependency kind.
///
/// The status is derived from the gaps, so the two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exploration {
    kind: ExplorationKind,
    gaps: Vec<ExplorationGap>,
}

impl Exploration {
    /// An exploration with the given gaps, deduplicated and sorted.
    pub fn new(kind: ExplorationKind, gaps: impl IntoIterator<Item = ExplorationGap>) -> Self {
        let mut gaps: Vec<_> = gaps.into_iter().collect();
        gaps.sort();
        gaps.dedup();
        Self { kind, gaps }
    }

    pub fn kind(&self) -> ExplorationKind {
        self.kind
    }

    pub fn status(&self) -> ExplorationStatus {
        if self.gaps.is_empty() {
            ExplorationStatus::Complete
        } else {
            ExplorationStatus::Incomplete
        }
    }

    pub fn gaps(&self) -> &[ExplorationGap] {
        &self.gaps
    }
}

impl Serialize for Exploration {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Repr<'a> {
            kind: ExplorationKind,
            status: ExplorationStatus,
            gaps: &'a [ExplorationGap],
        }
        Repr {
            kind: self.kind,
            status: self.status(),
            gaps: &self.gaps,
        }
        .serialize(serializer)
    }
}

/// Explore the model dependencies of the nodes reached from a source.
///
/// `reached` holds the source and every node reached from it. Only nodes
/// outside it can be gaps. On a hidden path to a missed target, the last
/// missing edge ends at a node that is (or has a known path to) that target.
///
/// Every project gap is a gap, since it is not attributed to a node and may
/// affect any of them.
pub(crate) fn explore_model_dependencies(
    graph: &LineageGraph,
    reached: &HashSet<NodeIndex>,
    node_types: &[NodeType],
    project_gaps: &[ProjectGap],
) -> Exploration {
    let mut gaps: Vec<ExplorationGap> = Vec::new();
    let mut scopes: BTreeSet<ProjectScope> = BTreeSet::new();
    let mut pending: Vec<_> = graph
        .node_indices()
        .filter(|idx| !reached.contains(idx) && node_types.contains(&graph[*idx].node_type))
        .collect();
    let mut relevant: HashSet<_> = pending.iter().copied().collect();
    while let Some(idx) = pending.pop() {
        pending.extend(
            graph
                .neighbors_directed(idx, Direction::Incoming)
                .filter(|parent| !reached.contains(parent) && relevant.insert(*parent)),
        );
        let node = &graph[idx];
        let mut reasons = Vec::new();
        for reason in node.dependency_analysis.unresolved() {
            match reason {
                DependencyReason::NotAssessed { scope } => {
                    // Phantom nodes have no project scope; their uncertainty
                    // is in the reasons of the nodes that reference them.
                    scopes.extend(scope.project_scope());
                }
                _ => reasons.push(reason.clone()),
            }
        }
        if !reasons.is_empty() {
            gaps.push(ExplorationGap::UnresolvedDependencies {
                unique_id: node.unique_id.clone(),
                depth: None,
                reasons,
            });
        }
    }
    for gap in project_gaps {
        match gap {
            ProjectGap::NotAssessed { scope } => {
                scopes.insert(*scope);
            }
            ProjectGap::InputSkipped { .. } => gaps.push(gap.clone().into()),
        }
    }
    gaps.extend(
        scopes
            .into_iter()
            .map(|scope| ExplorationGap::NotAssessed { scope }),
    );
    Exploration::new(ExplorationKind::ModelDependency, gaps)
}

/// How a reason rule matches the namespace a reason names (see
/// [`DependencyReason::namespace`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NamespaceMatch {
    /// Any namespace, or none.
    Any,
    /// Only reasons that name no namespace, including the reason types that
    /// have no namespace field.
    Unspecified,
    /// Only reasons naming exactly this namespace.
    Exact(String),
}

impl NamespaceMatch {
    fn matches(&self, namespace: Option<&str>) -> bool {
        match self {
            Self::Any => true,
            Self::Unspecified => namespace.is_none(),
            Self::Exact(expected) => namespace == Some(expected.as_str()),
        }
    }
}

/// A rule accepting the reasons of one type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReasonRule {
    pub code: DependencyReasonCode,
    pub namespace: NamespaceMatch,
}

impl ReasonRule {
    fn matches(&self, reason: &NodeDependencyReason) -> bool {
        reason.reason_code() == self.code && self.namespace.matches(reason.namespace())
    }
}

/// Which gaps a caller accepts. The default accepts none.
///
/// Accepting a gap is a decision to proceed despite the risk it describes.
/// It does not make an exploration complete, and it does not prove that a
/// change has no impact.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyPolicy {
    reasons: Vec<ReasonRule>,
    allow_input_skipped: bool,
    not_assessed: BTreeSet<ProjectScope>,
}

impl DependencyPolicy {
    /// A policy accepting no gap.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Accept the reasons of type `code` whose namespace matches.
    pub fn allow_reason(mut self, code: DependencyReasonCode, namespace: NamespaceMatch) -> Self {
        self.reasons.push(ReasonRule { code, namespace });
        self
    }

    /// Accept inputs skipped while building the graph.
    pub fn allow_input_skipped(mut self) -> Self {
        self.allow_input_skipped = true;
        self
    }

    /// Accept a scope whose dependencies were not assessed.
    pub fn allow_not_assessed(mut self, scope: ProjectScope) -> Self {
        self.not_assessed.insert(scope);
        self
    }

    /// Whether some rule accepts `reason`.
    pub fn accepts_reason(&self, reason: &NodeDependencyReason) -> bool {
        self.reasons.iter().any(|rule| rule.matches(reason))
    }

    /// Whether the policy accepts `gap`. An `unresolved_dependencies` gap is
    /// accepted only when it has reasons and every one of them is accepted.
    pub fn accepts_gap(&self, gap: &ExplorationGap) -> bool {
        match gap {
            ExplorationGap::UnresolvedDependencies { reasons, .. } => {
                !reasons.is_empty() && reasons.iter().all(|reason| self.accepts_reason(reason))
            }
            ExplorationGap::InputSkipped { .. } => self.allow_input_skipped,
            ExplorationGap::NotAssessed { scope } => self.not_assessed.contains(scope),
        }
    }
}
