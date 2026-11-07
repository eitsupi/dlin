#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::binding::derive_transform;
use super::catalog;
use super::context::{EvalContext, EvalKey};
use super::lineage::collect_output_sources;
use super::model::{ModelStore, OutputModels, Width};
use super::star::expand_star;

use crate::Ident;
use crate::Unresolved;
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, OutputPlan, VirtualColumnState};
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind};

pub(super) fn wildcard_mapping(
    output_table: Option<&TableRef>,
    source_table: TableRef,
) -> ColumnMapping {
    ColumnMapping {
        star_position: crate::types::StarPosition::Inferred,
        output_name: None,
        ordinal: None,
        target: ColumnRef {
            table: output_table.cloned(),
            column: Ident::unquoted("*"),
        },
        sources: vec![ColumnOrigin::Wildcard {
            table: source_table,
        }],
        transform: TransformKind::Direct,
    }
}

pub(super) fn known_empty_dependency(node_id: NodeId, graph: &RawGraph) -> bool {
    let (name, binding) = match &graph.nodes[node_id] {
        RawNode::Ref { name, binding, .. } | RawNode::Unqualified { name, binding, .. } => {
            (name, binding.as_ref())
        }
        _ => return false,
    };
    let Some(Binding::VirtualSource(source)) = binding else {
        return false;
    };
    let Some(column) = graph
        .scopes
        .virtual_source(*source)
        .columns
        .iter()
        .find(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))
    else {
        return false;
    };
    matches!(column.state, VirtualColumnState::KnownEmpty)
        && column
            .dependencies
            .iter()
            .all(|&dependency| known_empty_dependency(dependency, graph))
}

#[derive(Default)]
pub(super) struct ScopeMappingCache {
    entries: HashMap<usize, ScopeMappingEntry>,
    /// The structural models mappings are evaluated from.
    pub(super) models: ModelStore,
}

/// Mappings already computed for a scope. A scope still being computed is on
/// the evaluation stack instead, so an entry here is always complete.
type ScopeMappingEntry = Arc<[ColumnMapping]>;

#[cfg(test)]
thread_local! {
    static SCOPE_MAPPING_COMPUTATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_scope_mapping_stats() {
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(0));
}

#[cfg(test)]
pub(super) fn scope_mapping_computations() -> usize {
    SCOPE_MAPPING_COMPUTATIONS.with(Cell::get)
}

