use crate::parser::manifest::Manifest;
use std::collections::{HashMap, HashSet};

use super::relation::{RelationRef, RelationResolution, resolve_unique};
use super::{
    ColumnLineageAnalysis, InternalColumnSource, InternalModelColumnLineage, ModelOutputRef,
    ModelPathHop, OutputSlotId, SlotLookup, UnresolvedContribution, UnresolvedReason, ValueLineage,
    find_model_by_name, find_model_by_unique_id,
};

/// What the trace of one root output slot has collected so far.
#[derive(Default)]
struct TraceAccumulator {
    sources: Vec<InternalColumnSource>,
    unresolved: Vec<UnresolvedContribution>,
}

/// The upstream outputs that the trace of one root slot has already entered.
/// A slot is identified by its model and slot id, never by its name.
type Visited = HashSet<(String, OutputSlotId)>;

pub(super) fn compute_cross_model_column_lineage<'a, 'm>(
    analysis: &'a mut ColumnLineageAnalysis<'m>,
    model_name: &str,
) -> InternalModelColumnLineage {
    let mut ctx = CrossModelContext {
        analysis,
        in_memory_cache: HashMap::new(),
        computing: HashSet::new(),
    };
    let key = find_model_by_unique_id(ctx.analysis.manifest, model_name)
        .or_else(|| find_model_by_name(ctx.analysis.manifest, model_name))
        .map_or_else(|| model_name.to_string(), |node| node.unique_id.clone());
    ctx.computing.insert(key);
    compute_cross_model_inner(model_name, &mut ctx)
}

struct CrossModelContext<'a, 'm> {
    analysis: &'a mut ColumnLineageAnalysis<'m>,
    in_memory_cache: HashMap<String, InternalModelColumnLineage>,
    computing: HashSet<String>,
}

fn compute_cross_model_inner(
    model_name: &str,
    ctx: &mut CrossModelContext<'_, '_>,
) -> InternalModelColumnLineage {
    let mut result = super::compute_column_lineage_internal(ctx.analysis, model_name);
    let Some(root_uid) = find_model_by_unique_id(ctx.analysis.manifest, model_name)
        .or_else(|| find_model_by_name(ctx.analysis.manifest, model_name))
        .map(|node| node.unique_id.clone())
    else {
        return result;
    };
    let upstream_models = build_upstream_model_relations(ctx.analysis.manifest, model_name);

    let model = result.model.clone();
    let errors = &mut result.errors;
    let Some(output) = result.output.as_mut() else {
        return result;
    };
    for slot in &mut output.columns {
        let root = ModelOutputRef {
            unique_id: root_uid.clone(),
            model: model.clone(),
            slot: slot.id,
            column: slot.name.to_string(),
        };
        let mut acc = TraceAccumulator {
            sources: Vec::new(),
            unresolved: slot.lineage.unresolved().to_vec(),
        };
        let mut visited = Visited::new();
        visited.insert((root_uid.clone(), slot.id));

        for source in slot.lineage.known() {
            resolve_source_recursive(
                source,
                &root,
                &upstream_models,
                &mut visited,
                &mut acc,
                errors,
                ctx,
                &[],
            );
        }

        let TraceAccumulator {
            mut sources,
            mut unresolved,
        } = acc;
        sources.sort_by(|a, b| (&a.relation, &a.column).cmp(&(&b.relation, &b.column)));
        sources.dedup();
        unresolved.sort();
        unresolved.dedup();
        slot.lineage = ValueLineage::new(sources, unresolved);
    }

    result
}

pub(super) fn build_upstream_model_relations(
    manifest: &Manifest,
    model_name: &str,
) -> Vec<(RelationRef, String)> {
    let mut relations = Vec::new();

    let node = find_model_by_unique_id(manifest, model_name)
        .or_else(|| find_model_by_name(manifest, model_name));
    let node = match node {
        Some(n) => n,
        None => return relations,
    };

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if dep_node.resource_type != "model" {
                continue;
            }
            let relation = RelationRef::from_manifest(
                dep_node.database.as_deref(),
                dep_node.schema.as_deref(),
                dep_node.relation_name(),
            );
            relations.push((relation, dep_node.unique_id.clone()));
        }
    }

    relations
}

#[allow(clippy::too_many_arguments)]
fn resolve_source_recursive(
    source: &InternalColumnSource,
    root: &ModelOutputRef,
    upstream_models: &[(RelationRef, String)],
    visited: &mut Visited,
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
    let candidates = upstream_models
        .iter()
        .map(|(relation, _)| relation.clone())
        .collect::<Vec<_>>();
    let model_unique_id = match resolve_unique(&source.relation, &candidates, ctx.analysis.dialect)
    {
        RelationResolution::Unique(index) => upstream_models[index].1.clone(),
        RelationResolution::Ambiguous | RelationResolution::NotFound => return leaf(acc),
    };

    if !ctx.in_memory_cache.contains_key(&model_unique_id) {
        if ctx.computing.contains(&model_unique_id) {
            return leaf(acc);
        }
        ctx.computing.insert(model_unique_id.clone());
        let upstream_result = compute_cross_model_inner(&model_unique_id, ctx);
        ctx.in_memory_cache
            .insert(model_unique_id.clone(), upstream_result);
    }
    let upstream_result = ctx.in_memory_cache.get(&model_unique_id).unwrap();
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
    let followed = match &lookup {
        SlotLookup::Unique(id) => Some(*id),
        SlotLookup::Ambiguous(_) | SlotLookup::Absent => None,
    };
    if let Some(id) = followed
        && !visited.insert((model_unique_id.clone(), id))
    {
        return leaf(acc);
    }
    for err in &upstream_result.errors {
        if err.is_model_failure() || (followed.is_some() && err.slot() == followed) {
            let rebased = err.rebase_to(root);
            if !errors.contains(&rebased) {
                errors.push(rebased);
            }
        }
    }

    match lookup {
        SlotLookup::Unique(id) => {
            let slot = upstream_result
                .output
                .as_ref()
                .and_then(|output| output.slot(id))
                .expect("a looked-up slot exists");
            let mut extended_path = current_path.to_vec();
            extended_path.push(ModelPathHop {
                output: output_ref_of(&model_unique_id, &upstream_result.model, slot),
                transformation: slot.transformation.clone(),
            });

            let (known, unresolved) = (slot.lineage.known(), slot.lineage.unresolved());
            if known.is_empty() {
                // Leaf: the column exists at model_name but has no known sources.
                // Don't include model_name in model_path since it IS the leaf (avoids self-loop).
                // This holds for a partial column as well: its nearest terminal is
                // kept, and the unresolved contributions are carried next to it.
                leaf(acc);
            } else {
                for s in known {
                    let mut merged = s.clone();
                    let mut full_path = extended_path.clone();
                    full_path.extend(s.model_path.iter().cloned());
                    merged.model_path = full_path;
                    acc.sources.push(merged);
                }
            }
            for contribution in unresolved {
                acc.unresolved
                    .push(extend_contribution(contribution, &extended_path));
            }
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
            let origin = current_path
                .last()
                .map_or_else(|| root.clone(), |hop| hop.output.clone());
            acc.unresolved.push(UnresolvedContribution {
                reason: UnresolvedReason::AmbiguousOutput {
                    relation: source.relation.clone(),
                    column: source.column.clone(),
                    candidates,
                },
                origin,
                model_path: current_path.to_vec(),
            });
            leaf(acc);
        }
        // The column is not among the listed outputs of the model (or the
        // model has none): the source stays where it was found.
        SlotLookup::Absent => leaf(acc),
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
