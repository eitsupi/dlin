use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use serde::Serialize;

use crate::parser::manifest::Manifest;

use super::relation::{RelationResolution, resolve_unique};
use super::{
    ColumnLineageAnalysis, ColumnLineageError, ColumnLineageErrorKind, InternalModelColumnLineage,
    ModelOutputRef, TransformationType, UnresolvedReason, find_model_by_name,
    normalize_column_lineage_errors,
};

/// How a target depends on an input.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    /// The value of the target is computed from the input.
    Value,
    /// The existence of the target rows depends on the input.
    Row,
}

/// Which dependency kinds an impact query asks for.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ImpactScope {
    ValueOnly,
    ValueAndRow,
}

impl ImpactScope {
    fn kinds(self) -> &'static [DependencyKind] {
        match self {
            Self::ValueOnly => &[DependencyKind::Value],
            Self::ValueAndRow => &[DependencyKind::Value, DependencyKind::Row],
        }
    }
}

/// The input side of an impact edge.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DependencyRef {
    Column {
        unique_id: String,
        model: String,
        column: String,
    },
}

/// One hop of an impact path: `to` depends on `from`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImpactEdge {
    pub from: DependencyRef,
    pub to: ModelOutputRef,
    pub kind: DependencyKind,
    pub transformation: TransformationType,
}

/// A target that depends on the input through known edges.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConfirmedImpact {
    pub target: ModelOutputRef,
    pub kind: DependencyKind,
    /// Transformation that produces `target` from its direct input.
    pub transformation: TransformationType,
    /// Edges from the input column to `target`, in order.
    pub path: Vec<ImpactEdge>,
}

/// Why the search for one dependency kind may have missed targets.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TraceGap {
    /// A dependent output has unresolved contributions that were not proven
    /// unrelated to the input.
    UnresolvedLineage {
        at: ModelOutputRef,
        reasons: Vec<UnresolvedReason>,
    },
    /// A dependent model could not be analysed.
    AnalysisFailed {
        unique_id: String,
        model: String,
        error: ColumnLineageErrorKind,
    },
    /// The lineage of a dependent output could not be established at all.
    UnresolvedColumn { at: ModelOutputRef },
    /// This dependency kind is not collected by the analysis.
    NotCollected { kind: DependencyKind },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExplorationStatus {
    Complete,
    Incomplete { gaps: Vec<TraceGap> },
}

/// Completeness of the search for one dependency kind.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct KindExploration {
    pub kind: DependencyKind,
    #[serde(flatten)]
    pub status: ExplorationStatus,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ColumnImpactReport {
    pub model: String,
    pub column: String,
    pub scope: ImpactScope,
    pub confirmed: Vec<ConfirmedImpact>,
    /// One entry per requested dependency kind.
    pub exploration: Vec<KindExploration>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ColumnLineageError>,
}

impl ColumnImpactReport {
    /// Whether the report supports "no impact": nothing is confirmed and every
    /// requested exploration is complete.
    pub fn proves_no_impact(&self) -> bool {
        self.confirmed.is_empty()
            && self
                .exploration
                .iter()
                .all(|exploration| exploration.status == ExplorationStatus::Complete)
    }

    /// Gaps of every incomplete exploration, in report order.
    pub fn gaps(&self) -> Vec<(DependencyKind, &TraceGap)> {
        let mut gaps = Vec::new();
        for exploration in &self.exploration {
            if let ExplorationStatus::Incomplete { gaps: found } = &exploration.status {
                gaps.extend(found.iter().map(|gap| (exploration.kind, gap)));
            }
        }
        gaps
    }
}

type NodeKey = (String, String);

#[derive(Debug, Clone)]
struct PathNode {
    edge: ImpactEdge,
    parent: Option<Rc<PathNode>>,
}

#[derive(Debug, Clone)]
struct VisitedNode {
    key: NodeKey,
    parent: Option<Rc<VisitedNode>>,
}

type ImpactQueueItem = (String, String, Option<Rc<PathNode>>, Rc<VisitedNode>);

