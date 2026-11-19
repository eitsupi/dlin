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
use crate::types::{ColumnOrigin, Evidence, Origin, Sources, TransformKind};
use crate::{Candidate, Ident, Relation, Unresolved};

/// The value a reference node denotes on its own. Empty when the reference
/// denotes nothing, which can still rely on evidence.
pub(super) fn resolve_node(node_id: NodeId, cx: &mut EvalContext<'_, '_>) -> Sources {
    let graph = cx.graph;
    if let Some(origin) = ambiguous_qualified_origin(node_id, graph) {
        return Sources::one(origin);
    }
    if let Some(ref sources) = cx.resolved[node_id] {
        return sources.clone();
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
                Sources::one(ColumnOrigin::Ambiguous {
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

        RawNode::Unsupported { what } => {
            Sources::one(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }))
        }

        RawNode::Star { .. } | RawNode::Output { .. } => Sources::default(),
    };

    // An origin found while a cycle was cut depends on where the walk
    // started, so it is returned and not kept.
    if cx.cut_generation() == generation {
        cx.resolved[node_id] = Some(origin.clone());
    }
    origin
}

pub(super) fn ambiguous_qualified_origin(node: NodeId, graph: &RawGraph) -> Option<Origin> {
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
            .into()
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
        .lookup(env, name)
        .filter(|binding| matches!(binding, Binding::VirtualSource(_)))
        .cloned()
}

fn resolve_row_value_candidate(
    name: &Ident,
    env: BindingEnv,
    binding: Option<Binding>,
    cx: &mut EvalContext<'_, '_>,
) -> Sources {
    let graph = cx.graph;
    let catalog = cx.catalog;
    let binding = binding.or_else(|| graph.scopes.lookup(env, name).cloned());
    let row_value = || {
        Sources::one(ColumnOrigin::Unresolved(Unresolved::RowValue {
            alias: name.clone(),
        }))
    };
    let Some(binding) = binding else {
        return row_value();
    };

    match binding {
        Binding::Table(table) => {
            // Whether the relation has a column of this name decides between a
            // column and a whole-row value either way.
            let evidence = Evidence::relation(&table);
            if let Some(owner) = catalog
                .and_then(|catalog| catalog.resolve_column(name, std::slice::from_ref(&table)))
            {
                Sources::one(ColumnOrigin::Concrete {
                    table: owner,
                    column: name.clone(),
                })
                .relying_on(&evidence)
            } else {
                row_value().relying_on(&evidence)
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
                row_value()
            }
        }
        Binding::VirtualSource(_) => row_value(),
    }
}

pub(super) fn resolve_captured_binding(
    name: &Ident,
    binding: Binding,
    cx: &mut EvalContext<'_, '_>,
) -> Sources {
    let graph = cx.graph;
    match binding {
        Binding::Table(table) => {
            let evidence = Evidence::relation(&table);
            if cx.catalog.is_some_and(|catalog| {
                matches!(
                    catalog.column_presence(&table, name),
                    ColumnPresence::Duplicate
                )
            }) {
                return Sources::one(ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                    column: name.clone(),
                    candidates: vec![Candidate::Relation(Relation {
                        catalog: table.catalog,
                        schema: table.schema,
                        namespace: table.namespace,
                        name: table.table,
                    })],
                }))
                .relying_on(&evidence);
            }
            Sources::one(ColumnOrigin::Concrete {
                table,
                column: name.clone(),
            })
            .relying_on(&evidence)
        }
        Binding::Cte(scope) | Binding::DerivedTable(scope) => {
            resolve_through_scope(name, scope, cx)
        }
        Binding::VirtualSource(source) => {
            virtual_column_origin(name, source, graph).map_or_default(Sources::one)
        }
    }
}

/// The CTE or derived table an unqualified column reference is read from, when
/// the folded FROM clause settles on a single one.
pub(super) fn find_unqualified_binding(
    name: &Ident,
    env: BindingEnv,
    cx: &mut EvalContext<'_, '_>,
) -> Option<Binding> {
    lookup_input(env, name, cx).0.binding()
}

/// Resolve an unqualified column that no range variable names, from every
/// relation the FROM clause lets supply it. A column that `USING`/`NATURAL`
/// joins merge keeps all the sides that supply its value.
/// Returns `None` for any other reference.
pub(super) fn resolve_input_reference(
    node_id: NodeId,
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Sources, bool, TransformKind)> {
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
            binding: None | Some(Binding::VirtualSource(_)),
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
fn resolve_unqualified(name: &Ident, env: BindingEnv, cx: &mut EvalContext<'_, '_>) -> Sources {
    let (lookup, evidence) = lookup_input(env, name, cx);
    match lookup {
        InputLookup::Scope { scope: target, .. } => {
            resolve_through_scope(name, target, cx).relying_on(&evidence)
        }
        lookup => {
            debug_assert!(
                matches!(lookup, InputLookup::NoSource),
                "an answered lookup never reaches node resolution: {lookup:?}"
            );
            Sources::default()
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

pub(super) fn virtual_column_indexes(
    name: &Ident,
    source: VirtualSourceId,
    graph: &RawGraph,
) -> Vec<usize> {
    graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| {
            (column.name.column_key(graph.dialect) == name.column_key(graph.dialect))
                .then_some(index)
        })
        .collect()
}

pub(super) fn find_virtual_sources_for_column(
    env: BindingEnv,
    name: &Ident,
    graph: &RawGraph,
) -> Vec<VirtualSourceId> {
    for clause in graph.scopes.clause_envs(env) {
        let matches = effective_bindings(clause, graph)
            .into_iter()
            .filter_map(|(_, binding)| match binding {
                Binding::VirtualSource(source) if virtual_has_column(name, source, graph) => {
                    Some(source)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            return matches;
        }
    }
    Vec::new()
}

/// The origin of `column_name` read from the output of `target_scope`. A scope
/// whose model publishes nothing under the name, including one that depends on
/// itself, yields an ambiguous origin without candidates and never an empty one.
fn resolve_through_scope(
    column_name: &Ident,
    target_scope: usize,
    cx: &mut EvalContext<'_, '_>,
) -> Sources {
    // Resolve through the scope's model, which also lists the names a
    // catalog-expanded star introduced.
    if let Some((sources, has_back, _)) = named_scope_sources(column_name, target_scope, cx) {
        return if has_back {
            sources.recursive()
        } else {
            // A reference resolved on its own stands for one origin; what the
            // others and the conclusion relied on stays with the value.
            let evidence = sources.all_evidence();
            Sources {
                origins: sources.origins.into_iter().take(1).collect(),
                evidence,
            }
        };
    }
    // The scope publishes nothing under this name.
    Sources::one(ColumnOrigin::Ambiguous {
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
