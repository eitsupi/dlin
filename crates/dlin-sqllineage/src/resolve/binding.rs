use super::context::EvalContext;
use super::lineage::virtual_column_origin;
use super::model::InputLookup;
use super::star::{input_name_sources, lookup_input, named_scope_sources};
use super::target::effective_bindings;

use crate::catalog::ColumnPresence;
use crate::graph::RawGraph;
use crate::graph::edge::EdgeKind;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, BindingEnv, VirtualSourceId};
use crate::types::{ColumnOrigin, TransformKind};
use crate::{Candidate, Ident, Relation, Unresolved};

pub(super) fn resolve_node(node_id: NodeId, cx: &mut EvalContext<'_, '_>) -> Option<ColumnOrigin> {
    let graph = cx.graph;
    if let Some(origin) = ambiguous_qualified_origin(node_id, graph) {
        return Some(origin);
    }
    if let Some(ref origin) = cx.resolved[node_id] {
        return Some(origin.clone());
    }
    let generation = cx.cut_generation();

    let origin = match &graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier,
            env,
            binding,
        } => {
            let binding = binding.clone().or_else(|| {
                qualifier.as_deref().and_then(|parts| match parts {
                    [qualifier] => graph.scopes.lookup(*env, qualifier).cloned(),
                    _ => None,
                })
            });
            if let Some(binding) = binding {
                resolve_captured_binding(name, binding, cx)
            } else if qualifier.is_some() {
                // The qualifier names no relation in scope. Building a table
                // out of its text would present a guess as a confident
                // source, so the reference stays unresolved.
                Some(ColumnOrigin::Ambiguous {
                    column: name.clone(),
                    candidates: Vec::new(),
                })
            } else {
                resolve_unqualified(name, *env, cx)
            }
        }

        RawNode::Unqualified { name, env, binding } => {
            if let Some(binding) = binding
                .clone()
                .or_else(|| lookup_virtual_binding(*env, name, graph))
            {
                resolve_captured_binding(name, binding, cx)
            } else {
                resolve_unqualified(name, *env, cx)
            }
        }

        RawNode::RowValueCandidate { name, env, binding } => {
            resolve_row_value_candidate(name, *env, binding.clone(), cx)
        }

        RawNode::Star { .. } => None,

        RawNode::Output { .. } => None,
    };

    // An origin found while a cycle was cut depends on where the walk
    // started, so it is returned and not kept.
    if cx.cut_generation() == generation {
        cx.resolved[node_id].clone_from(&origin);
    }
    origin
}

pub(super) fn ambiguous_qualified_origin(node: NodeId, graph: &RawGraph) -> Option<ColumnOrigin> {
    let RawNode::Ref {
        name,
        qualifier: Some(parts),
        env,
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
        .ambiguous_binding_candidates(*env, qualifier)
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
    env: BindingEnv,
    name: &Ident,
    graph: &RawGraph,
) -> Option<Binding> {
    graph
        .scopes
        .visible_bindings(env)
        .into_iter()
        .find(|(binding_name, binding)| {
            matches!(binding, Binding::VirtualSource(_))
                && binding_name.column_key(graph.dialect) == name.column_key(graph.dialect)
        })
        .map(|(_, binding)| binding)
}

fn resolve_row_value_candidate(
    name: &Ident,
    env: BindingEnv,
    binding: Option<Binding>,
    cx: &mut EvalContext<'_, '_>,
) -> Option<ColumnOrigin> {
    let graph = cx.graph;
    let catalog = cx.catalog;
    let binding = binding.or_else(|| graph.scopes.lookup(env, name).cloned());
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
                resolve_through_scope(name, target_scope, cx)
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

pub(super) fn resolve_captured_binding(
    name: &Ident,
    binding: Binding,
    cx: &mut EvalContext<'_, '_>,
) -> Option<ColumnOrigin> {
    let graph = cx.graph;
    match binding {
        Binding::Table(table) => {
            if cx.catalog.is_some_and(|catalog| {
                matches!(
                    catalog.column_presence(&table, name),
                    ColumnPresence::Duplicate
                )
            }) {
                return Some(ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                    column: name.clone(),
                    candidates: vec![Candidate::Relation(Relation {
                        catalog: table.catalog,
                        schema: table.schema,
                        name: table.table,
                    })],
                }));
            }
            Some(ColumnOrigin::Concrete {
                table,
                column: name.clone(),
            })
        }
        Binding::Cte(scope) | Binding::DerivedTable(scope) => {
            resolve_through_scope(name, scope, cx)
        }
        Binding::VirtualSource(source) => virtual_column_origin(name, source, graph),
    }
}

/// The CTE or derived table an unqualified column reference is read from, when
/// the folded FROM clause settles on a single one.
pub(super) fn find_unqualified_binding(
    name: &Ident,
    env: BindingEnv,
    cx: &mut EvalContext<'_, '_>,
) -> Option<Binding> {
    lookup_input(env, name, cx).binding()
}

/// Resolve an unqualified column that no range variable names, from every
/// relation the FROM clause lets supply it. A column that `USING`/`NATURAL`
/// joins merge keeps all the sides that supply its value.
/// Returns `None` for any other reference.
pub(super) fn resolve_input_reference(
    node_id: NodeId,
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let (name, env) = match &cx.graph.nodes[node_id] {
        RawNode::Ref {
            name,
            qualifier: None,
            env,
            binding: None,
        }
        | RawNode::Unqualified {
            name,
            env,
            binding: None,
        } => (name, *env),
        _ => return None,
    };
    input_name_sources(env, name, cx)
}

/// The origin of an unqualified column that `resolve_input_reference` could not
/// answer: the one CTE or derived table that decides the name publishes nothing
/// under it, so the name is ambiguous, or a virtual source that does not
/// publish columns by name.
///
/// Every other answer of the input lookup (a column, several merged sources, an
/// ambiguity, a physical relation) is returned in full by
/// `resolve_input_reference` before a node is resolved on its own, so no merged
/// source can reach here to be reduced to its first.
fn resolve_unqualified(
    name: &Ident,
    env: BindingEnv,
    cx: &mut EvalContext<'_, '_>,
) -> Option<ColumnOrigin> {
    match lookup_input(env, name, cx) {
        InputLookup::Scope { scope: target, .. } => resolve_through_scope(name, target, cx),
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
    env: BindingEnv,
    name: &Ident,
    graph: &RawGraph,
) -> Vec<VirtualSourceId> {
    effective_bindings(env, graph)
        .into_iter()
        .filter_map(|(_, binding)| match binding {
            Binding::VirtualSource(source) if virtual_has_column(name, source, graph) => {
                Some(source)
            }
            _ => None,
        })
        .collect()
}

/// The origin of `column_name` read from the output of `target_scope`. A scope
/// whose model publishes nothing under the name, including one that depends on
/// itself, yields an ambiguous origin without candidates and never an empty one.
fn resolve_through_scope(
    column_name: &Ident,
    target_scope: usize,
    cx: &mut EvalContext<'_, '_>,
) -> Option<ColumnOrigin> {
    // Resolve through the scope's model, which also lists the names a
    // catalog-expanded star introduced.
    if let Some((origins, has_back, _)) = named_scope_sources(column_name, target_scope, cx) {
        return if has_back {
            Some(ColumnOrigin::Recursive {
                base_sources: origins,
            })
        } else {
            origins.into_iter().next()
        };
    }
    // The scope publishes nothing under this name.
    Some(ColumnOrigin::Ambiguous {
        column: column_name.clone(),
        candidates: Vec::new(),
    })
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
