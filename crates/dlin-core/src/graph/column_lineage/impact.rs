use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use serde::Serialize;

use crate::parser::manifest::Manifest;

use super::relation::{RelationRef, matching_indices};
use super::types::{cited_evidence, merge_evidence};
use super::{
    ColumnLineageAnalysis, ColumnLineageError, ColumnLineageErrorKind, EvidenceId, Ident,
    InternalModelColumnLineage, ModelOutputRef, OutputShape, OutputSlot, OutputSlotId, OutputSlots,
    SchemaEvidenceRecord, SlotLookup, TransformationType, UnexpandedStar, UnresolvedCandidate,
    UnresolvedReason, find_model_by_name, model_candidates_by_name,
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
    /// Schema evidence the path relies on: what bound each dependent's
    /// reference to the column it reads.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceId>,
}

/// A target that may depend on the input through a finite ambiguous path.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PossibleImpact {
    pub target: ModelOutputRef,
    pub kind: DependencyKind,
    pub transformation: TransformationType,
    /// Candidate edges from the input to the target, in order.
    pub path: Vec<ImpactEdge>,
    /// Schema evidence the path relies on, including what left each
    /// candidate edge unresolved.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceId>,
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
    /// listed may depend on the input when the relation cannot be proven
    /// unrelated to it.
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
    /// A relation can name several manifest models, including the current
    /// impact frontier.
    AmbiguousModel {
        at: ModelOutputRef,
        candidates: Vec<String>,
    },
    /// An impact path revisited the same output slot.
    Cycle { at: ModelOutputRef },
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
    /// Schema evidence the search relied on to identify the input and to
    /// conclude what each examined dependent does or does not read.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<EvidenceId>,
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
    pub possible: Vec<PossibleImpact>,
    /// One entry per requested dependency kind.
    pub exploration: Vec<KindExploration>,
    /// The schema evidence the report cites, and what it was derived from.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<SchemaEvidenceRecord>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ColumnLineageError>,
}

impl ColumnImpactReport {
    /// Whether the report supports "no impact": no target is confirmed or
    /// possible, and every requested exploration is complete.
    pub fn proves_no_impact(&self) -> bool {
        self.confirmed.is_empty()
            && self.possible.is_empty()
            && self
                .exploration
                .iter()
                .all(|exploration| exploration.status == ExplorationStatus::Complete)
    }

    /// The evidence a "no impact" conclusion relies on: that of every
    /// requested exploration. `None` when the report does not support "no
    /// impact".
    pub fn no_impact_evidence(&self) -> Option<Vec<EvidenceId>> {
        if !self.proves_no_impact() {
            return None;
        }
        let mut evidence = Vec::new();
        for exploration in &self.exploration {
            merge_evidence(&mut evidence, &exploration.evidence);
        }
        Some(evidence)
    }

