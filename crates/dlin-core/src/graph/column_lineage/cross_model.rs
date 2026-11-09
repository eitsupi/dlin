use crate::parser::manifest::Manifest;
use std::collections::HashSet;

use super::relation::{RelationRef, RelationResolution, matching_indices, resolve_unique};
use super::{
    ColumnLineageAnalysis, InternalColumnSource, InternalModelColumnLineage, ModelOutputRef,
    ModelPathHop, OutputSlotId, SlotLookup, TraceBoundary, TraceBoundaryPolicy,
    TraceBoundaryReason, TraceResource, TraceResourceCandidate, TraceResourceKind,
    UnresolvedContribution, UnresolvedReason, ValueLineage, find_model_by_name,
    find_model_by_unique_id,
};

/// What the trace of one root output slot has collected so far.
#[derive(Default)]
struct TraceAccumulator {
    sources: Vec<InternalColumnSource>,
    unresolved: Vec<UnresolvedContribution>,
    boundaries: Vec<TraceBoundary>,
}

#[derive(Debug, Clone)]
pub(super) struct ResolvedResource {
    pub(super) resource: TraceResource,
    pub(super) reason: TraceBoundaryReason,
    pub(super) model_unique_id: Option<String>,
}

/// The upstream outputs that the trace of one root slot has already entered.
/// A slot is identified by its model and slot id, never by its name.
type Visited = HashSet<(String, OutputSlotId)>;

pub(super) fn compute_cross_model_column_lineage<'a, 'm>(
    analysis: &'a mut ColumnLineageAnalysis<'m>,
    model_name: &str,
) -> InternalModelColumnLineage {
    let mut ctx = CrossModelContext { analysis };
    compute_cross_model_inner(model_name, &mut ctx)
}

struct CrossModelContext<'a, 'm> {
    analysis: &'a mut ColumnLineageAnalysis<'m>,
}

fn compute_cross_model_inner(
    model_name: &str,
    ctx: &mut CrossModelContext<'_, '_>,
) -> InternalModelColumnLineage {
    let mut result = super::compute_column_lineage_internal(ctx.analysis, model_name);
    let Some(root_node) = find_model_by_unique_id(ctx.analysis.manifest, model_name)
        .or_else(|| find_model_by_name(ctx.analysis.manifest, model_name))
    else {
        return result;
    };
    let root_uid = root_node.unique_id.clone();

    let model = result.model.clone();
    let errors = &mut result.errors;
    let Some(output) = result.output.as_mut() else {
        return result;
    };
    for slot_index in 0..output.columns.len() {
        let slot = &output.columns[slot_index];
        let root = ModelOutputRef {
            unique_id: root_uid.clone(),
            model: model.clone(),
            slot: slot.id,
            column: slot.name.to_string(),
        };
        let mut acc = TraceAccumulator {
            sources: Vec::new(),
            unresolved: slot.lineage.unresolved().to_vec(),
            boundaries: Vec::new(),
        };
        let mut visited = Visited::new();
        visited.insert((root_uid.clone(), slot.id));

        if slot.lineage.is_complete() && slot.lineage.known().is_empty() {
            acc.boundaries.push(TraceBoundary {
                root: root.clone(),
                at: root.clone(),
                reason: TraceBoundaryReason::SourceFree,
                resource: TraceResource {
                    unique_id: Some(root_uid.clone()),
                    name: root_node.name.clone(),
                    kind: TraceResourceKind::Model,
                    relation: RelationRef::from_manifest(
                        root_node.database.as_deref(),
                        root_node.schema.as_deref(),
                        root_node.relation_name(),
                    ),
                    column: slot.name.ident().cloned(),
                    candidates: Vec::new(),
                    policy: TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal,
                },
                model_path: Vec::new(),
            });
        }

        for source in slot.lineage.known() {
            let branch_visited = visited.clone();
            resolve_source_recursive(source, &root, branch_visited, &mut acc, errors, ctx, &[]);
        }

        let TraceAccumulator {
            mut sources,
            mut unresolved,
            mut boundaries,
        } = acc;
        sources.sort_by(|a, b| (&a.relation, &a.column).cmp(&(&b.relation, &b.column)));
        sources.dedup();
        unresolved.sort();
        unresolved.dedup();
        boundaries.sort();
        boundaries.dedup();
        result.boundaries.extend(boundaries);
        output.columns[slot_index].lineage = ValueLineage::new(sources, unresolved);
    }

    result.boundaries.sort();
    result.boundaries.dedup();

    result
}

