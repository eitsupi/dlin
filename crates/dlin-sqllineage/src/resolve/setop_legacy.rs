//! The positional merge of a set operation's operands, kept until slot
//! correspondence is modelled.
//!
//! The merge pairs the columns of the two operands by position and returns the
//! left operand's mappings, merged with what the right operand contributes,
//! followed by the right operand's columns that no left column stands for. The
//! mappings that stand for a column or a segment the operation publishes carry
//! that output's identity, which is how a reader finds one of them again. The
//! columns that come from the right operand alone carry none.

use std::collections::HashSet;
use std::rc::Rc;

use super::context::EvalContext;
use super::mapping::{Cut, mappings_of, merge_transform, operand_view, resolve_scope_mappings};
use super::model::OutputModel;

use crate::Unresolved;
use crate::graph::scope::ScopeId;
use crate::types::{ColumnMapping, ColumnOrigin, TransformKind};

/// The mappings of the set operation `scope` over `left` and `right`, tagged
/// with the operation's own published outputs. A recursive operation keeps its
/// left operand's mappings and marks them as recursive; its right term reads
/// the operation itself and is not merged.
pub(super) fn merge_operands(
    model: &Rc<OutputModel>,
    left: ScopeId,
    right: ScopeId,
    recursive: bool,
    cx: &mut EvalContext<'_, '_>,
) -> Result<Vec<ColumnMapping>, Cut> {
    let left_view = operand_view(model, left, cx);
    let left_mappings = mappings_of(left, &left_view, cx)?.to_vec();
    let mut merged = if recursive {
        // The branch resolvers expand stars independently. Preserve the left
        // branch's names and order, as SQL set operations do.
        left_mappings
            .into_iter()
            .map(|left_mapping| ColumnMapping {
                star_position: crate::types::StarPosition::Inferred,
                ordinal: left_mapping.ordinal,
                output_name: left_mapping.output_name,
                target: left_mapping.target,
                sources: vec![ColumnOrigin::Recursive {
                    base_sources: left_mapping.sources,
                }],
                transform: left_mapping.transform,
                slot: left_mapping.slot,
                unpublished_tail: left_mapping.unpublished_tail,
                merged_in: left_mapping.merged_in,
            })
            .collect()
    } else {
        let mut right_mappings = resolve_scope_mappings(right, cx)?.to_vec();
        // Nothing of the right operand stands for a column the operation
        // publishes, whatever the right operand published it as. Whatever of
        // it the merge keeps is merged in.
        for mapping in &mut right_mappings {
            mapping.slot = None;
            mapping.unpublished_tail = true;
            mapping.merged_in = true;
        }
        if mappings_have_unknown_shape(&left_mappings)
            || mappings_have_unknown_shape(&right_mappings)
            || !model.unknown_reasons.is_empty()
        {
            merge_unknown_shape_mappings(left_mappings, right_mappings)
        } else {
            merge_proven_positions(left_mappings, &right_mappings)
        }
    };
    // Attach every unknown-width source to each mapping produced by this set
    // operation, including mappings retained by an early unknown-star merge
    // and recursive set operations.
    for mapping in &mut merged {
        append_unsupported_reasons(&mut mapping.sources, &model.unknown_reasons);
    }
    retag(&mut merged, model)?;
    Ok(merged)
}

/// Replace the identity each mapping has in the model of the scope it was
/// computed for with the identity of the output of `reader` that stands for
/// it, from the correspondence `reader` recorded when it published.
pub(super) fn retag(mappings: &mut [ColumnMapping], reader: &OutputModel) -> Result<(), Cut> {
    let mut published = HashSet::new();
    for mapping in mappings.iter_mut() {
        let Some(from) = mapping.slot else {
            // The only output a positional merge can append without a slot
            // identity is a column contributed by its right branch.
            if mapping.unpublished_tail {
                continue;
            }
            return Err(Cut);
        };
        let mut matches = reader.forwarded.iter().filter(|pair| pair.from == from);
        let Some(pair) = matches.next() else {
            return Err(Cut);
        };
        if matches.next().is_some() || !published.insert(pair.to) {
            return Err(Cut);
        }
        mapping.slot = Some(pair.to);
    }
    Ok(())
}

