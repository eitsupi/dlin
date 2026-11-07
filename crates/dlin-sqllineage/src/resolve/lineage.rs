use std::collections::HashSet;

use super::binding::{
    find_cte_redirect, find_unqualified_binding, find_virtual_sources_for_column,
    lookup_virtual_binding, resolve_input_reference, resolve_node,
};
use super::context::{EvalContext, EvalKey};
use super::mapping::{known_empty_dependency, merge_transform, resolve_scope_mappings};
use super::star::named_scope_sources;

use crate::Ident;
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
    cx: &mut EvalContext<'_, '_>,
) -> (Vec<ColumnOrigin>, Vec<EdgeKind>, bool, TransformKind) {
    let mappings = resolve_scope_mappings(scope, cx);
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
    cx: &mut EvalContext<'_, '_>,
    visited: &mut HashSet<NodeId>,
) -> (Vec<ColumnOrigin>, Vec<EdgeKind>, bool, TransformKind) {
    if !visited.insert(node_id) {
        return (vec![], vec![], false, TransformKind::Direct);
    }
    // Reaching a node that is already being collected, from a walk that does
    // not share `visited`, means its sources depend on themselves.
    let key = EvalKey::Node(node_id);
    if !cx.enter(key) {
        return (
            vec![indeterminate_origin(cx.graph, node_id)],
            vec![],
            false,
            TransformKind::Direct,
        );
    }
    let graph = cx.graph;
    let incoming = cx.incoming;

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
        let (sub_sources, sub_back, sub_transform) = collect_leaf_origins(edge.from, cx, visited);
        for _ in &sub_sources {
            kinds.push(edge.kind.clone());
        }
        sources.extend(sub_sources);
        has_back |= sub_back;
        inherited_transform = merge_transform(&inherited_transform, &sub_transform);
    }

    cx.leave(key);
    (sources, kinds, has_back, inherited_transform)
}

/// The origin of a value whose evaluation depends on itself: nothing can be
/// proven about it, so it is reported as an ambiguous column rather than as a
/// source-free one.
fn indeterminate_origin(graph: &RawGraph, node_id: NodeId) -> ColumnOrigin {
    let column = match &graph.nodes[node_id] {
        RawNode::Output { name, .. }
        | RawNode::Ref { name, .. }
        | RawNode::Unqualified { name, .. }
        | RawNode::RowValueCandidate { name, .. } => name.clone(),
        RawNode::Star { .. } => Ident::unquoted("*"),
    };
    ColumnOrigin::Ambiguous {
        column,
        candidates: Vec::new(),
    }
}

pub(super) fn collect_leaf_origins(
    node_id: NodeId,
    cx: &mut EvalContext<'_, '_>,
    visited: &mut HashSet<NodeId>,
) -> (Vec<ColumnOrigin>, bool, TransformKind) {
    let graph = cx.graph;
    if let Some(origin) = super::binding::ambiguous_qualified_origin(node_id, graph) {
        return (vec![origin], false, TransformKind::Direct);
    }
    if let Some(result) = resolve_virtual_reference(node_id, cx) {
        return result;
    }

    if let Some(result) = resolve_input_reference(node_id, cx) {
        return result;
    }

    if let Some((sources, has_back, transform)) = resolve_named_scope_reference(node_id, cx) {
        return (sources, has_back, transform);
    }

    if let Some((target_output, scope)) = find_cte_redirect(node_id, cx) {
        if let Some(index) = graph
            .scopes
            .output_columns(scope)
            .iter()
            .position(|c| c.node_id == target_output)
            && !matches!(graph.scopes.output_plan(scope), OutputPlan::Projection)
        {
            let (sources, _, has_back, transform) = scope_column_sources(scope, index, cx);
            return (sources, has_back, transform);
        }
        let (sources, _, has_back, transform) = collect_output_sources(target_output, cx, visited);
        return (sources, has_back, transform);
    }

    if let RawNode::Output { .. } = &graph.nodes[node_id] {
        let (sources, _, has_back, transform) = collect_output_sources(node_id, cx, visited);
        (sources, has_back, transform)
    } else {
        let origin = resolve_node(node_id, cx, visited);
        match origin {
            Some(o) => (vec![o], false, TransformKind::Direct),
            None => (vec![], false, TransformKind::Direct),
        }
    }
}

fn resolve_virtual_reference(
    node_id: NodeId,
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let graph = cx.graph;
    let (name, binding, env) = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            env,
            binding,
        } => (
            name,
            binding
                .clone()
                .or_else(|| {
                    qualifier.as_deref().and_then(|parts| match parts {
                        [qualifier] => graph.scopes.lookup(*env, qualifier).cloned(),
                        _ => None,
                    })
                })
                .or_else(|| lookup_virtual_binding(*env, name, graph)),
            *env,
        ),
        RawNode::Unqualified { name, env, binding } => (
            name,
            binding
                .clone()
                .or_else(|| lookup_virtual_binding(*env, name, graph)),
            *env,
        ),
        _ => return None,
    };
    let source = match binding {
        Some(Binding::VirtualSource(source)) => source,
        Some(_) => return None,
        None => match find_virtual_sources_for_column(env, name, graph).as_slice() {
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
    resolve_virtual_column_sources(name, source, cx)
}

fn resolve_virtual_column_sources(
    name: &Ident,
    source: VirtualSourceId,
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let graph = cx.graph;
    let column = graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .find(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))?;
    let mut origins = Vec::new();
    for &dependency in &column.dependencies {
        let (dependency_origins, _, _) = collect_leaf_origins(dependency, cx, &mut HashSet::new());
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
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let graph = cx.graph;
    let (name, qualifier, env) = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            env,
            ..
        } => (name, qualifier.as_ref(), *env),
        RawNode::Unqualified { name, env, .. } => (name, None, *env),
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
            [qualifier] => graph.scopes.lookup(env, qualifier).cloned(),
            _ => None,
        })
    })
    .or_else(|| find_unqualified_binding(name, env, cx));
    let Some(Binding::Cte(target_scope) | Binding::DerivedTable(target_scope)) = binding else {
        return None;
    };
    named_scope_sources(name, target_scope, cx)
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
