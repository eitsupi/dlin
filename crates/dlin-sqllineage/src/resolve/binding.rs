use std::collections::{HashMap, HashSet};

use super::arity::effective_bindings;
use super::lineage::{
    collect_output_sources, flatten_mapping_sources, named_scope_sources,
    named_wildcard_sources_for_scope, virtual_column_origin,
};
use super::mapping::{ScopeMappingCache, merge_transform, resolve_scope_mappings};
use super::star::{StarNameDecision, scope_star_name_decision};

use crate::graph::RawGraph;
use crate::graph::edge::EdgeKind;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, JoinMerge, JoinMergeKind, MergedColumns, VirtualSourceId};
use crate::types::{CatalogProvider, ColumnOrigin, TableRef, TransformKind};
use crate::{Dialect, Ident};

pub(super) fn resolve_node(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
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
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    let binding = binding.or_else(|| graph.scopes.lookup(scope, name).cloned());
    let Some(binding) = binding else {
        return Some(ColumnOrigin::Ambiguous {
            column: name.clone(),
            candidates: Vec::new(),
        });
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
                Some(ColumnOrigin::Ambiguous {
                    column: name.clone(),
                    candidates: Vec::new(),
                })
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
                Some(ColumnOrigin::Ambiguous {
                    column: name.clone(),
                    candidates: Vec::new(),
                })
            }
        }
        Binding::VirtualSource(_) => Some(ColumnOrigin::Ambiguous {
            column: name.clone(),
            candidates: Vec::new(),
        }),
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
    catalog: Option<&dyn CatalogProvider>,
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
    catalog: Option<&dyn CatalogProvider>,
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
                .or_else(|| find_unqualified_binding(name, *scope, graph, catalog));
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
                .or_else(|| find_unqualified_binding(name, *scope, graph, catalog));
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

/// The relation an unqualified column reference is read from: the only
/// relation in scope, or else the only CTE/derived table publishing the name.
/// A name that joins merge from several relations, or that is ambiguous, has
/// no single relation; `resolve_merged_column` handles the merged case.
pub(super) fn find_unqualified_binding(
    name: &Ident,
    scope: usize,
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
) -> Option<Binding> {
    let bindings = effective_bindings(scope, graph);
    if bindings.len() == 1 {
        return Some(bindings[0].1.clone());
    }
    match unqualified_choice(name, &bindings, scope, graph, catalog) {
        UnqualifiedChoice::Merged(mut members) if members.len() == 1 => members.pop(),
        _ => None,
    }
}

/// What an unqualified column name denotes among the relations of a FROM
/// clause.
enum UnqualifiedChoice {
    /// Several distinct columns could be meant. The tables listed are the
    /// physical candidates (empty when only CTEs or derived tables compete);
    /// a catalog may still single one out.
    Ambiguous(Vec<TableRef>),
    /// A single column. Its contributing bindings are in FROM order.
    Merged(Vec<Binding>),
}

/// A relation that may supply an unqualified column.
struct ColumnRelation<'a> {
    name: &'a Ident,
    binding: &'a Binding,
    /// The CTE/derived-table scope, `None` for a physical table. Aliases of
    /// the same scope are separate relations for name resolution.
    scope: Option<usize>,
}

fn merge_covers(merge: &JoinMerge, name: &Ident, dialect: Dialect) -> bool {
    match &merge.columns {
        MergedColumns::Using(columns) => columns
            .iter()
            .any(|column| column.column_key(dialect) == name.column_key(dialect)),
        MergedColumns::Natural => true,
    }
}

/// What the catalog says about whether `table` has the column `name`. A table
/// the catalog does not describe is `None`: unknown, neither having nor
/// lacking the column.
fn table_has_column(
    table: &TableRef,
    name: &Ident,
    catalog: Option<&dyn CatalogProvider>,
) -> Option<bool> {
    let catalog = catalog?;
    if catalog
        .resolve_column(name, std::slice::from_ref(table))
        .is_some()
    {
        return Some(true);
    }
    // TODO: This legacy membership comparison is intentionally retained until
    // the dedicated table_has_column follow-up adds dialect-aware matching.
    catalog.list_columns(table).map(|columns| {
        let wanted = name.value.to_lowercase();
        columns
            .iter()
            .any(|column| column.value.to_lowercase() == wanted)
    })
}

