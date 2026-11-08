//! Materialize set-operation values from the correspondence the model proved.

use std::collections::HashSet;
use std::rc::Rc;

use super::context::EvalContext;
use super::mapping::{Cut, mappings_of, merge_transform, models};
use super::model::{OutputElement, OutputModel, OutputRef, SegmentId, SlotMatch, SlotMatchKind};
use crate::graph::scope::ScopeId;
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, StarPosition, TransformKind};
use crate::{Ident, SetCorrespondence, SetOperand, SetOperationKind, SourceColumn, Unresolved};

fn output_mapping(
    output: OutputRef,
    cx: &mut EvalContext<'_, '_>,
) -> Result<Option<ColumnMapping>, Cut> {
    let arena = &cx.mapping_cache.models.arena;
    let scope = arena.owner(output);
    let publication = arena.publication(output);
    let view = models(cx).view(publication);
    let mappings = mappings_of(scope, &view, cx)?;
    let mapping = mappings
        .iter()
        .find(|mapping| mapping.slot == Some(output))
        .cloned();
    if mapping.is_none() && matches!(output, OutputRef::Slot(_)) {
        cx.record_cut();
        return Err(Cut);
    }
    Ok(mapping)
}

fn incomplete(what: &'static str) -> ColumnOrigin {
    ColumnOrigin::Unresolved(Unresolved::Unsupported { what })
}

fn unknown_mapping() -> ColumnMapping {
    ColumnMapping {
        star_position: StarPosition::Unproven,
        output_name: None,
        ordinal: None,
        target: ColumnRef {
            table: None,
            column: Ident::unquoted("*"),
        },
        sources: vec![ColumnOrigin::UnknownStar],
        transform: TransformKind::Direct,
        slot: None,
    }
}

/// Concrete candidates remain candidates even when their own lineage was
/// partial. The missing part is represented by the enclosing operand marker.
fn candidates(origins: &[ColumnOrigin], result: &mut Vec<SourceColumn>) -> bool {
    let mut complete = true;
    for origin in origins {
        match origin {
            ColumnOrigin::Concrete { table, column } => result.push(SourceColumn {
                relation: super::analysis::plain_relation(table),
                column: column.clone(),
            }),
            ColumnOrigin::SourceFree { .. } => {}
            ColumnOrigin::Recursive { base_sources } => {
                candidates(base_sources, result);
                complete = false;
            }
            ColumnOrigin::Unresolved(Unresolved::AlignmentUnknown {
                possible_sources,
                unknown_operands,
                ..
            }) => {
                result.extend(possible_sources.iter().cloned());
                complete &= unknown_operands.is_empty();
            }
            _ => complete = false,
        }
    }
    complete
}

fn alignment(
    operation: SetOperationKind,
    matches: &[SlotMatch],
    left_unknown: bool,
    cx: &mut EvalContext<'_, '_>,
) -> ColumnOrigin {
    let mut possible_sources = Vec::new();
    let mut unknown_operands = Vec::new();
    if left_unknown {
        unknown_operands.push(SetOperand::Left);
    }
    for matched in matches {
        match output_mapping(matched.output, cx) {
            Ok(Some(mapping)) => {
                if !candidates(&mapping.sources, &mut possible_sources)
                    || matches!(matched.output, OutputRef::Segment(_))
                {
                    unknown_operands.push(matched.operand);
                }
            }
            Ok(None) | Err(Cut) => unknown_operands.push(matched.operand),
        }
    }
    ColumnOrigin::Unresolved(Unresolved::AlignmentUnknown {
        operation,
        possible_sources,
        unknown_operands,
    })
}

fn append_reasons(sources: &mut Vec<ColumnOrigin>, model: &OutputModel) {
    for &what in &model.unknown_reasons {
        sources.push(incomplete(what));
    }
}

fn merge_values(left: &mut ColumnMapping, right: &ColumnMapping) {
    let source_free = left.sources.is_empty() != right.sources.is_empty();
    left.sources.extend(right.sources.iter().cloned());
    if source_free {
        left.sources.push(ColumnOrigin::SourceFree {
            column: left.target.column.clone(),
        });
    }
    left.transform = merge_transform(&left.transform, &right.transform);
}

fn recursive(sources: Vec<ColumnOrigin>) -> Vec<ColumnOrigin> {
    vec![ColumnOrigin::Recursive {
        base_sources: sources,
    }]
}