/// Resolve one SQL relation using only the manifest resources that the owner
/// declares as dependencies. This is shared by lineage and impact analysis.
pub(super) fn resolve_resource(
    manifest: &Manifest,
    owner_unique_id: &str,
    relation: &RelationRef,
    column: &super::Ident,
    dialect: super::DlinDialect,
) -> ResolvedResource {
    let candidates = upstream_resource_candidates(manifest, owner_unique_id);
    let mut missing_dependency_ids = missing_dependency_ids(manifest, owner_unique_id);
    let relations = candidates
        .iter()
        .map(|(candidate, _)| candidate.relation.clone())
        .collect::<Vec<_>>();
    let matching = matching_indices(relation, &relations, dialect);
    let resolution = resolve_unique(relation, &relations, dialect);
    let (resource, reason, model_unique_id) = match resolution {
        RelationResolution::Unique(index) => {
            let candidate = candidates[index].0.clone();
            (
                TraceResource {
                    unique_id: Some(candidate.unique_id.clone()),
                    name: candidate.name.clone(),
                    kind: candidate.kind.clone(),
                    relation: candidate.relation.clone(),
                    column: Some(column.clone()),
                    candidates: Vec::new(),
                    policy: TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal,
                },
                candidates[index]
                    .1
                    .clone()
                    .unwrap_or(TraceBoundaryReason::MissingDefinition),
                (candidate.kind == TraceResourceKind::Model).then_some(candidate.unique_id),
            )
        }
        RelationResolution::NotFound if !missing_dependency_ids.is_empty() => {
            missing_dependency_ids.sort();
            let missing = missing_dependency_ids
                .iter()
                .map(|unique_id| TraceResourceCandidate {
                    unique_id: unique_id.clone(),
                    name: unique_id
                        .rsplit('.')
                        .next()
                        .unwrap_or(unique_id)
                        .to_string(),
                    kind: if unique_id.starts_with("snapshot.") {
                        TraceResourceKind::Snapshot
                    } else {
                        TraceResourceKind::Model
                    },
                    relation: relation.clone(),
                })
                .collect();
            (
                TraceResource {
                    unique_id: None,
                    name: relation.to_string(),
                    kind: TraceResourceKind::PhysicalRelation,
                    relation: relation.clone(),
                    column: Some(column.clone()),
                    candidates: missing,
                    policy: TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal,
                },
                TraceBoundaryReason::MissingDefinition,
                None,
            )
        }
        RelationResolution::NotFound => (
            TraceResource {
                unique_id: None,
                name: relation.to_string(),
                kind: TraceResourceKind::PhysicalRelation,
                relation: relation.clone(),
                column: Some(column.clone()),
                candidates: Vec::new(),
                policy: TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal,
            },
            TraceBoundaryReason::ExternalRelation,
            None,
        ),
        RelationResolution::Ambiguous => {
            let mut matches = matching
                .iter()
                .map(|index| candidates[*index].0.clone())
                .collect::<Vec<_>>();
            matches.sort();
            (
                TraceResource {
                    unique_id: None,
                    name: relation.to_string(),
                    kind: TraceResourceKind::PhysicalRelation,
                    relation: relation.clone(),
                    column: Some(column.clone()),
                    candidates: matches,
                    policy: TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal,
                },
                TraceBoundaryReason::AmbiguousModel,
                None,
            )
        }
    };
    ResolvedResource {
        resource,
        reason,
        model_unique_id,
    }
}

