use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use serde::Serialize;

use crate::parser::manifest::Manifest;

use super::relation::{RelationResolution, resolve_unique};
use super::{
    ColumnLineageAnalysis, ColumnLineageError, ColumnLineageErrorKind, Ident,
    InternalModelColumnLineage, ModelOutputRef, OutputShape, OutputSlot, OutputSlotId, SlotLookup,
    TransformationType, UnexpandedStar, UnresolvedReason, find_model_by_name,
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
    /// An output slot of a model.
    Column(ModelOutputRef),
    /// A column that is only known by its name, because it does not identify
    /// one output of its model: the model has no output, the name is not
    /// among its listed outputs, or several outputs share it. The search from
    /// such an input matches the sources of dependents by name.
    ///
    /// A confirmed impact whose path starts here means that the dependent
    /// references this column name of the start model. It does not establish
    /// a dependency on one particular slot of that model.
    UnidentifiedColumn {
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
    /// A dependent model has unexpanded stars, so columns that are not
    /// listed may depend on the input. Searched conservatively: the model is
    /// a gap whether or not the stars read the input.
    OpenOutput {
        unique_id: String,
        model: String,
        unexpanded: Vec<UnexpandedStar>,
    },
    /// A dependent output reads a column of the input model whose name
    /// matches several outputs of that model, one of which is the input. It
    /// is not known which one the output reads.
    AmbiguousInput {
        at: ModelOutputRef,
        candidates: Vec<ModelOutputRef>,
    },
    /// The requested input column does not identify one output of its model:
    /// no listed output has that name (or the model could not be analysed),
    /// or several do (`candidates`). The dependents are still searched by the
    /// name they give the column, but the exploration stays incomplete.
    UnidentifiedInput {
        unique_id: String,
        model: String,
        column: String,
        candidates: Vec<ModelOutputRef>,
    },
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
    /// The output the requested column identified, when it identified one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<ModelOutputRef>,
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

type NodeKey = (String, OutputSlotId);

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

/// What a dependent output is matched against: a slot of the source model, or,
/// for an input that identifies no slot, its name.
#[derive(Debug, Clone)]
enum ImpactInput {
    Slot(OutputSlotId),
    Name(Ident),
}

type ImpactQueueItem = (
    String,
    ImpactInput,
    Option<Rc<PathNode>>,
    Option<Rc<VisitedNode>>,
);

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
                input: None,
                scope,
                confirmed: vec![],
                exploration: exploration_for(scope, BTreeSet::from([gap])),
                errors: vec![ColumnLineageError {
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                    ..ColumnLineageError::model(
                        ColumnLineageErrorKind::ModelNotFound,
                        format!("model '{}' not found in manifest", model_name),
                    )
                }],
            };
        }
    };

    let downstream_map = build_downstream_model_map(manifest);

    let mut confirmed = Vec::new();
    let mut gaps = BTreeSet::new();
    let mut errors = Vec::new();
    let initial_uid = initial_node.unique_id.clone();
    let mut lineage_cache: HashMap<String, InternalModelColumnLineage> = HashMap::new();
    ensure_lineage(analysis, &mut lineage_cache, &initial_uid);

    // The requested column is looked up in the output slots of its model like
    // any other column reference. A name that does not identify exactly one
    // slot is reported instead of being guessed.
    let initial = &lineage_cache[&initial_uid];
    let reference = Ident::parse_reference(column_name);
    let lookup = initial
        .output
        .as_ref()
        .map_or(SlotLookup::Absent, |output| {
            output.lookup(&reference, dialect)
        });
    let start_slot = match &lookup {
        SlotLookup::Unique(id) => initial.output.as_ref().and_then(|output| output.slot(*id)),
        SlotLookup::Ambiguous(_) | SlotLookup::Absent => None,
    };
    // Each queue entry carries its own path-local set of visited (uid, slot)
    // pairs. This allows a node to be processed once per unique path leading to it, while
    // still preventing cycles within any single path.
    let (input, start, initial_visited) = match start_slot {
        Some(start_slot) => (
            Some(slot_ref(&initial_uid, &initial.model, start_slot)),
            ImpactInput::Slot(start_slot.id),
            Some(Rc::new(VisitedNode {
                key: (initial_uid.clone(), start_slot.id),
                parent: None,
            })),
        ),
        None => {
            // The name identifies no slot. That is a gap, but the dependents
            // name the column explicitly, so their sources are still matched
            // by name (dialect key), and the search continues by slot from
            // each match.
            let candidates = match (&lookup, &initial.output) {
                (SlotLookup::Ambiguous(ids), Some(output)) => ids
                    .iter()
                    .filter_map(|id| output.slot(*id))
                    .map(|slot| slot_ref(&initial_uid, &initial.model, slot))
                    .collect(),
                _ => Vec::new(),
            };
            gaps.insert(TraceGap::UnidentifiedInput {
                unique_id: initial_uid.clone(),
                model: initial.model.clone(),
                column: column_name.to_string(),
                candidates,
            });
            match &initial.output {
                Some(output) => errors.extend(output.lookup_error(column_name, &lookup)),
                None => errors.extend(initial.errors.iter().cloned()),
            }
            (None, ImpactInput::Name(reference.clone()), None)
        }
    };
    let mut queue: Vec<ImpactQueueItem> = vec![(initial_uid, start, None, initial_visited)];

    while let Some((source_uid, source_input, current_path, visited_nodes)) = queue.pop() {
        let dependents = match downstream_map.get(&source_uid) {
            Some(deps) => deps,
            None => continue,
        };
        for dep_uid in dependents {
            if !manifest.nodes.contains_key(dep_uid) {
                continue;
            }
            ensure_lineage(analysis, &mut lineage_cache, dep_uid);
            let source_lineage = &lineage_cache[&source_uid];
            let lineage = &lineage_cache[dep_uid];
            let source_ref = match &source_input {
                ImpactInput::Slot(id) => {
                    let Some(source_slot) = source_lineage
                        .output
                        .as_ref()
                        .and_then(|output| output.slot(*id))
                    else {
                        continue;
                    };
                    DependencyRef::Column(slot_ref(&source_uid, &source_lineage.model, source_slot))
                }
                ImpactInput::Name(name) => DependencyRef::UnidentifiedColumn {
                    unique_id: source_uid.clone(),
                    model: source_lineage.model.clone(),
                    column: name.to_string(),
                },
            };
            let dep_name = &lineage.model;
            let candidates = super::cross_model::build_upstream_model_relations(manifest, dep_uid);
            let candidate_relations = candidates
                .iter()
                .map(|(relation, _)| relation.clone())
                .collect::<Vec<_>>();

            let mut found_on_path = false;
            let mut path_slots = BTreeSet::new();

            if let Some(dep_output) = &lineage.output {
                if let OutputShape::Open { unexpanded } = &dep_output.shape {
                    gaps.insert(TraceGap::OpenOutput {
                        unique_id: dep_uid.clone(),
                        model: dep_name.clone(),
                        unexpanded: unexpanded.clone(),
                    });
                }
                let source_columns = source_lineage.output.as_ref();
                for entry in &dep_output.columns {
                    let output = slot_ref(dep_uid, dep_name, entry);
                    let target_key = (dep_uid.clone(), entry.id);

                    let mut references_source = false;
                    for s in entry.lineage.known() {
                        let reads_source = matches!(
                            resolve_unique(&s.relation, &candidate_relations, dialect),
                            RelationResolution::Unique(index) if candidates[index].1 == source_uid
                        );
                        if !reads_source {
                            continue;
                        }
                        let source_slot_id = match &source_input {
                            ImpactInput::Slot(id) => *id,
                            ImpactInput::Name(name) => {
                                let key = s.column.column_key(dialect);
                                references_source |=
                                    key.is_some() && key == name.column_key(dialect);
                                continue;
                            }
                        };
                        match source_columns.map(|columns| columns.lookup(&s.column, dialect)) {
                            Some(SlotLookup::Unique(id)) => {
                                references_source |= id == source_slot_id
                            }
                            Some(SlotLookup::Ambiguous(ids)) if ids.contains(&source_slot_id) => {
                                gaps.insert(TraceGap::AmbiguousInput {
                                    at: output.clone(),
                                    candidates: ids
                                        .iter()
                                        .filter_map(|id| source_columns?.slot(*id))
                                        .map(|slot| {
                                            slot_ref(&source_uid, &source_lineage.model, slot)
                                        })
                                        .collect(),
                                });
                            }
                            Some(SlotLookup::Ambiguous(_) | SlotLookup::Absent) | None => {}
                        }
                    }

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
                    if !references_source || visited_contains(visited_nodes.as_ref(), &target_key) {
                        continue;
                    }

                    found_on_path = true;
                    path_slots.insert(entry.id);
                    let edge = ImpactEdge {
                        from: source_ref.clone(),
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

                    let next_visited = Some(Rc::new(VisitedNode {
                        key: target_key,
                        parent: visited_nodes.clone(),
                    }));
                    queue.push((
                        dep_uid.clone(),
                        ImpactInput::Slot(entry.id),
                        Some(next_path),
                        next_visited,
                    ));
                }
            }

            // Slot-scoped diagnostics are per-output: only include them when the
            // slot is confirmed on the impact path. Model-level failures
            // (ParseFailure, NoCompiledCode) mean the model could not be
            // analyzed at all, so they are always propagated: the path through
            // that model may be incomplete and the user needs to know.
            // Differences between the YAML and the SQL output are about the
            // dependent model alone and are not part of an impact result.
            for err in &lineage.errors {
                if err.is_model_failure() {
                    gaps.insert(TraceGap::AnalysisFailed {
                        unique_id: dep_uid.clone(),
                        model: dep_name.clone(),
                        error: err.kind,
                    });
                    if !errors.contains(err) {
                        errors.push(err.clone());
                    }
                } else if found_on_path
                    && err.slot().is_some_and(|slot| path_slots.contains(&slot))
                    && !errors.contains(err)
                {
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
        input,
        scope,
        confirmed,
        exploration: exploration_for(scope, gaps),
        errors: normalize_column_lineage_errors(errors),
    }
}

fn ensure_lineage(
    analysis: &mut ColumnLineageAnalysis<'_>,
    cache: &mut HashMap<String, InternalModelColumnLineage>,
    unique_id: &str,
) {
    if !cache.contains_key(unique_id) {
        let lineage = super::compute_column_lineage_internal(analysis, unique_id);
        cache.insert(unique_id.to_string(), lineage);
    }
}

fn slot_ref<S>(unique_id: &str, model: &str, slot: &OutputSlot<S>) -> ModelOutputRef {
    ModelOutputRef {
        unique_id: unique_id.to_string(),
        model: model.to_string(),
        slot: slot.id,
        column: slot.name.to_string(),
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
