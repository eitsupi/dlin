use std::collections::HashSet;

use super::arity::same_column_name;
use super::binding::{
    derive_transform, find_cte_redirect, find_unqualified_binding, find_virtual_sources_for_column,
    lookup_virtual_binding, resolve_merged_column, resolve_node,
};
use super::mapping::{
    ScopeMappingCache, known_empty_dependency, merge_transform, resolve_scope_mappings,
};
use super::star::{
    StarNameDecision, expand_star, scope_star_name_decision, star_name_decision,
    table_matches_qualifier,
};

use crate::Ident;
use crate::graph::RawGraph;
use crate::graph::edge::EdgeKind;
use crate::graph::node::{NodeId, RawNode, StarBase, StarColumnName, StarOptions};
use crate::graph::scope::{Binding, OutputPlan, VirtualColumnState, VirtualSourceId};
use crate::types::{
    CatalogProvider, ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind,
};

/// Recursively expand a scope's output columns into `ColumnMapping`s.
#[allow(clippy::too_many_arguments)]
pub(super) fn expand_scope_columns(
    scope_id: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
    visited_scopes: &mut HashSet<usize>,
) {
    if !visited_scopes.insert(scope_id) {
        return;
    }
    if !matches!(graph.scopes.output_plan(scope_id), OutputPlan::Projection) {
        let nested =
            resolve_scope_mappings(scope_id, graph, resolved, incoming, catalog, mapping_cache);
        mappings.extend(nested.iter().cloned());
        return;
    }
    for col in graph.scopes.output_columns(scope_id) {
        if let RawNode::Star {
            base,
            options,
            scope,
        } = &graph.nodes[col.node_id]
        {
            expand_star(
                base,
                options,
                *scope,
                graph,
                resolved,
                incoming,
                catalog,
                mapping_cache,
                mappings,
                visited_scopes,
            );
        } else {
            let mut visited = HashSet::new();
            let (sources, edge_kinds, _, inherited_transform) = collect_output_sources(
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
                    column: col.name.clone(),
                },
                sources,
                transform,
            });
        }
    }
}