pub(super) fn upstream_resource_candidates(
    manifest: &Manifest,
    owner_unique_id: &str,
) -> Vec<(TraceResourceCandidate, Option<TraceBoundaryReason>)> {
    let dependencies = manifest
        .nodes
        .get(owner_unique_id)
        .map(|node| {
            node.depends_on
                .nodes
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut candidates = Vec::new();
    for unique_id in dependencies {
        if let Some(node) = manifest.nodes.get(unique_id) {
            let (kind, reason) = match node.resource_type.as_str() {
                "model" => (TraceResourceKind::Model, None),
                "seed" => (
                    TraceResourceKind::Seed,
                    Some(TraceBoundaryReason::ExternalRelation),
                ),
                "snapshot" => (
                    TraceResourceKind::Snapshot,
                    Some(if node.compiled_code.is_some() {
                        TraceBoundaryReason::UnsupportedMaterialization
                    } else {
                        TraceBoundaryReason::MissingDefinition
                    }),
                ),
                _ => continue,
            };
            candidates.push((
                TraceResourceCandidate {
                    unique_id: node.unique_id.clone(),
                    name: node.name.clone(),
                    kind,
                    relation: RelationRef::from_manifest(
                        node.database.as_deref(),
                        node.schema.as_deref(),
                        node.relation_name(),
                    ),
                },
                reason,
            ));
        } else if let Some(source) = manifest.sources.get(unique_id) {
            candidates.push((
                TraceResourceCandidate {
                    unique_id: source.unique_id.clone(),
                    name: format!("{}.{}", source.source_name, source.name),
                    kind: TraceResourceKind::Source,
                    relation: RelationRef::from_manifest(
                        source.database.as_deref(),
                        source.schema.as_deref(),
                        source.identifier.as_deref().unwrap_or(&source.name),
                    ),
                },
                Some(TraceBoundaryReason::ExternalRelation),
            ));
        }
    }
    candidates
}

fn missing_dependency_ids(manifest: &Manifest, owner_unique_id: &str) -> Vec<String> {
    manifest
        .nodes
        .get(owner_unique_id)
        .into_iter()
        .flat_map(|node| &node.depends_on.nodes)
        .filter(|unique_id| {
            (unique_id.starts_with("model.") || unique_id.starts_with("snapshot."))
                && !manifest.nodes.contains_key(*unique_id)
        })
        .cloned()
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn resolve_source_recursive(
    source: &InternalColumnSource,
    root: &ModelOutputRef,
    mut visited: Visited,
    acc: &mut TraceAccumulator,
    errors: &mut Vec<super::ColumnLineageError>,
    ctx: &mut CrossModelContext<'_, '_>,
    current_path: &[ModelPathHop],
) {
    let leaf = |acc: &mut TraceAccumulator| {
        let mut leaf = source.clone();
        leaf.model_path = current_path.to_vec();
        acc.sources.push(leaf);
    };
    let owner_uid = current_path
        .last()
        .map_or(root.unique_id.as_str(), |hop| hop.output.unique_id.as_str());
    let resolved = resolve_resource(
        ctx.analysis.manifest,
        owner_uid,
        &source.relation,
        &source.column,
        ctx.analysis.dialect,
    );
    let at = current_path
        .last()
        .map_or_else(|| root.clone(), |hop| hop.output.clone());
    let push_boundary =
        |acc: &mut TraceAccumulator, reason: TraceBoundaryReason, resource: TraceResource| {
            if !matches!(
                reason,
                TraceBoundaryReason::ExternalRelation | TraceBoundaryReason::SourceFree
            ) {
                acc.unresolved.push(UnresolvedContribution {
                    reason: UnresolvedReason::TraceBoundary {
                        reason: reason.clone(),
                        resource: resource.clone(),
                    },
                    origin: at.clone(),
                    model_path: current_path.to_vec(),
                });
            }
            acc.boundaries.push(TraceBoundary {
                root: root.clone(),
                at: at.clone(),
                reason,
                resource,
                model_path: current_path.to_vec(),
            });
        };
    let Some(model_unique_id) = resolved.model_unique_id.clone() else {
        let is_external = matches!(resolved.reason, TraceBoundaryReason::ExternalRelation);
        push_boundary(acc, resolved.reason, resolved.resource);
        if is_external {
            leaf(acc);
        }
        return;
    };

    let upstream_result = super::compute_column_lineage_internal(ctx.analysis, &model_unique_id);
    let lookup = upstream_result
        .output
        .as_ref()
        .map_or(SlotLookup::Absent, |output| {
            output.lookup(&source.column, ctx.analysis.dialect)
        });

    // Only a failure of the whole model and the diagnostics of the slot that
    // is followed may cross this model boundary. A diagnostic for another
    // slot belongs to a sibling branch (and can even refer to an EXCEPT-ed
    // projection), and a difference between the YAML and the SQL output is
    // about the upstream model alone.
    if upstream_result.output.is_none() {
        let reason = if upstream_result
            .errors
            .iter()
            .any(|error| error.kind == super::ColumnLineageErrorKind::NoCompiledCode)
        {
            TraceBoundaryReason::MissingDefinition
        } else {
            TraceBoundaryReason::AnalysisFailed
        };
        push_boundary(acc, reason, resolved.resource.clone());
        for err in &upstream_result.errors {
            let rebased = err.rebase_to(root);
            if !errors.contains(&rebased) {
                errors.push(rebased);
            }
        }
        return;
    }

    match lookup {
        SlotLookup::Unique(id) => {
            let slot = upstream_result
                .output
                .as_ref()
                .and_then(|output| output.slot(id))
                .expect("a looked-up slot exists");
            for err in &upstream_result.errors {
                if err.is_model_failure() || err.slot() == Some(id) {
                    let rebased = err.rebase_to(root);
                    if !errors.contains(&rebased) {
                        errors.push(rebased);
                    }
                }
            }
            let visited_key = (model_unique_id.clone(), id);
            if !visited.insert(visited_key.clone()) {
                push_boundary(acc, TraceBoundaryReason::Cycle, resolved.resource.clone());
                return;
            }
            let mut extended_path = current_path.to_vec();
            let output_ref = output_ref_of(&model_unique_id, &upstream_result.model, slot);
            extended_path.push(ModelPathHop {
                output: output_ref.clone(),
                transformation: slot.transformation.clone(),
            });

            let (known, unresolved) = (slot.lineage.known(), slot.lineage.unresolved());
            if known.is_empty() && unresolved.is_empty() {
                acc.boundaries.push(TraceBoundary {
                    root: root.clone(),
                    at: output_ref,
                    reason: TraceBoundaryReason::SourceFree,
                    resource: TraceResource {
                        unique_id: Some(model_unique_id.clone()),
                        name: upstream_result.model.clone(),
                        kind: TraceResourceKind::Model,
                        relation: resolved.resource.relation.clone(),
                        column: slot.name.ident().cloned(),
                        candidates: Vec::new(),
                        policy: TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal,
                    },
                    model_path: extended_path.clone(),
                });
            }
            for contribution in unresolved {
                acc.unresolved
                    .push(extend_contribution(contribution, &extended_path));
            }
            for nested_source in known {
                let branch_visited = visited.clone();
                resolve_source_recursive(
                    nested_source,
                    root,
                    branch_visited,
                    acc,
                    errors,
                    ctx,
                    &extended_path,
                );
            }
            visited.remove(&visited_key);
        }
        SlotLookup::Ambiguous(ids) => {
            // Several output columns of the upstream model have this name.
            // None of them is chosen: the source stays a leaf, and the
            // contribution that could not be attributed is recorded where the
            // reference was written.
            let output = upstream_result
                .output
                .as_ref()
                .expect("an ambiguous lookup has an output");
            let candidates = ids
                .iter()
                .filter_map(|id| output.slot(*id))
                .map(|slot| output_ref_of(&model_unique_id, &upstream_result.model, slot))
                .collect();
            acc.unresolved.push(UnresolvedContribution {
                reason: UnresolvedReason::AmbiguousOutput {
                    relation: source.relation.clone(),
                    column: source.column.clone(),
                    candidates,
                },
                origin: at,
                model_path: current_path.to_vec(),
            });
        }
        // The column is not among the listed outputs of the model (or the
        // model has none): the source stays where it was found.
        SlotLookup::Absent => {
            let reason = match upstream_result.output.as_ref().map(|output| &output.shape) {
                Some(super::OutputShape::Open { .. }) => UnresolvedReason::NamedBehindStar {
                    relation: resolved.resource.relation.clone(),
                    column: source.column.clone(),
                },
                _ => UnresolvedReason::AbsentOutput {
                    relation: resolved.resource.relation.clone(),
                    column: source.column.clone(),
                },
            };
            acc.unresolved.push(UnresolvedContribution {
                reason,
                origin: at,
                model_path: current_path.to_vec(),
            });
        }
    }
}

fn output_ref_of(
    unique_id: &str,
    model: &str,
    slot: &super::OutputSlot<InternalColumnSource>,
) -> ModelOutputRef {
    ModelOutputRef {
        unique_id: unique_id.to_string(),
        model: model.to_string(),
        slot: slot.id,
        column: slot.name.to_string(),
    }
}

/// Re-express a contribution found in an upstream model relative to the root
/// output. `prefix` is the hop path from the root to the upstream model,
/// including the upstream hop itself. The origin is kept as it was found.
fn extend_contribution(
    contribution: &UnresolvedContribution,
    prefix: &[ModelPathHop],
) -> UnresolvedContribution {
    let mut model_path = prefix.to_vec();
    model_path.extend(contribution.model_path.iter().cloned());
    UnresolvedContribution {
        reason: contribution.reason.clone(),
        origin: contribution.origin.clone(),
        model_path,
    }
}
