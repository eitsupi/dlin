use std::collections::HashSet;

use super::lineage::{collect_output_sources, virtual_column_origin};
use super::mapping::ScopeMappingCache;
use super::model::InputLookup;
use super::star::{input_name_sources, lookup_input, named_scope_sources};
use super::target::effective_bindings;

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::edge::EdgeKind;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, VirtualSourceId};
use crate::types::{ColumnOrigin, TransformKind};
use crate::{Ident, Unresolved};

pub(super) fn resolve_node(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    if let Some(origin) = ambiguous_qualified_origin(node_id, graph) {
        return Some(origin);
    }
    if let Some(ref origin) = resolved[node_id] {
        return Some(origin.clone());
    }

    let origin = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            scope,
            binding,
        } => {
            let binding = binding.clone().or_else(|| {
                qualifier.as_deref().and_then(|parts| match parts {
                    [qualifier] => graph.scopes.lookup(*scope, qualifier).cloned(),
                    _ => None,
                })
            });
            if let Some(binding) = binding {
                resolve_captured_binding(
                    name,
                    binding,
                    graph,
                    resolved,
                    incoming,
                    visited,
                    catalog,
                    mapping_cache,
                )
            } else if qualifier.is_some() {
                // The qualifier names no relation in scope. Building a table
                // out of its text would present a guess as a confident
                // source, so the reference stays unresolved.
                Some(ColumnOrigin::Ambiguous {
                    column: name.clone(),
                    candidates: Vec::new(),
                })
            } else {
                resolve_unqualified(
                    name,
                    *scope,
                    graph,
                    resolved,
                    incoming,
                    visited,
                    catalog,
                    mapping_cache,
                )
            }
        }

        RawNode::Unqualified {
            name,
            scope,
            binding,
        } => {
            if let Some(binding) = binding
                .clone()
                .or_else(|| lookup_virtual_binding(*scope, name, graph))
            {
                resolve_captured_binding(
                    name,
                    binding,
                    graph,
                    resolved,
                    incoming,
                    visited,
                    catalog,
                    mapping_cache,
                )
            } else {
                resolve_unqualified(
                    name,
                    *scope,
                    graph,
                    resolved,
                    incoming,
                    visited,
                    catalog,
                    mapping_cache,
                )
            }
        }

        RawNode::RowValueCandidate {
            name,
            scope,
            binding,
        } => resolve_row_value_candidate(
            name,
            *scope,
            binding.clone(),
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        ),

        RawNode::Star { .. } => None,

        RawNode::Output { .. } => None,
    };

    resolved[node_id].clone_from(&origin);
    origin
}

pub(super) fn ambiguous_qualified_origin(node: NodeId, graph: &RawGraph) -> Option<ColumnOrigin> {
    let RawNode::Ref {
        name,
        qualifier: Some(parts),
        scope,
        ..
    } = &graph.nodes[node]
    else {
        return None;
    };
    let [qualifier] = parts.as_slice() else {
        return None;
    };
    graph
        .scopes
        .ambiguous_binding_candidates(*scope, qualifier)
        .map(|candidates| {
            ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                column: name.clone(),
                candidates,
            })
        })
}

/// Virtual source aliases stand in for output columns in expressions such as
/// `UNNEST(...) AS item`. Compare them with column rules, since SQL engines
/// apply column-name case behavior to those aliases.
pub(super) fn lookup_virtual_binding(
    scope: usize,
    name: &Ident,
    graph: &RawGraph,
) -> Option<Binding> {
    graph
        .scopes
        .visible_bindings(scope)
        .into_iter()
        .find(|(binding_name, binding)| {
            matches!(binding, Binding::VirtualSource(_))
                && binding_name.column_key(graph.dialect) == name.column_key(graph.dialect)
        })
        .map(|(_, binding)| binding)
}