/// Decide what the unqualified `name` denotes.
///
/// Relations that can supply it are CTEs/derived tables that publish the name
/// and physical tables, whose columns are unknown unless a catalog says
/// otherwise (a table the catalog knows to lack the name is left out, but only
/// when a join merges the name, so ordinary lookups behave as without the
/// catalog and are refined afterwards). Each starts in a group of its own, and
/// every `USING`/`NATURAL` join that covers the name unites the groups of its
/// two sides. `USING (x)` guarantees both sides have `x`. `NATURAL` merges
/// only the columns both sides have, so it unites a physical table only when
/// the catalog confirms the table has the name; claiming a merge with a table
/// that may lack the column would report a source that may not exist, so such
/// tables stay separate choices. CTEs and derived tables are known from their
/// output columns. A join merges only the relations on its own two sides, so a
/// relation outside it (comma-joined, or joined with `ON`) stays a separate
/// choice. A side that already holds several unmerged relations makes the name
/// ambiguous within the join itself, so that join merges nothing.
///
/// The name is one column when all competing relations share a group. When a
/// CTE or derived table publishes the name it outranks physical tables
/// (their columns are unknown), so only relations merged with such a provider
/// count; otherwise the physical tables compete.
fn unqualified_choice(
    name: &Ident,
    bindings: &[(Ident, Binding)],
    scope: usize,
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
) -> UnqualifiedChoice {
    let merges: Vec<&JoinMerge> = graph
        .scopes
        .join_merges(scope)
        .iter()
        .filter(|merge| merge_covers(merge, name, graph.dialect))
        .collect();
    let consult_catalog = !merges.is_empty();

    let mut relations: Vec<ColumnRelation<'_>> = Vec::new();
    let mut relation_of: HashMap<String, usize> = HashMap::new();
    // Whether each relation is known to have the column.
    let mut known_to_have: Vec<bool> = Vec::new();
    for (binding_name, binding) in bindings {
        match binding {
            Binding::Cte(provider) | Binding::DerivedTable(provider) => {
                if !binding_publishes(binding, name, graph) {
                    continue;
                }
                relations.push(ColumnRelation {
                    name: binding_name,
                    binding,
                    scope: Some(*provider),
                });
                known_to_have.push(true);
                relation_of.insert(
                    binding_name.relation_key(graph.dialect).into_owned(),
                    relations.len() - 1,
                );
            }
            Binding::Table(table) => {
                let has_column = if consult_catalog {
                    table_has_column(table, name, catalog)
                } else {
                    None
                };
                if has_column == Some(false) {
                    continue;
                }
                known_to_have.push(has_column == Some(true));
                relations.push(ColumnRelation {
                    name: binding_name,
                    binding,
                    scope: None,
                });
                relation_of.insert(
                    binding_name.relation_key(graph.dialect).into_owned(),
                    relations.len() - 1,
                );
            }
            Binding::VirtualSource(_) => {}
        }
    }

    let mut groups: Vec<usize> = (0..relations.len()).collect();
    // Group membership resolves names; contributors track the value after
    // each join, including the preserved side of an outer join.
    let mut contributes = vec![true; relations.len()];
    for merge in merges {
        let natural = matches!(merge.columns, MergedColumns::Natural);
        let side_group = |side: &[Ident]| {
            let mut found = side.iter().filter_map(|member| {
                let key = member.relation_key(graph.dialect);
                relation_of
                    .get(key.as_ref())
                    .filter(|&&index| !natural || known_to_have[index])
                    .map(|&index| groups[index])
            });
            let first = found.next()?;
            found.all(|group| group == first).then_some(first)
        };
        let (left, right) = (side_group(&merge.left), side_group(&merge.right));
        let (Some(left), Some(right)) = (left, right) else {
            continue;
        };
        if left != right {
            for (index, &group) in groups.iter().enumerate() {
                if (group == left && matches!(merge.kind, JoinMergeKind::Right))
                    || (group == right && matches!(merge.kind, JoinMergeKind::Left))
                {
                    contributes[index] = false;
                }
            }
        }
        for group in &mut groups {
            if *group == right {
                *group = left;
            }
        }
    }

    let providers: Vec<usize> = (0..relations.len())
        .filter(|&index| relations[index].scope.is_some())
        .collect();
    if let Some(&first) = providers.first() {
        if providers
            .iter()
            .any(|&index| groups[index] != groups[first])
        {
            return UnqualifiedChoice::Ambiguous(
                providers
                    .iter()
                    .map(|&index| TableRef {
                        catalog: None,
                        schema: None,
                        table: relations[index].name.clone(),
                    })
                    .collect(),
            );
        }
        return UnqualifiedChoice::Merged(
            (0..relations.len())
                .filter(|&index| groups[index] == groups[first] && contributes[index])
                .map(|index| relations[index].binding.clone())
                .collect(),
        );
    }

    let one_column = groups.windows(2).all(|pair| pair[0] == pair[1]);
    if one_column && !relations.is_empty() {
        UnqualifiedChoice::Merged(
            relations
                .iter()
                .enumerate()
                .filter(|(index, _)| contributes[*index])
                .map(|(_, relation)| relation.binding.clone())
                .collect(),
        )
    } else {
        UnqualifiedChoice::Ambiguous(
            relations
                .iter()
                .filter_map(|relation| match relation.binding {
                    Binding::Table(table) => Some(table.clone()),
                    _ => None,
                })
                .collect(),
        )
    }
}