    /// Every evidence id the impacts and explorations cite.
    fn cited(&self) -> impl Iterator<Item = &EvidenceId> {
        self.confirmed
            .iter()
            .flat_map(|impact| &impact.evidence)
            .chain(self.possible.iter().flat_map(|impact| &impact.evidence))
            .chain(
                self.exploration
                    .iter()
                    .flat_map(|exploration| &exploration.evidence),
            )
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
    /// The evidence the edge relies on.
    evidence: Vec<EvidenceId>,
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
    bool,
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

/// The edges of a path in order, and the evidence of all of them.
fn materialize_path(mut cursor: Option<&Rc<PathNode>>) -> (Vec<ImpactEdge>, Vec<EvidenceId>) {
    let mut path = Vec::new();
    let mut evidence = Vec::new();
    while let Some(node) = cursor {
        path.push(node.edge.clone());
        merge_evidence(&mut evidence, &node.evidence);
        cursor = node.parent.as_ref();
    }
    path.reverse();
    (path, evidence)
}

/// Sort impacts and merge those that differ only in their evidence.
fn normalize_impacts<T: Ord>(
    mut impacts: Vec<T>,
    same: impl Fn(&T, &T) -> bool,
    evidence: impl Fn(&mut T) -> &mut Vec<EvidenceId>,
) -> Vec<T> {
    impacts.sort();
    let mut merged: Vec<T> = Vec::with_capacity(impacts.len());
    for mut impact in impacts {
        match merged.last_mut() {
            Some(last) if same(last, &impact) => {
                let more = std::mem::take(evidence(&mut impact));
                merge_evidence(evidence(last), &more);
            }
            _ => merged.push(impact),
        }
    }
    merged
}

/// The evidence the identification of the input relies on: what decides
/// which columns the model lists and under which names.
fn identification_evidence<S>(output: &OutputSlots<S>) -> Vec<EvidenceId> {
    let mut evidence = output.evidence.clone();
    for slot in &output.columns {
        merge_evidence(&mut evidence, &slot.evidence.structure);
    }
    evidence
}

fn exploration_for(
    scope: ImpactScope,
    value_gaps: BTreeSet<TraceGap>,
    value_evidence: &[EvidenceId],
    row_uncollected: bool,
) -> Vec<KindExploration> {
    scope
        .kinds()
        .iter()
        .map(|&kind| {
            let gaps: Vec<TraceGap> = match kind {
                DependencyKind::Value => value_gaps.iter().cloned().collect(),
                DependencyKind::Row if row_uncollected => {
                    vec![TraceGap::NotCollected { kind }]
                }
                DependencyKind::Row => Vec::new(),
            };
            // Row dependencies are not collected, so no evidence was used
            // for them.
            let evidence = match kind {
                DependencyKind::Value => value_evidence.to_vec(),
                DependencyKind::Row => Vec::new(),
            };
            KindExploration {
                kind,
                status: if gaps.is_empty() {
                    ExplorationStatus::Complete
                } else {
                    ExplorationStatus::Incomplete { gaps }
                },
                evidence,
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
            let ambiguous = model_candidates_by_name(manifest, model_name);
            let (error_kind, message) = if ambiguous.is_empty() {
                (
                    ColumnLineageErrorKind::ModelNotFound,
                    format!("model '{}' not found in manifest", model_name),
                )
            } else {
                let ids = ambiguous
                    .iter()
                    .map(|node| node.unique_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                (
                    ColumnLineageErrorKind::ModelAmbiguous,
                    format!("model name '{model_name}' is ambiguous; use a unique_id: {ids}"),
                )
            };
            let gap = if ambiguous.is_empty() {
                TraceGap::AnalysisFailed {
                    unique_id: model_name.to_string(),
                    model: model_name.to_string(),
                    error: error_kind,
                }
            } else {
                TraceGap::UnidentifiedInput {
                    unique_id: model_name.to_string(),
                    model: model_name.to_string(),
                    column: column_name.to_string(),
                    candidates: Vec::new(),
                }
            };
            return ColumnImpactReport {
                model: model_name.to_string(),
                column: column_name.to_string(),
                input: None,
                scope,
                confirmed: vec![],
                possible: vec![],
                exploration: exploration_for(scope, BTreeSet::from([gap]), &[], true),
                evidence: Vec::new(),
                errors: vec![ColumnLineageError {
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                    ..ColumnLineageError::model(
                        error_kind,
                        message,
                    )
                }],
            };
        }
    };

    let downstream_map = build_downstream_model_map(manifest);
    let row_uncollected = downstream_map
        .get(&initial_node.unique_id)
        .is_some_and(|dependents| !dependents.is_empty());

    let mut confirmed = Vec::new();
    let mut possible = Vec::new();
    let mut gaps = BTreeSet::new();
    // What the value exploration relied on: the identification of the input,
    // and for each examined dependent, its shape and the lineage of each of
    // its outputs as a whole.
    let mut explored = Vec::new();
    let mut errors = Vec::new();
    let initial_uid = initial_node.unique_id.clone();
    let original_input_uid = initial_uid.clone();
    let input_reachable = downstream_model_ids(manifest, &initial_uid);
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
    if let Some(output) = &initial.output {
        merge_evidence(&mut explored, &identification_evidence(output));
    }
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
    let mut queue: Vec<ImpactQueueItem> = vec![(initial_uid, start, None, initial_visited, false)];

    while let Some((source_uid, source_input, current_path, visited_nodes, already_possible)) =
        queue.pop()
    {
        let dependents = match downstream_map.get(&source_uid) {
            Some(deps) => deps,
            None => continue,
        };
        for dep_uid in dependents {
            let Some(dep_node) = manifest.nodes.get(dep_uid) else {
                continue;
            };
            if dep_node.resource_type == "snapshot" {
                let error = if dep_node.compiled_code.is_some() {
                    ColumnLineageErrorKind::UnsupportedMaterialization
                } else {
                    ColumnLineageErrorKind::NoCompiledCode
                };
                gaps.insert(TraceGap::AnalysisFailed {
                    unique_id: dep_uid.clone(),
                    model: dep_node.name.clone(),
                    error,
                });
                continue;
            }
            if dep_node.resource_type != "model" {
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
            let candidates = super::cross_model::upstream_resource_candidates(manifest, dep_uid);
            let candidate_relations = candidates
                .iter()
                .map(|(candidate, _)| candidate.relation.clone())
                .collect::<Vec<_>>();

            let mut found_on_path = false;
            let mut path_slots = BTreeSet::new();

            if let Some(dep_output) = &lineage.output {
                // The shape decides which outputs the dependent lists and
                // whether it has more.
                merge_evidence(&mut explored, &dep_output.evidence);
                if let OutputShape::Open { unexpanded } = &dep_output.shape {
                    let relevant: Vec<_> = unexpanded
                        .iter()
                        .filter(|star| {
                            star.relation.as_ref().is_none_or(|relation| {
                                relation_relevance(
                                    relation,
                                    dep_uid,
                                    &input_reachable,
                                    manifest,
                                    dialect,
                                ) != RelationRelevance::Unrelated
                            })
                        })
                        .cloned()
                        .collect();
                    if !relevant.is_empty() {
                        gaps.insert(TraceGap::OpenOutput {
                            unique_id: dep_uid.clone(),
                            model: dep_name.clone(),
                            unexpanded: relevant,
                        });
                    }
                }
                let source_columns = source_lineage.output.as_ref();
                // Matching a source by name to a slot of the source model
                // relies on what decides which slots it lists, and their names.
                let lookup_evidence = source_columns
                    .map(identification_evidence)
                    .unwrap_or_default();
                for entry in &dep_output.columns {
                    let output = slot_ref(dep_uid, dep_name, entry);
                    let target_key = (dep_uid.clone(), entry.id);
                    // Whether the output reads the input is concluded from its
                    // lineage as a whole.
                    merge_evidence(&mut explored, &entry.evidence.lineage);

                    let mut references_source = false;
                    let mut possible_source = false;
                    // The evidence of the known sources and unresolved parts
                    // that make the edge confirmed, and possible.
                    let mut confirmed_evidence = Vec::new();
                    let mut possible_evidence = Vec::new();
                    for s in entry.lineage.known() {
                        // Whether this source makes the edge confirmed, and
                        // possible, and whether it was matched by slot.
                        let (reads, may_read, by_slot) = 'source: {
                            let relation_matches =
                                matching_indices(&s.relation, &candidate_relations, dialect);
                            let reads_source = relation_matches.iter().any(|index| {
                                candidates[*index].0.kind == super::TraceResourceKind::Model
                                    && candidates[*index].0.unique_id == source_uid
                            });
                            let relation_is_ambiguous = relation_matches.len() > 1;
                            if !reads_source {
                                break 'source (false, false, false);
                            }
                            let model_candidates = relation_is_ambiguous.then(|| {
                                relation_matches
                                    .iter()
                                    .map(|index| candidates[*index].0.unique_id.clone())
                                    .collect::<Vec<_>>()
                            });
                            let source_slot_id = match &source_input {
                                ImpactInput::Slot(id) => *id,
                                ImpactInput::Name(name) => {
                                    let key = s.column.column_key(dialect);
                                    let matches_name =
                                        key.is_some() && key == name.column_key(dialect);
                                    if matches_name && let Some(candidates) = &model_candidates {
                                        gaps.insert(TraceGap::AmbiguousModel {
                                            at: output.clone(),
                                            candidates: candidates.clone(),
                                        });
                                    }
                                    break 'source (
                                        matches_name && !relation_is_ambiguous,
                                        matches_name && relation_is_ambiguous,
                                        false,
                                    );
                                }
                            };
                            match source_columns.map(|columns| columns.lookup(&s.column, dialect)) {
                                Some(SlotLookup::Unique(id)) => {
                                    if id == source_slot_id
                                        && let Some(candidates) = &model_candidates
                                    {
                                        gaps.insert(TraceGap::AmbiguousModel {
                                            at: output.clone(),
                                            candidates: candidates.clone(),
                                        });
                                    }
                                    (
                                        id == source_slot_id && !relation_is_ambiguous,
                                        id == source_slot_id && relation_is_ambiguous,
                                        true,
                                    )
                                }
                                Some(SlotLookup::Ambiguous(ids))
                                    if ids.contains(&source_slot_id) =>
                                {
                                    if relation_is_ambiguous {
                                        if let Some(candidates) = &model_candidates {
                                            gaps.insert(TraceGap::AmbiguousModel {
                                                at: output.clone(),
                                                candidates: candidates.clone(),
                                            });
                                        }
                                    } else {
                                        gaps.insert(TraceGap::AmbiguousInput {
                                            at: output.clone(),
                                            candidates: ids
                                                .iter()
                                                .filter_map(|id| source_columns?.slot(*id))
                                                .map(|slot| {
                                                    slot_ref(
                                                        &source_uid,
                                                        &source_lineage.model,
                                                        slot,
                                                    )
                                                })
                                                .collect(),
                                        });
                                    }
                                    (false, true, true)
                                }
                                Some(SlotLookup::Ambiguous(_) | SlotLookup::Absent) | None => {
                                    (false, false, true)
                                }
                            }
                        };
                        references_source |= reads;
                        possible_source |= may_read;
                        for (matched, evidence) in [
                            (reads, &mut confirmed_evidence),
                            (may_read, &mut possible_evidence),
                        ] {
                            if matched {
                                merge_evidence(evidence, &s.evidence);
                                if by_slot {
                                    merge_evidence(evidence, &lookup_evidence);
                                }
                            }
                        }
                    }

                    // Every partial output of a dependent is a gap, whether or not
                    // it is confirmed: its unresolved part is not proven unrelated
                    // to the input, so it may hide paths that are not listed.
                    if entry.lineage.is_partial() {
                        let input_column = match &source_input {
                            ImpactInput::Name(name) => Some(name.clone()),
                            ImpactInput::Slot(id) => source_lineage
                                .output
                                .as_ref()
                                .and_then(|output| output.slot(*id))
                                .and_then(|slot| slot.name.ident().cloned()),
                        };
                        let unresolved = entry
                            .lineage
                            .unresolved()
                            .iter()
                            .filter(|contribution| {
                                unresolved_relevance(
                                    &contribution.reason,
                                    dep_uid,
                                    (&original_input_uid, &reference),
                                    &input_reachable,
                                    manifest,
                                    dialect,
                                ) != RelationRelevance::Unrelated
                            })
                            .collect::<Vec<_>>();
                        let mut reasons: Vec<UnresolvedReason> = unresolved
                            .iter()
                            .map(|contribution| contribution.reason.clone())
                            .collect();
                        reasons.sort();
                        reasons.dedup();
                        if !reasons.is_empty() {
                            gaps.insert(TraceGap::UnresolvedLineage {
                                at: output.clone(),
                                reasons,
                            });
                            for contribution in &unresolved {
                                if unresolved_may_include_input(
                                    &contribution.reason,
                                    &source_uid,
                                    &source_input,
                                    input_column.as_ref(),
                                    &candidates,
                                    dialect,
                                ) {
                                    possible_source = true;
                                    merge_evidence(&mut possible_evidence, &contribution.evidence);
                                }
                            }
                        }
                    }
                    if !references_source && !possible_source {
                        continue;
                    }
                    if visited_contains(visited_nodes.as_ref(), &target_key) {
                        gaps.insert(TraceGap::Cycle { at: output });
                        continue;
                    }

                    path_slots.insert(entry.id);
                    let edge = ImpactEdge {
                        from: source_ref.clone(),
                        to: output.clone(),
                        kind: DependencyKind::Value,
                        transformation: entry.transformation.clone(),
                    };
                    // A confirmed and a possible edge to the same output each
                    // rely on their own evidence.
                    let confirmed_path = Rc::new(PathNode {
                        edge: edge.clone(),
                        evidence: confirmed_evidence,
                        parent: current_path.clone(),
                    });
                    let possible_path = Rc::new(PathNode {
                        edge,
                        evidence: possible_evidence,
                        parent: current_path.clone(),
                    });
                    let mut push_possible = |node: &Rc<PathNode>| {
                        let (path, evidence) = materialize_path(Some(node));
                        possible.push(PossibleImpact {
                            target: output.clone(),
                            kind: DependencyKind::Value,
                            transformation: entry.transformation.clone(),
                            path,
                            evidence,
                        });
                    };
                    if possible_source {
                        push_possible(&possible_path);
                    }
                    if references_source && already_possible {
                        push_possible(&confirmed_path);
                    }
                    if references_source && !already_possible {
                        found_on_path = true;
                        let (path, evidence) = materialize_path(Some(&confirmed_path));
                        confirmed.push(ConfirmedImpact {
                            target: output.clone(),
                            kind: DependencyKind::Value,
                            transformation: entry.transformation.clone(),
                            path,
                            evidence,
                        });
                    }

                    let next_visited = Some(Rc::new(VisitedNode {
                        key: target_key,
                        parent: visited_nodes.clone(),
                    }));
                    if references_source {
                        queue.push((
                            dep_uid.clone(),
                            ImpactInput::Slot(entry.id),
                            Some(confirmed_path),
                            next_visited.clone(),
                            already_possible,
                        ));
                    }
                    if possible_source {
                        queue.push((
                            dep_uid.clone(),
                            ImpactInput::Slot(entry.id),
                            Some(possible_path),
                            next_visited,
                            true,
                        ));
                    }
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

    let confirmed = normalize_impacts(
        confirmed,
        |a, b| {
            (&a.target, a.kind, &a.transformation, &a.path)
                == (&b.target, b.kind, &b.transformation, &b.path)
        },
        |impact| &mut impact.evidence,
    );
    let possible = normalize_impacts(
        possible,
        |a, b| {
            (&a.target, a.kind, &a.transformation, &a.path)
                == (&b.target, b.kind, &b.transformation, &b.path)
        },
        |impact| &mut impact.evidence,
    );

    let mut report = ColumnImpactReport {
        model: model_name.to_string(),
        column: column_name.to_string(),
        input,
        scope,
        confirmed,
        possible,
        exploration: exploration_for(scope, gaps, &explored, row_uncollected),
        evidence: Vec::new(),
        errors: normalize_column_lineage_errors(errors),
    };
    // Each analysed model describes the evidence its result cites, which
    // includes everything the report cites.
    let ledger: Vec<SchemaEvidenceRecord> = lineage_cache
        .values()
        .flat_map(|lineage| lineage.evidence.iter().cloned())
        .collect();
    report.evidence = cited_evidence(report.cited(), &ledger);
    report
}

fn unresolved_may_include_input(
    reason: &UnresolvedReason,
    source_uid: &str,
    source_input: &ImpactInput,
    input_column: Option<&Ident>,
    candidates: &[(
        super::TraceResourceCandidate,
        Option<super::TraceBoundaryReason>,
    )],
    dialect: super::DlinDialect,
) -> bool {
    let relation_matches_source = |relation: &RelationRef| {
        candidates.iter().any(|(candidate, _)| {
            candidate.kind == super::TraceResourceKind::Model
                && candidate.unique_id == source_uid
                && relation.matches(&candidate.relation, dialect)
        })
    };
    let name_matches = |column: &Ident| {
        input_column.is_some_and(|input| column.column_key(dialect) == input.column_key(dialect))
    };
    match reason {
        UnresolvedReason::NamedBehindStar { relation, column } => {
            name_matches(column) && relation_matches_source(relation)
        }
        UnresolvedReason::Ambiguous {
            column,
            candidates: unresolved,
        } => {
            if !name_matches(column) {
                return false;
            }
            unresolved.iter().any(|candidate| match candidate {
                UnresolvedCandidate::Relation { relation } => relation_matches_source(relation),
                UnresolvedCandidate::Cte { .. } | UnresolvedCandidate::Derived { .. } => false,
            })
        }
        UnresolvedReason::AmbiguousOutput {
            relation,
            column,
            candidates: slots,
        } => {
            if !relation_matches_source(relation) {
                return false;
            }
            match source_input {
                ImpactInput::Slot(id) => slots
                    .iter()
                    .any(|slot| slot.unique_id == source_uid && slot.slot == *id),
                ImpactInput::Name(_) => name_matches(column),
            }
        }
        UnresolvedReason::AlignmentUnknown {
            possible_sources, ..
        } => possible_sources.iter().any(|source| {
            name_matches(&source.column) && relation_matches_source(&source.relation)
        }),
        UnresolvedReason::TraceBoundary { reason, resource } => {
            *reason == super::TraceBoundaryReason::AmbiguousModel
                && resource.column.as_ref().is_none_or(name_matches)
                && resource
                    .candidates
                    .iter()
                    .any(|candidate| candidate.unique_id == source_uid)
        }
        _ => false,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RelationRelevance {
    Relevant,
    Unrelated,
    Unknown,
}

fn combine_relevance(results: impl IntoIterator<Item = RelationRelevance>) -> RelationRelevance {
    let mut any = false;
    let mut unknown = false;
    for result in results {
        any = true;
        match result {
            RelationRelevance::Relevant => return RelationRelevance::Relevant,
            RelationRelevance::Unrelated => {}
            RelationRelevance::Unknown => unknown = true,
        }
    }
    if !any || unknown {
        RelationRelevance::Unknown
    } else {
        RelationRelevance::Unrelated
    }
}

fn unresolved_relevance(
    reason: &UnresolvedReason,
    owner_uid: &str,
    original_input: (&str, &Ident),
    input_reachable: &BTreeSet<String>,
    manifest: &Manifest,
    dialect: super::DlinDialect,
) -> RelationRelevance {
    let related = |relation: &RelationRef| {
        relation_relevance(relation, owner_uid, input_reachable, manifest, dialect)
    };
    let named = |relation: &RelationRef, column: &Ident| {
        named_relation_relevance(
            relation,
            column,
            owner_uid,
            original_input,
            input_reachable,
            manifest,
            dialect,
        )
    };
    match reason {
        UnresolvedReason::BehindStar { relation } => related(relation),
        UnresolvedReason::NamedBehindStar { relation, column }
        | UnresolvedReason::AbsentOutput { relation, column } => named(relation, column),
        UnresolvedReason::AmbiguousOutput {
            relation,
            column,
            candidates,
        } => {
            let relation_result = named(relation, column);
            if candidates.is_empty() {
                relation_result
            } else if relation_result == RelationRelevance::Unrelated {
                RelationRelevance::Unrelated
            } else {
                RelationRelevance::Unknown
            }
        }
        UnresolvedReason::SchemaDependent { relations, .. } => {
            combine_relevance(relations.iter().map(related))
        }
        UnresolvedReason::AlignmentUnknown {
            possible_sources,
            unknown_operands,
            ..
        } => {
            let possible = combine_relevance(
                possible_sources
                    .iter()
                    .map(|source| named(&source.relation, &source.column)),
            );
            if possible == RelationRelevance::Relevant {
                RelationRelevance::Relevant
            } else if !unknown_operands.is_empty() || possible == RelationRelevance::Unknown {
                RelationRelevance::Unknown
            } else {
                RelationRelevance::Unrelated
            }
        }
        UnresolvedReason::Ambiguous {
            column,
            candidates: unresolved,
        } => combine_relevance(unresolved.iter().map(|candidate| match candidate {
            UnresolvedCandidate::Relation { relation } => named(relation, column),
            UnresolvedCandidate::Cte { .. } | UnresolvedCandidate::Derived { .. } => {
                RelationRelevance::Unknown
            }
        })),
        UnresolvedReason::Unbound { .. }
        | UnresolvedReason::Recursive
        | UnresolvedReason::RowValue { .. }
        | UnresolvedReason::Unsupported { .. }
        | UnresolvedReason::TraceBoundary { .. } => RelationRelevance::Unknown,
    }
}

fn named_relation_relevance(
    relation: &RelationRef,
    column: &Ident,
    owner_uid: &str,
    original_input: (&str, &Ident),
    input_reachable: &BTreeSet<String>,
    manifest: &Manifest,
    dialect: super::DlinDialect,
) -> RelationRelevance {
    let resolved =
        super::cross_model::resolve_resource(manifest, owner_uid, relation, column, dialect);
    if resolved.reason == super::TraceBoundaryReason::AmbiguousModel {
        return relation_relevance(relation, owner_uid, input_reachable, manifest, dialect);
    }
    if let Some(model_uid) = resolved.model_unique_id {
        if model_uid == original_input.0 {
            return match same_column_key(column, original_input.1, dialect) {
                Some(true) => RelationRelevance::Relevant,
                Some(false) => RelationRelevance::Unrelated,
                None => RelationRelevance::Unknown,
            };
        }
        if input_reachable.contains(&model_uid) {
            // A downstream model is reachable from the input, but its output
            // column name does not prove whether the original input contributed.
            return RelationRelevance::Unknown;
        }
        return RelationRelevance::Unrelated;
    }
    relation_relevance(relation, owner_uid, input_reachable, manifest, dialect)
}

fn same_column_key(left: &Ident, right: &Ident, dialect: super::DlinDialect) -> Option<bool> {
    Some(left.column_key(dialect)? == right.column_key(dialect)?)
}

/// Resolve whether a relation is on an input-reachable model, is a known
/// external relation, or cannot be classified from the manifest.
fn relation_relevance(
    relation: &RelationRef,
    owner_uid: &str,
    input_reachable: &BTreeSet<String>,
    manifest: &Manifest,
    dialect: super::DlinDialect,
) -> RelationRelevance {
    let resolved = super::cross_model::resolve_resource(
        manifest,
        owner_uid,
        relation,
        &Ident::parse_reference("*"),
        dialect,
    );
    if resolved.reason == super::TraceBoundaryReason::AmbiguousModel {
        return combine_relevance(resolved.resource.candidates.iter().map(|candidate| {
            if input_reachable.contains(&candidate.unique_id) {
                RelationRelevance::Relevant
            } else {
                RelationRelevance::Unrelated
            }
        }));
    }
    if let Some(unique_id) = resolved.resource.unique_id {
        return if input_reachable.contains(&unique_id) {
            RelationRelevance::Relevant
        } else {
            RelationRelevance::Unrelated
        };
    }
    if resolved.reason == super::TraceBoundaryReason::MissingDefinition {
        return RelationRelevance::Unknown;
    }
    // A physical relation without a declared identity can be external, but
    // its non-match against in-scope dependencies does not prove that it is
    // unrelated to the input.
    RelationRelevance::Unknown
}

fn downstream_model_ids(manifest: &Manifest, start: &str) -> BTreeSet<String> {
    let downstream = build_downstream_model_map(manifest);
    let mut found = BTreeSet::from([start.to_owned()]);
    let mut queue = vec![start.to_owned()];
    while let Some(current) = queue.pop() {
        for next in downstream.get(&current).into_iter().flatten() {
            if found.insert(next.clone()) {
                queue.push(next.clone());
            }
        }
    }
    found
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
        if !matches!(node.resource_type.as_str(), "model" | "snapshot") {
            continue;
        }
        for dep_id in &node.depends_on.nodes {
            if let Some(dep_node) = manifest.nodes.get(dep_id)
                && matches!(dep_node.resource_type.as_str(), "model" | "snapshot")
            {
                map.entry(dep_node.unique_id.clone())
                    .or_default()
                    .push(node.unique_id.clone());
            }
        }
    }

    map
}

#[cfg(test)]
mod impact_relevance_tests {
    use super::*;
    use crate::parser::manifest::{DependsOn, ManifestConfig, ManifestNode};

    fn model_node(unique_id: &str, name: &str, dependencies: &[&str]) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: Default::default(),
            compiled_code: Some("SELECT 1".to_string()),
            database: None,
            schema: None,
        }
    }

    #[test]
    fn named_input_column_comparison_uses_dialect_identifier_keys() {
        let input_uid = "model.proj.input";
        let owner_uid = "model.proj.owner";
        let manifest = Manifest {
            nodes: HashMap::from([
                (input_uid.to_string(), model_node(input_uid, "input", &[])),
                (
                    owner_uid.to_string(),
                    model_node(owner_uid, "owner", &[input_uid]),
                ),
            ]),
            ..Default::default()
        };
        let reachable = BTreeSet::from([input_uid.to_string(), owner_uid.to_string()]);
        let relation = RelationRef::from_manifest(None, None, "input");
        let original_column = Ident::parse_reference("Id");

        assert_eq!(
            named_relation_relevance(
                &relation,
                &Ident::parse_reference("id"),
                owner_uid,
                (input_uid, &original_column),
                &reachable,
                &manifest,
                super::super::DlinDialect::Snowflake,
            ),
            RelationRelevance::Relevant
        );
        assert_eq!(
            named_relation_relevance(
                &relation,
                &Ident::parse_reference("\"id\""),
                owner_uid,
                (input_uid, &original_column),
                &reachable,
                &manifest,
                super::super::DlinDialect::Snowflake,
            ),
            RelationRelevance::Unrelated
        );
        assert_eq!(
            named_relation_relevance(
                &relation,
                &Ident::parse_reference("\"id\""),
                owner_uid,
                (input_uid, &original_column),
                &reachable,
                &manifest,
                super::super::DlinDialect::BigQuery,
            ),
            RelationRelevance::Relevant
        );
    }
}
