//! Dependency gaps and the completeness of a lineage exploration.

use serde::Serialize;

use super::dependency::{
    InputFailure, InputKind, InputLocation, InputStage, NodeDependencyReason, ProjectGap,
    ProjectScope,
};

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