/// Align set-operation contributions only where both mappings carry the same
/// proven ordinal. Display order is not evidence of correspondence.
fn merge_proven_positions(left: Vec<ColumnMapping>, right: &[ColumnMapping]) -> Vec<ColumnMapping> {
    left.into_iter()
        .map(|mut left_mapping| {
            let aligned = left_mapping.ordinal.and_then(|ordinal| {
                right
                    .iter()
                    .find(|mapping| mapping.ordinal == Some(ordinal))
            });
            if let Some(right_mapping) = aligned {
                let (sources, transform) = merge_branch_sources(&left_mapping, right_mapping);
                left_mapping.sources = sources;
                left_mapping.transform = transform;
            } else if left_mapping.output_name.is_some() {
                left_mapping
                    .sources
                    .push(ColumnOrigin::Unresolved(Unresolved::AlignmentUnknown));
            }
            left_mapping
        })
        .collect()
}

fn mappings_have_unknown_shape(mappings: &[ColumnMapping]) -> bool {
    mappings.iter().any(|mapping| mapping.output_name.is_none())
}

fn merge_unknown_shape_mappings(
    left: Vec<ColumnMapping>,
    right: Vec<ColumnMapping>,
) -> Vec<ColumnMapping> {
    let left_barrier = first_unknown_mapping(&left).unwrap_or(left.len());
    let right_barrier = first_unknown_mapping(&right).unwrap_or(right.len());

    // A leading unknown star determines the output names for the set
    // operation. When the left side is already a merged set (and therefore
    // contains named slots contributed by an earlier operand), a right-only
    // tail would publish names that the leading operand never declared. Keep
    // the left candidates and their existing wildcard provenance; a direct
    // two-branch `SELECT * UNION SELECT a, b` still retains the right branch
    // names because there are no prior named slots to preserve.
    if left_barrier == 0 && left.len() > 1 && right_barrier == right.len() {
        return left
            .into_iter()
            .map(|mut mapping| {
                if mapping.output_name.is_some() && mapping.ordinal.is_none() {
                    mapping
                        .sources
                        .push(ColumnOrigin::Unresolved(Unresolved::AlignmentUnknown));
                }
                mapping
            })
            .collect();
    }

    let prefix_len = left_barrier.min(right_barrier);
    let left_unknown = wildcard_sources(&left);
    let right_unknown = wildcard_sources(&right);
    let mut merged = Vec::with_capacity(left.len() + right.len() - prefix_len);

    for (left_mapping, right_mapping) in left.iter().zip(right.iter()).take(prefix_len) {
        let mut mapping = left_mapping.clone();
        if left_mapping.ordinal.is_some() && left_mapping.ordinal == right_mapping.ordinal {
            let (sources, transform) = merge_branch_sources(left_mapping, right_mapping);
            mapping.sources = sources;
            mapping.transform = transform;
        } else if left_mapping.output_name.is_some() {
            mapping
                .sources
                .push(ColumnOrigin::Unresolved(Unresolved::AlignmentUnknown));
        }
        merged.push(mapping);
    }
    // Catalog evidence can prove an individual position beyond an unknown
    // interval. Such a slot still aligns with exactly that position, rather
    // than losing a guaranteed branch contribution at the prefix barrier.
    let proven_left: Vec<_> = left.iter().filter_map(|mapping| mapping.ordinal).collect();
    merged.extend(left.into_iter().skip(prefix_len).map(|mut mapping| {
        if let Some(other) = mapping
            .ordinal
            .and_then(|ordinal| right.iter().find(|other| other.ordinal == Some(ordinal)))
        {
            let (sources, transform) = merge_branch_sources(&mapping, other);
            mapping.sources = sources;
            mapping.transform = transform;
            mapping
        } else {
            append_unknown_sources(mapping, &right_unknown)
        }
    }));
    merged.extend(
        right
            .into_iter()
            .skip(prefix_len)
            .filter(|mapping| {
                !mapping
                    .ordinal
                    .is_some_and(|ordinal| proven_left.contains(&ordinal))
            })
            .map(|mapping| append_unknown_sources(mapping, &left_unknown)),
    );
    merged
}