/// `ColumnOrigin` has no equality of its own; its debug form covers every field.
fn same_origin(left: &ColumnOrigin, right: &ColumnOrigin) -> bool {
    format!("{left:?}") == format!("{right:?}")
}

fn binding_publishes(binding: &Binding, name: &Ident, graph: &RawGraph) -> bool {
    let (Binding::Cte(scope) | Binding::DerivedTable(scope)) = binding else {
        return false;
    };
    graph
        .scopes
        .output_columns(*scope)
        .iter()
        .any(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))
}

/// Resolve an unqualified column that `USING`/`NATURAL` joins merge from
/// several relations. Only the sides that supply its value contribute:
/// both for inner/full joins, the preserved side for left/right joins.
/// A physical table contributes itself, a CTE or derived table its origins
/// and transform.
/// Returns `None` for any other reference, which resolves to one relation.
pub(super) fn resolve_merged_column(
    node_id: NodeId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
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
    let bindings = effective_bindings(scope, graph);
    if bindings.len() < 2 {
        return None;
    }
    let UnqualifiedChoice::Merged(members) =
        unqualified_choice(name, &bindings, scope, graph, catalog)
    else {
        return None;
    };
    if members.len() < 2 {
        return None;
    }

    let mut sources: Vec<ColumnOrigin> = Vec::new();
    let mut has_back = false;
    let mut transform = TransformKind::Direct;
    for member in members {
        let (origins, member_back, member_transform) = match member {
            Binding::Table(table) => (
                vec![ColumnOrigin::Concrete {
                    table,
                    column: name.clone(),
                }],
                false,
                TransformKind::Direct,
            ),
            Binding::Cte(provider) | Binding::DerivedTable(provider) => named_scope_sources(
                name,
                provider,
                graph,
                resolved,
                incoming,
                catalog,
                mapping_cache,
            )
            .unwrap_or_else(|| {
                (
                    vec![ColumnOrigin::Ambiguous {
                        column: name.clone(),
                        candidates: Vec::new(),
                    }],
                    false,
                    TransformKind::Direct,
                )
            }),
            Binding::VirtualSource(_) => continue,
        };
        for origin in origins {
            // Two relations often reach the same underlying column.
            if !sources.iter().any(|seen| same_origin(seen, &origin)) {
                sources.push(origin);
            }
        }
        has_back |= member_back;
        transform = merge_transform(&transform, &member_transform);
    }
    Some((sources, has_back, transform))
}