fn visited_contains(mut cursor: Option<&Rc<VisitedNode>>, key: &NodeKey) -> bool {
    while let Some(node) = cursor {
        if &node.key == key {
            return true;
        }
        cursor = node.parent.as_ref();
    }
    false
}

fn materialize_path(mut cursor: Option<&Rc<PathNode>>) -> Vec<ImpactEdge> {
    let mut path = Vec::new();
    while let Some(node) = cursor {
        path.push(node.edge.clone());
        cursor = node.parent.as_ref();
    }
    path.reverse();
    path
}

fn exploration_for(scope: ImpactScope, value_gaps: BTreeSet<TraceGap>) -> Vec<KindExploration> {
    scope
        .kinds()
        .iter()
        .map(|&kind| {
            let gaps: Vec<TraceGap> = match kind {
                DependencyKind::Value => value_gaps.iter().cloned().collect(),
                DependencyKind::Row => vec![TraceGap::NotCollected { kind }],
            };
            KindExploration {
                kind,
                status: if gaps.is_empty() {
                    ExplorationStatus::Complete
                } else {
                    ExplorationStatus::Incomplete { gaps }
                },
            }
        })
        .collect()
}

pub(super) fn compute_column_impact(
    analysis: &mut ColumnLineageAnalysis<'_>,
    model_name: &str,
    column_name: &str,
    scope: ImpactScope,
) -> ColumnImpactReport {
    let manifest = analysis.manifest;
    let dialect = analysis.dialect;
    let initial_node = match super::find_model_by_unique_id(manifest, model_name)
        .or_else(|| find_model_by_name(manifest, model_name))
    {
        Some(n) => n,
        None => {
            let gap = TraceGap::AnalysisFailed {
                unique_id: model_name.to_string(),
                model: model_name.to_string(),
                error: ColumnLineageErrorKind::ModelNotFound,
            };
            return ColumnImpactReport {
                model: model_name.to_string(),
                column: column_name.to_string(),
                scope,
                confirmed: vec![],
                exploration: exploration_for(scope, BTreeSet::from([gap])),
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ModelNotFound,
                    column: None,
                    what: format!("model '{}' not found in manifest", model_name),
                    why: None,
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                }],
            };
        }
    };

    let downstream_map = build_downstream_model_map(manifest);

    let mut confirmed = Vec::new();
    let mut gaps = BTreeSet::new();
    let mut errors = Vec::new();
    let initial_uid = initial_node.unique_id.clone();
    let initial_name = initial_node.name.clone();
    let mut lineage_cache: HashMap<String, InternalModelColumnLineage> = HashMap::new();

    // Each queue entry carries its own path-local set of visited (uid, col) pairs.
    // This allows a node to be processed once per unique path leading to it, while
    // still preventing cycles within any single path.
    let initial_visited = Rc::new(VisitedNode {
        key: (initial_uid.clone(), column_name.to_string()),
        parent: None,
    });
    let mut queue: Vec<ImpactQueueItem> =
        vec![(initial_uid, column_name.to_string(), None, initial_visited)];

    while let Some((source_uid, source_column, current_path, visited_nodes)) = queue.pop() {
        let dependents = match downstream_map.get(&source_uid) {
            Some(deps) => deps,
            None => continue,
        };
        let source_model = manifest
            .nodes
            .get(&source_uid)
            .map_or(initial_name.as_str(), |node| node.name.as_str())
            .to_string();

        for dep_uid in dependents {
            let dep_node = match manifest.nodes.get(dep_uid) {
                Some(n) => n,
                None => continue,
            };
            let dep_name = &dep_node.name;
            let candidates = super::cross_model::build_upstream_model_relations(manifest, dep_uid);
            let candidate_relations = candidates
                .iter()
                .map(|(relation, _)| relation.clone())
                .collect::<Vec<_>>();

            let lineage = lineage_cache
                .entry(dep_uid.clone())
                .or_insert_with(|| super::compute_column_lineage_internal(analysis, dep_uid));

            let mut found_on_path = false;
            let mut path_columns = BTreeSet::new();

            for entry in &lineage.columns {
                let output = ModelOutputRef {
                    unique_id: dep_uid.clone(),
                    model: dep_name.clone(),
                    column: entry.column.clone(),
                };
                let target_key = (dep_uid.clone(), entry.column.clone());

                let references_source = entry.lineage.known().iter().any(|s| {
                    if s.column != source_column {
                        return false;
                    }
                    match resolve_unique(&s.relation, &candidate_relations, dialect) {
                        RelationResolution::Unique(index) => candidates[index].1 == source_uid,
                        RelationResolution::Ambiguous | RelationResolution::NotFound => false,
                    }
                });

                // Every partial output of a dependent is a gap, whether or not
                // it is confirmed: its unresolved part is not proven unrelated
                // to the input, so it may hide paths that are not listed.
                if entry.lineage.is_partial() {
                    let mut reasons: Vec<UnresolvedReason> = entry
                        .lineage
                        .unresolved()
                        .iter()
                        .map(|contribution| contribution.reason.clone())
                        .collect();
                    reasons.sort();
                    reasons.dedup();
                    gaps.insert(TraceGap::UnresolvedLineage {
                        at: output.clone(),
                        reasons,
                    });
                }
                if !references_source || visited_contains(Some(&visited_nodes), &target_key) {
                    continue;
                }

                found_on_path = true;
                path_columns.insert(entry.column.clone());
                let edge = ImpactEdge {
                    from: DependencyRef::Column {
                        unique_id: source_uid.clone(),
                        model: source_model.clone(),
                        column: source_column.clone(),
                    },
                    to: output.clone(),
                    kind: DependencyKind::Value,
                    transformation: entry.transformation.clone(),
                };
                let next_path = Rc::new(PathNode {
                    edge: edge.clone(),
                    parent: current_path.clone(),
                });

                confirmed.push(ConfirmedImpact {
                    target: output,
                    kind: DependencyKind::Value,
                    transformation: entry.transformation.clone(),
                    path: materialize_path(Some(&next_path)),
                });

                let next_visited = Rc::new(VisitedNode {
                    key: target_key,
                    parent: Some(visited_nodes.clone()),
                });
                queue.push((
                    dep_uid.clone(),
                    entry.column.clone(),
                    Some(next_path),
                    next_visited,
                ));
            }

            // Column-scoped diagnostics are per-column: only include them when the model
            // has the diagnosed column confirmed on the impact path. Model-level failures
            // (ParseFailure, NoCompiledCode, ColumnInferenceFailed) mean the model could
            // not be analyzed at all, so they are always propagated — the path through
            // that model may be incomplete and the user needs to know.
            for err in &lineage.errors {
                match err.column_name() {
                    Some(column) => {
                        if !lineage.columns.iter().any(|entry| entry.column == column) {
                            gaps.insert(TraceGap::UnresolvedColumn {
                                at: ModelOutputRef {
                                    unique_id: dep_uid.clone(),
                                    model: dep_name.clone(),
                                    column: column.to_string(),
                                },
                            });
                        }
                    }
                    None => {
                        gaps.insert(TraceGap::AnalysisFailed {
                            unique_id: dep_uid.clone(),
                            model: dep_name.clone(),
                            error: err.kind,
                        });
                    }
                }
                let is_column_level = err.is_column_scoped();
                let is_on_path = err
                    .column_name()
                    .is_some_and(|column| path_columns.contains(column));
                if (!is_column_level || (found_on_path && is_on_path)) && !errors.contains(err) {
                    errors.push(err.clone());
                }
            }
        }
    }

    confirmed.sort();
    confirmed.dedup();

    ColumnImpactReport {
        model: model_name.to_string(),
        column: column_name.to_string(),
        scope,
        confirmed,
        exploration: exploration_for(scope, gaps),
        errors: normalize_column_lineage_errors(errors),
    }
}

pub(super) fn build_downstream_model_map(manifest: &Manifest) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for node in manifest.nodes.values() {
        if node.resource_type != "model" {
            continue;
        }
        for dep_id in &node.depends_on.nodes {
            if let Some(dep_node) = manifest.nodes.get(dep_id)
                && dep_node.resource_type == "model"
            {
                map.entry(dep_node.unique_id.clone())
                    .or_default()
                    .push(node.unique_id.clone());
            }
        }
    }

    map
}