/// Resolve a scope's output plan into ordered mappings. Set-operation
/// branches are resolved independently so catalog expansion happens before
/// their positional merge.
pub(super) fn resolve_scope_mappings(
    scope: usize,
    cx: &mut EvalContext<'_, '_>,
) -> Arc<[ColumnMapping]> {
    if let Some(mappings) = cx.mapping_cache.entries.get(&scope) {
        return mappings.clone();
    }
    // A scope that is being computed cannot safely publish a partially
    // materialized result. Returning no mappings preserves the existing
    // fallback to raw output resolution and does not cache an incomplete
    // entry as resolved.
    // TODO: re-entry or an exhausted budget must surface as an unresolved
    // value, not as an empty list that readers treat as "no dependencies".
    if !cx.enter(EvalKey::Mappings(scope)) {
        return Arc::from([]);
    }
    let graph = cx.graph;
    #[cfg(test)]
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(computations.get() + 1));

    let mappings = match graph.scopes.output_plan(scope).clone() {
        OutputPlan::Projection => {
            let mut mappings = Vec::new();
            let mut next_ordinal = Some(0usize);
            for col in graph.scopes.output_columns(scope) {
                match &graph.nodes[col.node_id] {
                    RawNode::Output { name, .. } => {
                        let mut visited = HashSet::new();
                        let (sources, edge_kinds, has_back, inherited_transform) =
                            collect_output_sources(col.node_id, cx, &mut visited);
                        let transform = merge_transform(
                            &derive_transform(&graph.nodes[col.node_id], &edge_kinds),
                            &inherited_transform,
                        );
                        mappings.push(ColumnMapping {
                            star_position: crate::types::StarPosition::Inferred,
                            ordinal: next_ordinal,
                            output_name: graph
                                .output_names
                                .get(&col.node_id)
                                .cloned()
                                .or_else(|| Some(crate::OutputName::Explicit(name.clone()))),
                            target: ColumnRef {
                                table: None,
                                column: name.clone(),
                            },
                            sources: if has_back {
                                vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }]
                            } else {
                                sources
                            },
                            transform,
                        });
                        next_ordinal = next_ordinal.map(|ordinal| ordinal + 1);
                    }
                    RawNode::Star { .. } => {
                        let start = mappings.len();
                        let star_width =
                            OutputModels::new(graph, cx.catalog, &mut cx.mapping_cache.models)
                                .star(col.node_id)
                                .width;
                        expand_star(col.node_id, cx, &mut mappings);
                        let base = next_ordinal;
                        for mapping in &mut mappings[start..] {
                            mapping.star_position = mapping.star_position.shifted(base);
                            mapping.ordinal = mapping
                                .ordinal
                                .and_then(|ordinal| base.map(|base| base + ordinal));
                        }
                        next_ordinal = match (base, star_width) {
                            (Some(base), Width::Exact(width)) => Some(base + width),
                            _ => None,
                        };
                    }
                    _ => {}
                }
            }
            if let Some(cat) = cx.catalog {
                catalog::apply_catalog(&mut mappings, cat);
            }
            mappings
        }
        OutputPlan::Delegate(child) => resolve_scope_mappings(child, cx).iter().cloned().collect(),
        OutputPlan::SetOperation {
            left,
            right,
            recursive,
        } => {
            let unknown_reasons =
                OutputModels::new(graph, cx.catalog, &mut cx.mapping_cache.models)
                    .scope(scope)
                    .unknown_reasons
                    .clone();
            let left_mappings = resolve_scope_mappings(left, cx)
                .iter()
                .cloned()
                .collect::<Vec<_>>();
            let right_mappings = if recursive {
                Vec::new()
            } else {
                resolve_scope_mappings(right, cx).iter().cloned().collect()
            };
            let mut merged = if !recursive
                && (mappings_have_unknown_shape(&left_mappings)
                    || mappings_have_unknown_shape(&right_mappings)
                    || !unknown_reasons.is_empty())
            {
                merge_unknown_shape_mappings(left_mappings, right_mappings)
            } else if !recursive {
                merge_proven_positions(left_mappings, &right_mappings)
            } else {
                // The branch resolvers expand stars independently. Preserve the
                // left branch's names and order, as SQL set operations do.
                let mut merged = Vec::with_capacity(left_mappings.len());
                for (idx, left_mapping) in left_mappings.into_iter().enumerate() {
                    if let Some(right_mapping) = right_mappings.get(idx) {
                        let (sources, transform) =
                            merge_branch_sources(&left_mapping, right_mapping);
                        if recursive {
                            merged.push(ColumnMapping {
                                star_position: crate::types::StarPosition::Inferred,
                                ordinal: left_mapping.ordinal,
                                output_name: left_mapping.output_name.clone(),
                                target: left_mapping.target,
                                sources: vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }],
                                transform,
                            });
                        } else {
                            merged.push(ColumnMapping {
                                star_position: crate::types::StarPosition::Inferred,
                                ordinal: left_mapping.ordinal,
                                output_name: left_mapping.output_name.clone(),
                                target: left_mapping.target,
                                sources,
                                transform,
                            });
                        }
                    } else {
                        let sources = left_mapping.sources;
                        let transform = left_mapping.transform;
                        if recursive {
                            merged.push(ColumnMapping {
                                star_position: crate::types::StarPosition::Inferred,
                                ordinal: left_mapping.ordinal,
                                output_name: left_mapping.output_name.clone(),
                                target: left_mapping.target,
                                sources: vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }],
                                transform,
                            });
                        } else {
                            merged.push(ColumnMapping {
                                star_position: crate::types::StarPosition::Inferred,
                                ordinal: left_mapping.ordinal,
                                output_name: left_mapping.output_name.clone(),
                                target: left_mapping.target,
                                sources,
                                transform,
                            });
                        }
                    }
                }
                merged
            };
            // Attach every unknown-width source to each mapping produced by
            // this set operation, including mappings retained by an early
            // unknown-star merge and recursive set operations.
            for mapping in &mut merged {
                append_unsupported_reasons(&mut mapping.sources, &unknown_reasons);
            }
            merged
        }
    };
    let mappings: Arc<[ColumnMapping]> = mappings.into();
    cx.leave(EvalKey::Mappings(scope));
    cx.mapping_cache.entries.insert(scope, mappings.clone());
    mappings
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

pub(super) fn mappings_have_unknown_shape(mappings: &[ColumnMapping]) -> bool {
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

pub(super) fn wildcard_sources(mappings: &[ColumnMapping]) -> Vec<ColumnOrigin> {
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

pub(super) fn merge_transform(left: &TransformKind, right: &TransformKind) -> TransformKind {
    if matches!(left, TransformKind::Aggregation) || matches!(right, TransformKind::Aggregation) {
        TransformKind::Aggregation
    } else if matches!(left, TransformKind::Conditional)
        || matches!(right, TransformKind::Conditional)
    {
        TransformKind::Conditional
    } else if matches!(left, TransformKind::Expression)
        || matches!(right, TransformKind::Expression)
    {
        TransformKind::Expression
    } else if matches!(left, TransformKind::Window) || matches!(right, TransformKind::Window) {
        TransformKind::Window
    } else if matches!(left, TransformKind::Unknown) || matches!(right, TransformKind::Unknown) {
        TransformKind::Unknown
    } else {
        TransformKind::Direct
    }
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
                }];
                assert_eq!(mappings_have_unknown_shape(&mappings), !quoted);
                assert_eq!(wildcard_sources(&mappings).is_empty(), quoted);
            }
        }
    }
}