/// Collect source origins for one logical output slot, retaining both sides
/// of a set operation. Unlike the public mapping path this returns origins so
/// a later CTE/derived-table reference can continue through the slot.
fn scope_column_sources(
    scope: usize,
    index: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> (Vec<ColumnOrigin>, Vec<EdgeKind>, bool, TransformKind) {
    let mappings = resolve_scope_mappings(scope, graph, resolved, incoming, catalog, mapping_cache);
    let Some(mapping) = mappings.get(index) else {
        return (vec![], vec![], false, TransformKind::Direct);
    };
    let mut sources = Vec::new();
    let mut has_back = false;
    for source in &mapping.sources {
        match source {
            ColumnOrigin::Recursive { base_sources } => {
                sources.extend(base_sources.clone());
                has_back = true;
            }
            source => sources.push(source.clone()),
        }
    }
    (sources, vec![], has_back, mapping.transform.clone())
}

pub(super) fn collect_output_sources(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> (Vec<ColumnOrigin>, Vec<EdgeKind>, bool, TransformKind) {
    if !visited.insert(node_id) {
        return (vec![], vec![], false, TransformKind::Direct);
    }

    let mut sources = Vec::new();
    let mut kinds = Vec::new();
    let mut has_back = false;
    let mut inherited_transform = TransformKind::Direct;

    for &edge_idx in &incoming[node_id] {
        let edge = &graph.edges[edge_idx];
        if edge.is_recursive_back_edge {
            has_back = true;
            continue;
        }
        let (sub_sources, sub_back, sub_transform) = collect_leaf_origins(
            edge.from,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        );
        for _ in &sub_sources {
            kinds.push(edge.kind.clone());
        }
        sources.extend(sub_sources);
        has_back |= sub_back;
        inherited_transform = merge_transform(&inherited_transform, &sub_transform);
    }

    (sources, kinds, has_back, inherited_transform)
}

pub(super) fn collect_leaf_origins(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> (Vec<ColumnOrigin>, bool, TransformKind) {
    if let Some(result) =
        resolve_virtual_reference(node_id, graph, resolved, incoming, catalog, mapping_cache)
    {
        return result;
    }

    if let Some(result) =
        resolve_merged_column(node_id, graph, resolved, incoming, catalog, mapping_cache)
    {
        return result;
    }

    if let Some((sources, has_back, transform)) =
        resolve_named_scope_reference(node_id, graph, resolved, incoming, catalog, mapping_cache)
    {
        return (sources, has_back, transform);
    }

    if let Some((target_output, scope)) = find_cte_redirect(node_id, graph, catalog) {
        if let Some(index) = graph
            .scopes
            .output_columns(scope)
            .iter()
            .position(|c| c.node_id == target_output)
            && !matches!(graph.scopes.output_plan(scope), OutputPlan::Projection)
        {
            let (sources, _, has_back, transform) = scope_column_sources(
                scope,
                index,
                graph,
                resolved,
                incoming,
                catalog,
                mapping_cache,
            );
            return (sources, has_back, transform);
        }
        let (sources, _, has_back, transform) = collect_output_sources(
            target_output,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        );
        return (sources, has_back, transform);
    }

    if let RawNode::Output { .. } = &graph.nodes[node_id] {
        let (sources, _, has_back, transform) = collect_output_sources(
            node_id,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        );
        (sources, has_back, transform)
    } else {
        let origin = resolve_node(
            node_id,
            graph,
            resolved,
            incoming,
            visited,
            catalog,
            mapping_cache,
        );
        match origin {
            Some(o) => (vec![o], false, TransformKind::Direct),
            None => (vec![], false, TransformKind::Direct),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_virtual_reference(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let (name, binding, scope) = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            scope,
            binding,
        } => (
            name,
            binding
                .clone()
                .or_else(|| {
                    qualifier.as_deref().and_then(|parts| match parts {
                        [qualifier] => graph.scopes.lookup(*scope, qualifier).cloned(),
                        _ => None,
                    })
                })
                .or_else(|| lookup_virtual_binding(*scope, name, graph)),
            *scope,
        ),
        RawNode::Unqualified {
            name,
            scope,
            binding,
        } => (
            name,
            binding
                .clone()
                .or_else(|| lookup_virtual_binding(*scope, name, graph)),
            *scope,
        ),
        _ => return None,
    };
    let source = match binding {
        Some(Binding::VirtualSource(source)) => source,
        Some(_) => return None,
        None => match find_virtual_sources_for_column(scope, name, graph).as_slice() {
            [source] => *source,
            [] => return None,
            _ => {
                return Some((
                    vec![ColumnOrigin::Ambiguous {
                        column: name.clone(),
                        candidates: Vec::new(),
                    }],
                    false,
                    TransformKind::Direct,
                ));
            }
        },
    };
    resolve_virtual_column_sources(
        name,
        source,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_virtual_column_sources(
    name: &Ident,
    source: VirtualSourceId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let column = graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .find(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))?;
    let mut origins = Vec::new();
    for &dependency in &column.dependencies {
        let (dependency_origins, _, _) = collect_leaf_origins(
            dependency,
            graph,
            resolved,
            incoming,
            &mut HashSet::new(),
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
    Some((origins, false, TransformKind::Direct))
}

pub(super) fn virtual_column_origin(
    name: &Ident,
    source: VirtualSourceId,
    graph: &RawGraph,
) -> Option<ColumnOrigin> {
    let column = graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .find(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))?;
    match column.state {
        VirtualColumnState::KnownEmpty => None,
        VirtualColumnState::Unknown => Some(ColumnOrigin::Ambiguous {
            column: column.name.clone(),
            candidates: Vec::new(),
        }),
    }
}

/// Resolve a named reference through a set-operation/Delegate scope using the
/// expanded mappings. Raw scope columns intentionally do not contain names
/// for individual catalog-expanded star outputs, so lookup must happen here.
fn resolve_named_scope_reference(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let (name, qualifier, scope) = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            scope,
            ..
        } => (name, qualifier.as_ref(), *scope),
        RawNode::Unqualified { name, scope, .. } => (name, None, *scope),
        _ => return None,
    };
    let binding = match &graph.nodes[node_id] {
        RawNode::Ref {
            binding: Some(binding),
            ..
        }
        | RawNode::Unqualified {
            binding: Some(binding),
            ..
        } => Some(binding.clone()),
        _ => None,
    }
    .or_else(|| {
        qualifier.and_then(|parts| match parts.as_slice() {
            [qualifier] => graph.scopes.lookup(scope, qualifier).cloned(),
            _ => None,
        })
    })
    .or_else(|| find_unqualified_binding(name, scope, graph, catalog));
    let Some(Binding::Cte(target_scope) | Binding::DerivedTable(target_scope)) = binding else {
        return None;
    };
    named_scope_sources(
        name,
        target_scope,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    )
}

/// The origins of the column `name` published by the CTE or derived table
/// `target_scope`, with the transform that scope applies to it.
#[allow(clippy::too_many_arguments)]
pub(super) fn named_scope_sources(
    name: &Ident,
    target_scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let mappings = resolve_scope_mappings(
        target_scope,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    );
    if let Some(mapping) = mappings.iter().find(|mapping| {
        mapping.target.column.column_key(graph.dialect) == name.column_key(graph.dialect)
    }) {
        let (sources, has_back) = flatten_mapping_sources(&mapping.sources);
        return Some((sources, has_back, mapping.transform.clone()));
    }
    let requested_name =
        match scope_star_name_decision(target_scope, name, graph, &mut HashSet::new()) {
            StarNameDecision::Denied | StarNameDecision::Ambiguous => {
                return Some((
                    vec![ColumnOrigin::Ambiguous {
                        column: name.clone(),
                        candidates: Vec::new(),
                    }],
                    false,
                    TransformKind::Direct,
                ));
            }
            StarNameDecision::Replaced(node_id) => {
                let (sources, _, has_back, transform) = collect_output_sources(
                    node_id,
                    graph,
                    resolved,
                    incoming,
                    &mut HashSet::new(),
                    catalog,
                    mapping_cache,
                );
                return Some((sources, has_back, transform));
            }
            StarNameDecision::Renamed(old) => old,
            StarNameDecision::Allowed => name.clone(),
        };
    let wildcard_sources = named_wildcard_sources_for_scope(
        target_scope,
        &requested_name,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    );
    (!wildcard_sources.is_empty()).then_some((wildcard_sources, false, TransformKind::Direct))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn named_wildcard_sources_for_scope(
    scope: usize,
    column: &Ident,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Vec<ColumnOrigin> {
    match graph.scopes.output_plan(scope) {
        OutputPlan::Projection => {
            let mut sources = Vec::new();
            for output in graph.scopes.output_columns(scope) {
                let RawNode::Star {
                    base,
                    options,
                    scope: star_scope,
                } = &graph.nodes[output.node_id]
                else {
                    continue;
                };
                let source_column =
                    match star_name_decision(options, column, Some(base), graph.dialect) {
                        StarNameDecision::Allowed => column.clone(),
                        StarNameDecision::Renamed(old) => old,
                        StarNameDecision::Denied
                        | StarNameDecision::Replaced(_)
                        | StarNameDecision::Ambiguous => continue,
                    };
                let mut expanded = Vec::new();
                expand_star(
                    base,
                    &StarOptions::default(),
                    *star_scope,
                    graph,
                    resolved,
                    incoming,
                    catalog,
                    mapping_cache,
                    &mut expanded,
                    &mut HashSet::new(),
                );
                sources.extend(named_wildcard_sources_for_star(
                    &expanded,
                    &source_column,
                    options,
                    base,
                    *star_scope,
                    graph,
                ));
            }
            sources
        }
        OutputPlan::Delegate(child) => named_wildcard_sources_for_scope(
            *child,
            column,
            graph,
            resolved,
            incoming,
            catalog,
            mapping_cache,
        ),
        // Set-operation output names and modifiers come from the left branch,
        // but lineage provenance is collected from every branch. A denied
        // right branch remains an explicit uncertainty marker because its
        // unknown-width slot cannot be aligned safely.
        OutputPlan::SetOperation { left, right, .. } => {
            let mut sources = named_wildcard_sources_for_scope(
                *left,
                column,
                graph,
                resolved,
                incoming,
                catalog,
                mapping_cache,
            );
            let right_sources = named_wildcard_sources_for_scope(
                *right,
                column,
                graph,
                resolved,
                incoming,
                catalog,
                mapping_cache,
            );
            if right_sources.is_empty()
                && matches!(
                    scope_star_name_decision(*right, column, graph, &mut HashSet::new()),
                    StarNameDecision::Denied | StarNameDecision::Ambiguous
                )
            {
                sources.push(ColumnOrigin::Ambiguous {
                    column: column.clone(),
                    candidates: Vec::new(),
                });
            } else {
                sources.extend(right_sources);
            }
            sources
        }
    }
}

pub(super) fn named_wildcard_sources_for_star(
    mappings: &[ColumnMapping],
    column: &Ident,
    options: &StarOptions,
    base: &StarBase,
    scope: usize,
    graph: &RawGraph,
) -> Vec<ColumnOrigin> {
    let mut sources = Vec::new();
    let mut wildcard_seen = false;
    let mut denied = false;
    for mapping in mappings {
        for source in &mapping.sources {
            match source {
                ColumnOrigin::Wildcard { table } => {
                    wildcard_seen = true;
                    if options.exclude.iter().any(|excluded| {
                        excluded_matches_source(excluded, column, source, base, scope, graph)
                    }) {
                        denied = true;
                    } else {
                        sources.push(ColumnOrigin::NamedWildcard {
                            table: table.clone(),
                            column: column.clone(),
                        });
                    }
                }
                ColumnOrigin::Recursive { base_sources } => {
                    for nested in base_sources {
                        if let ColumnOrigin::Wildcard { table } = nested {
                            wildcard_seen = true;
                            if options.exclude.iter().any(|excluded| {
                                excluded_matches_source(
                                    excluded, column, nested, base, scope, graph,
                                )
                            }) {
                                denied = true;
                            } else {
                                sources.push(ColumnOrigin::NamedWildcard {
                                    table: table.clone(),
                                    column: column.clone(),
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if sources.is_empty() && wildcard_seen && denied {
        sources.push(ColumnOrigin::Ambiguous {
            column: column.clone(),
            candidates: Vec::new(),
        });
    }
    sources
}

fn excluded_matches_source(
    excluded: &StarColumnName,
    column: &Ident,
    source: &ColumnOrigin,
    base: &StarBase,
    scope: usize,
    graph: &RawGraph,
) -> bool {
    let Some((excluded_column, qualifier)) = excluded.parts.split_last() else {
        return false;
    };
    if !same_column_name(column, excluded_column, graph.dialect) {
        return false;
    }
    if qualifier.is_empty() {
        return true;
    }
    if let StarBase::Qualified(parts) = base
        && parts.len() == qualifier.len()
        && parts
            .iter()
            .zip(qualifier)
            .all(|(a, b)| a.relation_key(graph.dialect) == b.relation_key(graph.dialect))
    {
        return true;
    }
    if qualifier.len() == 1
        && relation_from_origin(source).is_some_and(|table| {
            graph.scopes.visible_bindings(scope).iter().any(|(name, binding)| {
                name.relation_key(graph.dialect) == qualifier[0].relation_key(graph.dialect)
                    && matches!(binding, Binding::Table(binding_table) if binding_table == table)
            })
        })
    {
        return true;
    }
    relation_from_origin(source)
        .is_some_and(|table| table_matches_qualifier(table, qualifier, graph.dialect))
}

fn relation_from_origin(source: &ColumnOrigin) -> Option<&TableRef> {
    match source {
        ColumnOrigin::Wildcard { table }
        | ColumnOrigin::Concrete { table, .. }
        | ColumnOrigin::NamedWildcard { table, .. } => Some(table),
        _ => None,
    }
}

pub(super) fn flatten_mapping_sources(sources: &[ColumnOrigin]) -> (Vec<ColumnOrigin>, bool) {
    let mut flattened = Vec::new();
    let mut has_back = false;
    for source in sources {
        match source {
            ColumnOrigin::Recursive { base_sources } => {
                flattened.extend(base_sources.clone());
                has_back = true;
            }
            source => flattened.push(source.clone()),
        }
    }
    (flattened, has_back)
}
