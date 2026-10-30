#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::binding::derive_transform;
use super::catalog;
use super::lineage::{collect_leaf_origins, collect_output_sources};
use super::star::expand_star;

use crate::Ident;
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, OutputPlan, VirtualColumnState, VirtualSourceId};
use crate::types::{
    CatalogProvider, ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind,
};

pub(super) fn wildcard_mapping(
    output_table: Option<&TableRef>,
    source_table: TableRef,
) -> ColumnMapping {
    ColumnMapping {
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

#[allow(clippy::too_many_arguments)]
pub(super) fn expand_virtual_source(
    source: VirtualSourceId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
) {
    for column in &graph.scopes.virtual_source(source).columns {
        let mut origins = Vec::new();
        let mut visited = HashSet::new();
        for &dependency in &column.dependencies {
            let (dependency_origins, _, _) = collect_leaf_origins(
                dependency,
                graph,
                resolved,
                incoming,
                &mut visited,
                catalog,
                mapping_cache,
            );
            origins.extend(dependency_origins);
        }
        if origins.is_empty()
            && matches!(column.state, VirtualColumnState::Unknown)
            && !column
                .dependencies
                .iter()
                .all(|&dependency| known_empty_dependency(dependency, graph))
        {
            origins.push(ColumnOrigin::Ambiguous {
                column: column.name.clone(),
                candidates: Vec::new(),
            });
        }
        mappings.push(ColumnMapping {
            target: ColumnRef {
                table: None,
                column: column.name.clone(),
            },
            sources: origins,
            transform: TransformKind::Direct,
        });
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
}

enum ScopeMappingEntry {
    Computing,
    Resolved(Arc<[ColumnMapping]>),
}

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
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_scope_mappings(
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Arc<[ColumnMapping]> {
    if let Some(entry) = mapping_cache.entries.get(&scope) {
        return match entry {
            ScopeMappingEntry::Resolved(mappings) => mappings.clone(),
            // A recursive scope cannot safely publish a partially materialized
            // result. Returning no mappings preserves the existing fallback to
            // raw output resolution and, importantly, does not cache an
            // incomplete entry as resolved.
            ScopeMappingEntry::Computing => Arc::from([]),
        };
    }
    mapping_cache
        .entries
        .insert(scope, ScopeMappingEntry::Computing);
    #[cfg(test)]
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(computations.get() + 1));

    let mappings = match graph.scopes.output_plan(scope).clone() {
        OutputPlan::Projection => {
            let mut mappings = Vec::new();
            for col in graph.scopes.output_columns(scope) {
                match &graph.nodes[col.node_id] {
                    RawNode::Output { name, .. } => {
                        let mut visited = HashSet::new();
                        let (sources, edge_kinds, has_back, inherited_transform) =
                            collect_output_sources(
                                col.node_id,
                                graph,
                                resolved,
                                incoming,
                                &mut visited,
                                catalog,
                                mapping_cache,
                            );
                        let transform = merge_transform(
                            &derive_transform(&graph.nodes[col.node_id], &edge_kinds),
                            &inherited_transform,
                        );
                        mappings.push(ColumnMapping {
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
                    }
                    RawNode::Star {
                        base,
                        options,
                        scope,
                    } => expand_star(
                        base,
                        options,
                        *scope,
                        graph,
                        resolved,
                        incoming,
                        catalog,
                        mapping_cache,
                        &mut mappings,
                        &mut HashSet::new(),
                    ),
                    _ => {}
                }
            }
            if let Some(cat) = catalog {
                catalog::apply_catalog(&mut mappings, cat);
            }
            mappings
        }
        OutputPlan::Delegate(child) => {
            resolve_scope_mappings(child, graph, resolved, incoming, catalog, mapping_cache)
                .iter()
                .cloned()
                .collect()
        }
        OutputPlan::SetOperation {
            left,
            right,
            recursive,
        } => {
            let left_mappings =
                resolve_scope_mappings(left, graph, resolved, incoming, catalog, mapping_cache)
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
            let right_mappings = if recursive {
                Vec::new()
            } else {
                resolve_scope_mappings(right, graph, resolved, incoming, catalog, mapping_cache)
                    .iter()
                    .cloned()
                    .collect()
            };
            // A wildcard without catalog metadata is a variable-width slot.
            // Positional alignment at or after it would fabricate an ordinal
            // and lose the remaining branch columns. Merge only the exact
            // prefix before the first wildcard, then retain both tails and
            // expose the Wildcard origin.
            if !recursive
                && (mappings_have_unknown_shape(&left_mappings)
                    || mappings_have_unknown_shape(&right_mappings))
            {
                merge_unknown_shape_mappings(left_mappings, right_mappings)
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
                                target: left_mapping.target,
                                sources: vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }],
                                transform,
                            });
                        } else {
                            merged.push(ColumnMapping {
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
                                target: left_mapping.target,
                                sources: vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }],
                                transform,
                            });
                        } else {
                            merged.push(ColumnMapping {
                                target: left_mapping.target,
                                sources,
                                transform,
                            });
                        }
                    }
                }
                merged
            }
        }
    };
    let mappings: Arc<[ColumnMapping]> = mappings.into();
    mapping_cache
        .entries
        .insert(scope, ScopeMappingEntry::Resolved(mappings.clone()));
    mappings
}

pub(super) fn mappings_have_unknown_shape(mappings: &[ColumnMapping]) -> bool {
    mappings.iter().any(|mapping| {
        // A named output that happens to flow through an unresolved star has
        // a known ordinal/name and must not act as a variable-width barrier.
        mapping.target.column.value == "*"
            && mapping.sources.iter().any(|source| match source {
                ColumnOrigin::Wildcard { .. } => true,
                ColumnOrigin::Ambiguous { column, .. } if column.value == "*" => true,
                ColumnOrigin::Recursive { base_sources } => base_sources.iter().any(|source| {
                    matches!(source, ColumnOrigin::Wildcard { .. })
                        || matches!(source, ColumnOrigin::Ambiguous { column, .. } if column.value == "*")
                }),
                _ => false,
            })
    })
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
        return left;
    }

    let prefix_len = left_barrier.min(right_barrier);
    let left_unknown = wildcard_sources(&left);
    let right_unknown = wildcard_sources(&right);
    let mut merged = Vec::with_capacity(left.len() + right.len() - prefix_len);

    for (left_mapping, right_mapping) in left.iter().zip(right.iter()).take(prefix_len) {
        let (sources, transform) = merge_branch_sources(left_mapping, right_mapping);
        merged.push(ColumnMapping {
            target: left_mapping.target.clone(),
            sources,
            transform,
        });
    }
    merged.extend(
        left.into_iter()
            .skip(prefix_len)
            .map(|mapping| append_unknown_sources(mapping, &right_unknown)),
    );
    merged.extend(
        right
            .into_iter()
            .skip(prefix_len)
            .map(|mapping| append_unknown_sources(mapping, &left_unknown)),
    );
    merged
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
                ColumnOrigin::Ambiguous { column, .. } if column.value == "*" => {
                    sources.push(source.clone());
                }
                ColumnOrigin::Recursive { base_sources } => sources.extend(
                    base_sources
                        .iter()
                        .filter(|source| {
                            matches!(source, ColumnOrigin::Wildcard { .. })
                                || matches!(source, ColumnOrigin::Ambiguous { column, .. } if column.value == "*")
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
