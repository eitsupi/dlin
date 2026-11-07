use std::collections::HashSet;

use super::binding::{
    find_cte_redirect, find_unqualified_binding, find_virtual_sources_for_column,
    lookup_virtual_binding, resolve_merged_column, resolve_node,
};
use super::mapping::{
    ScopeMappingCache, known_empty_dependency, merge_transform, resolve_scope_mappings,
};
use super::star::{Unlisted, unlisted_name_sources};

use crate::Ident;
use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::edge::EdgeKind;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, OutputPlan, VirtualColumnState, VirtualSourceId};
use crate::types::{ColumnOrigin, TransformKind};

/// Collect source origins for one logical output slot, retaining both sides
/// of a set operation. Unlike the public mapping path this returns origins so
/// a later CTE/derived-table reference can continue through the slot.
fn scope_column_sources(
    scope: usize,
    index: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
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
    catalog: Option<&CatalogContext<'_>>,
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
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) -> (Vec<ColumnOrigin>, bool, TransformKind) {
    if let Some(origin) = super::binding::ambiguous_qualified_origin(node_id, graph) {
        return (vec![origin], false, TransformKind::Direct);
    }
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
    catalog: Option<&CatalogContext<'_>>,
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
    catalog: Option<&CatalogContext<'_>>,
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
    catalog: Option<&CatalogContext<'_>>,
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
    catalog: Option<&CatalogContext<'_>>,
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
    match unlisted_name_sources(
        target_scope,
        name,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    ) {
        Unlisted::Denied => Some((
            vec![ColumnOrigin::Ambiguous {
                column: name.clone(),
                candidates: Vec::new(),
            }],
            false,
            TransformKind::Direct,
        )),
        Unlisted::Sources(sources) => Some((sources, false, TransformKind::Direct)),
        Unlisted::None => None,
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