/// The names and positions come from the operation's published model. Only
/// model-proven identity correspondences contribute known right-hand values.
pub(super) fn evaluate_set_operation(
    _scope: ScopeId,
    model: &Rc<OutputModel>,
    cx: &mut EvalContext<'_, '_>,
) -> Result<Vec<ColumnMapping>, Cut> {
    let Some(operation) = model.set_operation.as_ref() else {
        // Model construction can itself have been cut at this scope. Its
        // opaque publication is not a completed set-operation model.
        return Err(Cut);
    };
    let mut result = Vec::new();
    let mut exposed_segments = HashSet::new();
    for element in &model.elements {
        match element {
            OutputElement::Column(slot) => {
                let left = model
                    .forwarded
                    .iter()
                    .find(|pair| pair.to == OutputRef::Slot(slot.published()))
                    .map(|pair| pair.from)
                    .ok_or(Cut)?;
                let OutputRef::Slot(left_id) = left else {
                    return Err(Cut);
                };
                let aligned = operation
                    .slots
                    .iter()
                    .find(|aligned| aligned.left == left_id)
                    .ok_or(Cut)?;
                let mut mapping = output_mapping(left, cx)?.ok_or(Cut)?;
                if operation.kind.correspondence == SetCorrespondence::Positional {
                    if operation.recursive {
                        mapping.sources = recursive(mapping.sources);
                    } else if let Some(proven) = aligned
                        .matches
                        .iter()
                        .find(|matched| matched.kind == SlotMatchKind::Proven)
                    {
                        let right = output_mapping(proven.output, cx)?.ok_or(Cut)?;
                        merge_values(&mut mapping, &right);
                    } else {
                        mapping.sources.push(alignment(
                            operation.kind,
                            &aligned.matches,
                            false,
                            cx,
                        ));
                    }
                }
                mapping.output_name = Some(slot.output_name.clone());
                mapping.ordinal = slot.ordinal;
                mapping.target.column = slot.name.clone();
                mapping.slot = Some(OutputRef::Slot(slot.published()));
                append_reasons(&mut mapping.sources, model);
                result.push(mapping);
            }
            OutputElement::Unknown(segment) => {
                let left = model
                    .forwarded
                    .iter()
                    .find(|pair| pair.to == OutputRef::Segment(segment.published()))
                    .map(|pair| pair.from);
                let mut mapping = match left {
                    Some(output) => output_mapping(output, cx)?.unwrap_or_else(unknown_mapping),
                    None => unknown_mapping(),
                };
                if left.is_none() && operation.kind.correspondence == SetCorrespondence::ByName {
                    // This is the deliberately unsupported remainder, not
                    // an additional failed star expansion.
                    mapping.sources.clear();
                }
                if let Some(OutputRef::Segment(left_id)) = left {
                    let aligned = operation
                        .segments
                        .iter()
                        .find(|aligned| aligned.left == left_id)
                        .ok_or(Cut)?;
                    if operation.kind.correspondence == SetCorrespondence::Positional {
                        if operation.recursive {
                            mapping.sources = recursive(mapping.sources);
                        } else {
                            // Publish each right interval once, even if several
                            // left intervals have overlapping possible positions.
                            for matched in &aligned.matches {
                                if matches!(matched.output, OutputRef::Segment(_))
                                    && exposed_segments.insert(matched.output)
                                    && let Some(right) = output_mapping(matched.output, cx)?
                                {
                                    marker_sources(&right.sources, &mut mapping.sources);
                                }
                            }
                            mapping.sources.push(alignment(
                                operation.kind,
                                &aligned.matches,
                                true,
                                cx,
                            ));
                        }
                    }
                }
                mapping.slot = Some(OutputRef::Segment(segment.published()));
                mapping.star_position = segment.position;
                mapping.ordinal = None;
                mapping.output_name = None;
                append_reasons(&mut mapping.sources, model);
                result.push(mapping);
            }
        }
    }
    Ok(result)
}

/// Preserve the relation markers of an unenumerated interval without
/// asserting that any right-hand column has the queried left-hand name.
fn marker_sources(origins: &[ColumnOrigin], result: &mut Vec<ColumnOrigin>) {
    for origin in origins {
        match origin {
            ColumnOrigin::Wildcard { .. } | ColumnOrigin::UnknownStar => {
                result.push(origin.clone());
            }
            ColumnOrigin::Recursive { base_sources } => marker_sources(base_sources, result),
            _ => {}
        }
    }
}

/// A name is looked up only in the left interval. Right contributions use
/// position candidates from the model, including across nested operations.
pub(super) fn set_operation_segment_sources(
    op: ScopeId,
    left: SegmentId,
    column: &Ident,
    cx: &mut EvalContext<'_, '_>,
) -> Vec<ColumnOrigin> {
    let mut sources = super::star::named_segment_sources(left, column, cx);
    if sources.is_empty() {
        sources.push(ColumnOrigin::Unresolved(Unresolved::Unbound {
            column: column.clone(),
        }));
    }
    let model = models(cx).scope(op);
    let Some(operation) = &model.set_operation else {
        cx.record_cut();
        sources.push(ColumnOrigin::Unresolved(Unresolved::Unbound {
            column: column.clone(),
        }));
        return sources;
    };
    let Some(aligned) = operation
        .segments
        .iter()
        .find(|aligned| aligned.left == left)
    else {
        cx.record_cut();
        sources.push(ColumnOrigin::Unresolved(Unresolved::Unbound {
            column: column.clone(),
        }));
        return sources;
    };
    if operation.kind.correspondence == SetCorrespondence::Positional {
        if operation.recursive {
            sources = recursive(sources);
        } else {
            sources.push(alignment(operation.kind, &aligned.matches, true, cx));
        }
    }
    append_reasons(&mut sources, &model);
    sources
}