#[allow(clippy::too_many_arguments)]
fn resolve_row_value_candidate(
    name: &Ident,
    scope: usize,
    binding: Option<Binding>,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    let binding = binding.or_else(|| graph.scopes.lookup(scope, name).cloned());
    let Some(binding) = binding else {
        return Some(ColumnOrigin::Unresolved(Unresolved::RowValue {
            alias: name.clone(),
        }));
    };

    match binding {
        Binding::Table(table) => {
            if let Some(owner) = catalog
                .and_then(|catalog| catalog.resolve_column(name, std::slice::from_ref(&table)))
            {
                Some(ColumnOrigin::Concrete {
                    table: owner,
                    column: name.clone(),
                })
            } else {
                Some(ColumnOrigin::Unresolved(Unresolved::RowValue {
                    alias: name.clone(),
                }))
            }
        }
        Binding::Cte(target_scope) | Binding::DerivedTable(target_scope) => {
            let is_named_column = graph
                .scopes
                .output_columns(target_scope)
                .iter()
                .any(|column| {
                    column.name.column_key(graph.dialect) == name.column_key(graph.dialect)
                });
            if is_named_column {
                resolve_through_scope(
                    name,
                    target_scope,
                    graph,
                    resolved,
                    incoming,
                    visited,
                    catalog,
                    mapping_cache,
                )
            } else {
                Some(ColumnOrigin::Unresolved(Unresolved::RowValue {
                    alias: name.clone(),
                }))
            }
        }
        Binding::VirtualSource(_) => Some(ColumnOrigin::Unresolved(Unresolved::RowValue {
            alias: name.clone(),
        })),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_captured_binding(
    name: &Ident,
    binding: Binding,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    match binding {
        Binding::Table(table) => Some(ColumnOrigin::Concrete {
            table,
            column: name.clone(),
        }),
        Binding::Cte(scope) | Binding::DerivedTable(scope) => resolve_through_scope(
            name,
            scope,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        ),
        Binding::VirtualSource(source) => virtual_column_origin(name, source, graph),
    }
}

pub(super) fn find_cte_redirect(
    node_id: NodeId,
    graph: &RawGraph,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<(NodeId, usize)> {
    match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            scope,
            binding,
        } => {
            let binding = binding
                .clone()
                .or_else(|| {
                    qualifier.as_deref().and_then(|parts| match parts {
                        [qualifier] => graph.scopes.lookup(*scope, qualifier).cloned(),
                        _ => None,
                    })
                })
                .or_else(|| find_unqualified_binding(name, *scope, graph, catalog, mapping_cache));
            match binding {
                Some(Binding::Cte(s) | Binding::DerivedTable(s)) => graph
                    .scopes
                    .output_columns(s)
                    .iter()
                    .find(|c| c.name.column_key(graph.dialect) == name.column_key(graph.dialect))
                    .map(|c| (c.node_id, s)),
                _ => None,
            }
        }
        RawNode::Unqualified {
            name,
            scope,
            binding,
        } => {
            let binding = binding
                .clone()
                .or_else(|| find_unqualified_binding(name, *scope, graph, catalog, mapping_cache));
            match binding {
                Some(Binding::Cte(s) | Binding::DerivedTable(s)) => graph
                    .scopes
                    .output_columns(s)
                    .iter()
                    .find(|c| c.name.column_key(graph.dialect) == name.column_key(graph.dialect))
                    .map(|c| (c.node_id, s)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The CTE or derived table an unqualified column reference is read from, when
/// the folded FROM clause settles on a single one.
pub(super) fn find_unqualified_binding(
    name: &Ident,
    scope: usize,
    graph: &RawGraph,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<Binding> {
    lookup_input(scope, name, graph, catalog, mapping_cache).binding()
}

/// Resolve an unqualified column that no range variable names, from every
/// relation the FROM clause lets supply it. A column that `USING`/`NATURAL`
/// joins merge keeps all the sides that supply its value.
/// Returns `None` for any other reference.
pub(super) fn resolve_input_reference(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let (name, scope) = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier: None,
            scope,
            binding: None,
        }
        | RawNode::Unqualified {
            name,
            scope,
            binding: None,
        } => (name, *scope),
        _ => return None,
    };
    input_name_sources(
        scope,
        name,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    )
}

/// The origin of an unqualified column that `resolve_input_reference` could not
/// answer: the one CTE or derived table that decides the name publishes nothing
/// under it, so the name falls back to the scope's raw output columns, or a
/// virtual source that does not publish columns by name.
///
/// Every other answer of the input lookup (a column, several merged sources, an
/// ambiguity, a physical relation) is returned in full by
/// `resolve_input_reference` before a node is resolved on its own, so no merged
/// source can reach here to be reduced to its first.
#[allow(clippy::too_many_arguments)]
fn resolve_unqualified(
    name: &Ident,
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    match lookup_input(scope, name, graph, catalog, mapping_cache) {
        InputLookup::Scope { scope: target, .. } => resolve_through_scope(
            name,
            target,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        ),
        lookup => {
            debug_assert!(
                matches!(lookup, InputLookup::NoSource),
                "an answered lookup never reaches node resolution: {lookup:?}"
            );
            None
        }
    }
}

fn virtual_has_column(name: &Ident, source: VirtualSourceId, graph: &RawGraph) -> bool {
    graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .any(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))
}

pub(super) fn find_virtual_sources_for_column(
    scope: usize,
    name: &Ident,
    graph: &RawGraph,
) -> Vec<VirtualSourceId> {
    effective_bindings(scope, graph)
        .into_iter()
        .filter_map(|(_, binding)| match binding {
            Binding::VirtualSource(source) if virtual_has_column(name, source, graph) => {
                Some(source)
            }
            _ => None,
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn resolve_through_scope(
    column_name: &Ident,
    target_scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    // Resolve through the scope's model, which also lists the names a
    // catalog-expanded star introduced.
    if let Some((origins, has_back, _)) = named_scope_sources(
        column_name,
        target_scope,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    ) {
        return if has_back {
            Some(ColumnOrigin::Recursive {
                base_sources: origins,
            })
        } else {
            origins.into_iter().next()
        };
    }

    if let Some(col) = graph
        .scopes
        .output_columns(target_scope)
        .iter()
        .find(|c| c.name.column_key(graph.dialect) == column_name.column_key(graph.dialect))
    {
        let (origins, _, has_back, _) = collect_output_sources(
            col.node_id,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        );
        if has_back {
            Some(ColumnOrigin::Recursive {
                base_sources: origins,
            })
        } else if origins.len() == 1 {
            Some(origins.into_iter().next().unwrap())
        } else {
            // Multi-source CTE output (e.g., UNION inside CTE). Returns the first
            // origin here; the full list is collected transitively by
            // collect_output_sources when building the final ColumnMapping.
            origins.into_iter().next()
        }
    } else {
        Some(ColumnOrigin::Ambiguous {
            column: column_name.clone(),
            candidates: Vec::new(),
        })
    }
}

pub(super) fn derive_transform(node: &RawNode, edge_kinds: &[EdgeKind]) -> TransformKind {
    let kinds = if edge_kinds.is_empty() {
        match node {
            RawNode::Output { intrinsic_kind, .. } => std::slice::from_ref(intrinsic_kind),
            _ => edge_kinds,
        }
    } else {
        edge_kinds
    };

    if kinds.iter().any(|k| matches!(k, EdgeKind::ViaAggregation)) {
        TransformKind::Aggregation
    } else if kinds.iter().any(|k| matches!(k, EdgeKind::ViaConditional)) {
        TransformKind::Conditional
    } else if kinds.iter().any(|k| matches!(k, EdgeKind::ViaExpression)) {
        TransformKind::Expression
    } else {
        TransformKind::Direct
    }
}