#[allow(clippy::too_many_arguments)]
fn resolve_unqualified(
    name: &Ident,
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    resolve_from_bindings(
        name,
        &effective_bindings(scope, graph),
        scope,
        graph,
        resolved,
        incoming,
        visited,
        catalog,
        mapping_cache,
    )
}

#[allow(clippy::too_many_arguments)]
fn resolve_from_bindings(
    name: &Ident,
    bindings: &[(Ident, Binding)],
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    visited: &mut HashSet<NodeId>,
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    if bindings.len() == 1 {
        let (_, binding) = &bindings[0];
        match binding {
            Binding::Table(table_ref) => Some(ColumnOrigin::Concrete {
                table: table_ref.clone(),
                column: name.clone(),
            }),
            Binding::Cte(cte_scope) | Binding::DerivedTable(cte_scope) => resolve_through_scope(
                name,
                *cte_scope,
                graph,
                resolved,
                incoming,
                visited,
                catalog,
                mapping_cache,
            ),
            Binding::VirtualSource(_) => None,
        }
    } else if bindings.is_empty() {
        Some(ColumnOrigin::Ambiguous {
            column: name.clone(),
            candidates: Vec::new(),
        })
    } else {
        match unqualified_choice(name, bindings, scope, graph, catalog) {
            UnqualifiedChoice::Ambiguous(candidates) => Some(ColumnOrigin::Ambiguous {
                column: name.clone(),
                candidates,
            }),
            // A merged column is collected from all its relations by
            // `resolve_merged_column`; a single origin can only name the
            // leftmost one.
            UnqualifiedChoice::Merged(members) => match members.into_iter().next() {
                Some(Binding::Table(table)) => Some(ColumnOrigin::Concrete {
                    table,
                    column: name.clone(),
                }),
                Some(Binding::Cte(provider) | Binding::DerivedTable(provider)) => {
                    resolve_through_scope(
                        name,
                        provider,
                        graph,
                        resolved,
                        incoming,
                        visited,
                        catalog,
                        mapping_cache,
                    )
                }
                Some(Binding::VirtualSource(_)) | None => None,
            },
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
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Option<ColumnOrigin> {
    // Resolve through the same expanded output mappings used by the public
    // projection path. This is important for a qualified CTE/derived
    // reference whose name was introduced by a catalog-expanded star.
    let mappings = resolve_scope_mappings(
        target_scope,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    );
    if let Some(mapping) = mappings.iter().find(|mapping| {
        mapping.target.column.column_key(graph.dialect) == column_name.column_key(graph.dialect)
    }) {
        let (origins, has_back) = flatten_mapping_sources(&mapping.sources);
        return if has_back {
            Some(ColumnOrigin::Recursive {
                base_sources: origins,
            })
        } else {
            origins.into_iter().next()
        };
    }
    let requested_name =
        match scope_star_name_decision(target_scope, column_name, graph, &mut HashSet::new()) {
            StarNameDecision::Denied | StarNameDecision::Ambiguous => {
                return Some(ColumnOrigin::Ambiguous {
                    column: column_name.clone(),
                    candidates: Vec::new(),
                });
            }
            StarNameDecision::Replaced(node_id) => {
                let (sources, _, has_back, _) = collect_output_sources(
                    node_id,
                    graph,
                    resolved,
                    incoming,
                    &mut HashSet::new(),
                    catalog,
                    mapping_cache,
                );
                return if has_back {
                    Some(ColumnOrigin::Recursive {
                        base_sources: sources,
                    })
                } else if sources.len() == 1 {
                    sources.into_iter().next()
                } else {
                    Some(ColumnOrigin::Ambiguous {
                        column: column_name.clone(),
                        candidates: Vec::new(),
                    })
                };
            }
            StarNameDecision::Renamed(old) => old,
            StarNameDecision::Allowed => column_name.clone(),
        };
    if let Some(source) = named_wildcard_sources_for_scope(
        target_scope,
        &requested_name,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    )
    .into_iter()
    .next()
    {
        return Some(source);
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