fn append_unsupported_reasons(sources: &mut Vec<ColumnOrigin>, reasons: &[&'static str]) {
    for &what in reasons {
        if !sources.iter().any(|source| {
            matches!(source, ColumnOrigin::Unresolved(Unresolved::Unsupported { what: existing }) if *existing == what)
        }) {
            sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }));
        }
    }
}

/// Merge two positional branch mappings while retaining the fact that one
/// branch was source-free. An empty source list is meaningful for literals;
/// dropping it would overclaim complete lineage from the other branch.
fn merge_branch_sources(
    left: &ColumnMapping,
    right: &ColumnMapping,
) -> (Vec<ColumnOrigin>, TransformKind) {
    let mut sources = left.sources.clone();
    sources.extend(right.sources.clone());
    if left.sources.is_empty() != right.sources.is_empty() {
        sources.push(ColumnOrigin::SourceFree {
            column: left.target.column.clone(),
        });
    }
    (sources, merge_transform(&left.transform, &right.transform))
}

fn first_unknown_mapping(mappings: &[ColumnMapping]) -> Option<usize> {
    mappings
        .iter()
        .position(|mapping| mappings_have_unknown_shape(std::slice::from_ref(mapping)))
}

fn wildcard_sources(mappings: &[ColumnMapping]) -> Vec<ColumnOrigin> {
    let mut sources = Vec::new();
    for mapping in mappings {
        for source in &mapping.sources {
            match source {
                ColumnOrigin::Wildcard { .. } => sources.push(source.clone()),
                source if source.is_unknown_star() => sources.push(source.clone()),
                ColumnOrigin::Recursive { base_sources } => sources.extend(
                    base_sources
                        .iter()
                        .filter(|source| {
                            matches!(source, ColumnOrigin::Wildcard { .. })
                                || source.is_unknown_star()
                        })
                        .cloned(),
                ),
                _ => {}
            }
        }
    }
    sources
}

fn append_unknown_sources(
    mut mapping: ColumnMapping,
    unknown_sources: &[ColumnOrigin],
) -> ColumnMapping {
    if !unknown_sources.is_empty() {
        mapping.ordinal = None;
    }
    if mapping.sources.is_empty() && !unknown_sources.is_empty() {
        mapping.sources.push(ColumnOrigin::SourceFree {
            column: mapping.target.column.clone(),
        });
    }
    mapping.sources.extend(unknown_sources.iter().cloned());
    mapping
}

#[cfg(test)]
mod tests {
    use super::{mappings_have_unknown_shape, wildcard_sources};
    use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TransformKind};
    use crate::{Ident, Quote};

    #[test]
    fn only_typed_unknown_stars_create_unknown_shape_barriers() {
        for recursive in [false, true] {
            for quoted in [false, true] {
                let name = if quoted {
                    Ident::quoted("*", Quote::Double)
                } else {
                    Ident::unquoted("*")
                };
                let origin = if quoted {
                    ColumnOrigin::Unresolved(crate::Unresolved::Unbound {
                        column: name.clone(),
                    })
                } else {
                    ColumnOrigin::UnknownStar
                };
                let origin = if recursive {
                    ColumnOrigin::Recursive {
                        base_sources: vec![origin],
                    }
                } else {
                    origin
                };
                let mappings = [ColumnMapping {
                    star_position: crate::types::StarPosition::Inferred,
                    ordinal: None,
                    output_name: if quoted {
                        Some(crate::OutputName::Explicit(name.clone()))
                    } else {
                        None
                    },
                    target: ColumnRef {
                        table: None,
                        column: name,
                    },
                    sources: vec![origin],
                    transform: TransformKind::Direct,
                    slot: None,
                    unpublished_tail: false,
                    merged_in: false,
                }];
                assert_eq!(mappings_have_unknown_shape(&mappings), !quoted);
                assert_eq!(wildcard_sources(&mappings).is_empty(), quoted);
            }
        }
    }
}
