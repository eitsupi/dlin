use std::collections::HashSet;

use super::binding::{derive_transform, resolve_captured_binding};
use super::lineage::{
    collect_leaf_origins, collect_output_sources, expand_scope_columns,
    named_wildcard_sources_for_star,
};
use super::mapping::{
    ScopeMappingCache, expand_virtual_source, mappings_have_unknown_shape, merge_transform,
    wildcard_mapping,
};
use super::target::{
    StarTarget, effective_bindings, excluded_matches_name, ilike_matches, resolve_star_target,
    same_column_name,
};

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode, StarBase, StarOptions};
use crate::graph::scope::{Binding, JoinMergeKind, OutputPlan};
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind};
use crate::{Dialect, Ident};

/// Expand a Star node (qualified or unqualified) into `ColumnMapping`s.
#[allow(clippy::too_many_arguments)]
pub(super) fn expand_star(
    base: &StarBase,
    options: &StarOptions,
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
    visited_scopes: &mut HashSet<usize>,
) {
    let start = mappings.len();
    let range_variables = expand_star_columns(
        base,
        scope,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
        mappings,
        visited_scopes,
    );
    apply_star_options(
        mappings,
        start,
        &range_variables,
        base,
        scope,
        options,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    );
}

/// Expand without modifiers, returning the range variables for each new mapping.
#[allow(clippy::too_many_arguments)]
pub(super) fn expand_star_columns(
    base: &StarBase,
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
    visited_scopes: &mut HashSet<usize>,
) -> Vec<Vec<Ident>> {
    let start = mappings.len();
    let mut range_variables = Vec::new();
    match resolve_star_target(base, scope, graph) {
        StarTarget::Ambiguous(candidates) => mappings.push(ColumnMapping {
            output_name: None,
            target: ColumnRef {
                table: None,
                column: Ident::unquoted("*"),
            },
            sources: vec![ColumnOrigin::Unresolved(crate::Unresolved::Ambiguous {
                column: Ident::unquoted("*"),
                candidates,
            })],
            transform: TransformKind::Direct,
        }),
        StarTarget::All => {
            let mut groups = Vec::new();
            for (name, binding) in effective_bindings(scope, graph) {
                let binding_start = mappings.len();
                expand_star_binding(
                    &binding,
                    graph,
                    resolved,
                    incoming,
                    catalog,
                    mapping_cache,
                    mappings,
                    visited_scopes,
                );
                let columns = mappings[binding_start..]
                    .iter()
                    .map(|column| (column.clone(), vec![name.clone()]))
                    .collect();
                range_variables.extend((binding_start..mappings.len()).map(|_| vec![name.clone()]));
                groups.push((vec![name], columns));
            }
            if !graph.scopes.join_merges(scope).is_empty()
                && !mappings_have_unknown_shape(&mappings[start..])
                && let Some(merged) = super::join::merge_join_columns(
                    groups,
                    graph.scopes.join_merges(scope),
                    graph.dialect,
                    |column| &column.0.target.column,
                    |left, right, kind| {
                        let mut column = left.0.clone();
                        match kind {
                            JoinMergeKind::Left => {}
                            JoinMergeKind::Right => {
                                column.sources.clone_from(&right.0.sources);
                                column.transform.clone_from(&right.0.transform);
                            }
                            JoinMergeKind::Inner | JoinMergeKind::Full => {
                                column.sources.extend(right.0.sources.clone());
                                column.transform =
                                    merge_transform(&left.0.transform, &right.0.transform);
                            }
                        }
                        (
                            column,
                            left.1.iter().chain(&right.1).cloned().collect::<Vec<_>>(),
                        )
                    },
                )
            {
                mappings.truncate(start);
                range_variables.clear();
                for (column, qualifiers) in merged {
                    mappings.push(column);
                    range_variables.push(qualifiers);
                }
            }
            for &child in graph.scopes.anonymous_derived(scope) {
                expand_scope_columns(
                    child,
                    graph,
                    resolved,
                    incoming,
                    catalog,
                    mapping_cache,
                    mappings,
                    visited_scopes,
                );
            }
        }
        StarTarget::Binding { name, binding } => {
            expand_star_binding(
                &binding,
                graph,
                resolved,
                incoming,
                catalog,
                mapping_cache,
                mappings,
                visited_scopes,
            );
            range_variables.resize(mappings.len() - start, vec![name]);
        }
        StarTarget::Unknown(table) => expand_unknown_relation(table, catalog, mappings),
        StarTarget::FieldPath { binding, path } => {
            let mut sources = Vec::new();
            if let Some(field) = path.first()
                && let Some(origin) = resolve_captured_binding(
                    field,
                    binding,
                    graph,
                    resolved,
                    incoming,
                    &mut HashSet::new(),
                    catalog,
                    mapping_cache,
                )
            {
                sources.push(origin);
            }
            sources.push(ColumnOrigin::UnknownStar);
            mappings.push(ColumnMapping {
                output_name: None,
                target: ColumnRef {
                    table: None,
                    column: Ident::unquoted("*"),
                },
                sources,
                transform: TransformKind::Direct,
            });
        }
        StarTarget::Expr => {
            let mut sources = Vec::new();
            if let StarBase::Expr(dependencies) = base {
                for &dependency in dependencies {
                    let (origins, _, _) = collect_leaf_origins(
                        dependency,
                        graph,
                        resolved,
                        incoming,
                        &mut HashSet::new(),
                        catalog,
                        mapping_cache,
                    );
                    sources.extend(origins);
                }
            }
            if !sources.iter().any(ColumnOrigin::is_unknown_star) {
                sources.push(ColumnOrigin::UnknownStar);
            }
            mappings.push(ColumnMapping {
                output_name: None,
                target: ColumnRef {
                    table: None,
                    column: Ident::unquoted("*"),
                },
                sources,
                transform: TransformKind::Direct,
            });
        }
    }
    range_variables.resize(mappings.len() - start, Vec::new());
    range_variables
}

#[allow(clippy::too_many_arguments)]
fn expand_star_binding(
    binding: &Binding,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
    visited_scopes: &mut HashSet<usize>,
) {
    match binding {
        Binding::Table(table) => {
            if let Some(columns) = catalog.and_then(|catalog| catalog.list_columns(table)) {
                mappings.extend(columns.into_iter().map(|column| ColumnMapping {
                    output_name: Some(crate::OutputName::Explicit(column.clone())),
                    target: ColumnRef {
                        table: None,
                        column: column.clone(),
                    },
                    sources: vec![ColumnOrigin::Concrete {
                        table: table.clone(),
                        column,
                    }],
                    transform: TransformKind::Direct,
                }));
            } else {
                mappings.push(wildcard_mapping(None, table.clone()));
            }
        }
        Binding::Cte(scope) | Binding::DerivedTable(scope) => expand_scope_columns(
            *scope,
            graph,
            resolved,
            incoming,
            catalog,
            mapping_cache,
            mappings,
            visited_scopes,
        ),
        Binding::VirtualSource(source) => expand_virtual_source(
            *source,
            graph,
            resolved,
            incoming,
            catalog,
            mapping_cache,
            mappings,
        ),
    }
}

fn expand_unknown_relation(
    table: TableRef,
    catalog: Option<&CatalogContext<'_>>,
    mappings: &mut Vec<ColumnMapping>,
) {
    if let Some(columns) = catalog.and_then(|catalog| catalog.list_columns(&table)) {
        mappings.extend(columns.into_iter().map(|column| ColumnMapping {
            output_name: Some(crate::OutputName::Explicit(column.clone())),
            target: ColumnRef {
                table: None,
                column: column.clone(),
            },
            sources: vec![ColumnOrigin::Concrete {
                table: table.clone(),
                column,
            }],
            transform: TransformKind::Direct,
        }));
    } else {
        mappings.push(wildcard_mapping(None, table));
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_star_options(
    mappings: &mut Vec<ColumnMapping>,
    start: usize,
    range_variables: &[Vec<Ident>],
    base: &StarBase,
    scope: usize,
    options: &StarOptions,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) {
    if mappings_have_unknown_shape(&mappings[start..]) {
        append_unknown_star_options(
            mappings,
            start,
            range_variables,
            base,
            scope,
            options,
            graph,
            resolved,
            incoming,
            catalog,
            mapping_cache,
        );
        return;
    }
    let original = mappings.split_off(start);
    let mut retained = Vec::with_capacity(original.len());
    for (mapping, qualifiers) in original.into_iter().zip(range_variables) {
        let name = &mapping.target.column;
        let excluded = options.exclude.iter().any(|excluded| {
            excluded_matches_name(excluded, name, Some(base), qualifiers, graph.dialect)
        });
        let ilike_mismatch = options
            .ilike
            .as_deref()
            .is_some_and(|pattern| !ilike_matches(pattern, &name.value));
        if !excluded && !ilike_mismatch {
            retained.push(mapping);
        }
    }
    for replacement in &options.replace {
        let Some(index) = retained.iter().position(|mapping| {
            same_column_name(&mapping.target.column, &replacement.column, graph.dialect)
        }) else {
            continue;
        };
        let mut visited = HashSet::new();
        let (sources, edge_kinds, _, inherited_transform) = collect_output_sources(
            replacement.node_id,
            graph,
            resolved,
            incoming,
            &mut visited,
            catalog,
            mapping_cache,
        );
        retained[index].sources = sources;
        retained[index].transform = merge_transform(
            &derive_transform(&graph.nodes[replacement.node_id], &edge_kinds),
            &inherited_transform,
        );
    }
    for (old, new) in &options.rename {
        if let Some(mapping) = retained
            .iter_mut()
            .find(|mapping| same_column_name(&mapping.target.column, old, graph.dialect))
        {
            mapping.target.column.clone_from(new);
            mapping.output_name = Some(crate::OutputName::Explicit(new.clone()));
        }
    }
    mappings.extend(retained);
}

#[allow(clippy::too_many_arguments)]
fn append_unknown_star_options(
    mappings: &mut Vec<ColumnMapping>,
    start: usize,
    range_variables: &[Vec<Ident>],
    base: &StarBase,
    scope: usize,
    options: &StarOptions,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&CatalogContext<'_>>,
    mapping_cache: &mut ScopeMappingCache,
) {
    // Keep the raw expansion for wildcard lookups and USING membership: removing
    // a known column must not remove its range variable from the merged key.
    let unknown_sources = mappings[start..].to_vec();
    let original = mappings.split_off(start);
    for (index, mapping) in original.into_iter().enumerate() {
        if mapping.output_name.is_some() {
            let name = &mapping.target.column;
            let merged_ranges = super::join::merged_star_range_variables(
                &unknown_sources,
                range_variables,
                name,
                graph.scopes.join_merges(scope),
                graph.dialect,
            );
            let excluded = options.exclude.iter().any(|excluded| {
                excluded_matches_name(
                    excluded,
                    name,
                    Some(base),
                    &merged_ranges[index],
                    graph.dialect,
                )
            });
            let ilike_mismatch = options
                .ilike
                .as_deref()
                .is_some_and(|pattern| !ilike_matches(pattern, &name.value));
            if excluded || ilike_mismatch {
                continue;
            }
        }
        mappings.push(mapping);
    }
    for replacement in &options.replace {
        if !name_passes_unknown_star_filters(
            &replacement.column,
            options,
            base,
            scope,
            &unknown_sources,
            range_variables,
            graph,
        ) {
            continue;
        }
        let mut visited = HashSet::new();
        let (sources, edge_kinds, _, inherited_transform) = collect_output_sources(
            replacement.node_id,
            graph,
            resolved,
            incoming,
            &mut visited,
            catalog,
            mapping_cache,
        );
        mappings.push(ColumnMapping {
            output_name: Some(crate::OutputName::Explicit(replacement.column.clone())),
            target: ColumnRef {
                table: None,
                column: replacement.column.clone(),
            },
            sources,
            transform: merge_transform(
                &derive_transform(&graph.nodes[replacement.node_id], &edge_kinds),
                &inherited_transform,
            ),
        });
    }
    for (old, new) in &options.rename {
        if !name_passes_unknown_star_filters(
            old,
            options,
            base,
            scope,
            &unknown_sources,
            range_variables,
            graph,
        ) {
            continue;
        }
        let sources = named_wildcard_sources_for_star(
            &unknown_sources,
            range_variables,
            old,
            options,
            base,
            scope,
            graph,
        );
        if !sources
            .iter()
            .any(|source| matches!(source, ColumnOrigin::NamedWildcard { .. }))
        {
            continue;
        }
        mappings.push(ColumnMapping {
            output_name: Some(crate::OutputName::Explicit(new.clone())),
            target: ColumnRef {
                table: None,
                column: new.clone(),
            },
            sources,
            transform: TransformKind::Direct,
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn name_passes_unknown_star_filters(
    name: &Ident,
    options: &StarOptions,
    base: &StarBase,
    scope: usize,
    mappings: &[ColumnMapping],
    range_variables: &[Vec<Ident>],
    graph: &RawGraph,
) -> bool {
    if options
        .ilike
        .as_deref()
        .is_some_and(|pattern| !ilike_matches(pattern, &name.value))
    {
        return false;
    }
    let ranges = super::join::merged_star_range_variables(
        mappings,
        range_variables,
        name,
        graph.scopes.join_merges(scope),
        graph.dialect,
    );
    mappings.iter().zip(ranges).any(|(mapping, qualifiers)| {
        (mapping.output_name.is_none()
            || same_column_name(&mapping.target.column, name, graph.dialect))
            && !options.exclude.iter().any(|excluded| {
                excluded_matches_name(excluded, name, Some(base), &qualifiers, graph.dialect)
            })
    })
}

#[derive(Clone)]
pub(super) enum StarNameDecision {
    Allowed,
    Denied,
    Renamed(Ident),
    Replaced(NodeId),
    Ambiguous,
}

pub(super) fn star_name_decision(
    options: &StarOptions,
    name: &Ident,
    base: Option<&StarBase>,
    dialect: Dialect,
) -> StarNameDecision {
    if options.exclude.iter().any(|excluded| {
        excluded.parts.len() == 1 && same_column_name(name, &excluded.parts[0], dialect)
            || base
                .is_some_and(|base| excluded_matches_name(excluded, name, Some(base), &[], dialect))
    }) || options
        .ilike
        .as_deref()
        .is_some_and(|pattern| !ilike_matches(pattern, &name.value))
    {
        return StarNameDecision::Denied;
    }
    if let Some(replacement) = options
        .replace
        .iter()
        .find(|replacement| same_column_name(name, &replacement.column, dialect))
    {
        return StarNameDecision::Replaced(replacement.node_id);
    }
    if let Some((old, _)) = options
        .rename
        .iter()
        .find(|(_, new)| same_column_name(name, new, dialect))
    {
        return StarNameDecision::Renamed(old.clone());
    }
    if options
        .rename
        .iter()
        .any(|(old, _)| same_column_name(name, old, dialect))
    {
        return StarNameDecision::Denied;
    }
    StarNameDecision::Allowed
}

pub(super) fn scope_star_name_decision(
    scope: usize,
    name: &Ident,
    graph: &RawGraph,
    visited: &mut HashSet<usize>,
) -> StarNameDecision {
    if !visited.insert(scope) {
        return StarNameDecision::Allowed;
    }
    match graph.scopes.output_plan(scope) {
        OutputPlan::Projection => combine_star_name_decisions(
            graph
                .scopes
                .output_columns(scope)
                .iter()
                .filter_map(|column| {
                    if let RawNode::Star { base, options, .. } = &graph.nodes[column.node_id] {
                        Some(star_name_decision(options, name, Some(base), graph.dialect))
                    } else {
                        None
                    }
                })
                .collect(),
            graph.dialect,
        ),
        OutputPlan::Delegate(child) => scope_star_name_decision(*child, name, graph, visited),
        // The left branch defines set-operation output names and modifiers.
        OutputPlan::SetOperation { left, .. } => {
            scope_star_name_decision(*left, name, graph, visited)
        }
    }
}

fn combine_star_name_decisions(
    decisions: Vec<StarNameDecision>,
    dialect: Dialect,
) -> StarNameDecision {
    let mut has_allowed = false;
    let mut has_denied = false;
    let mut specific: Option<StarNameDecision> = None;
    for decision in decisions {
        match decision {
            StarNameDecision::Allowed => has_allowed = true,
            StarNameDecision::Denied => has_denied = true,
            StarNameDecision::Ambiguous => return StarNameDecision::Ambiguous,
            StarNameDecision::Renamed(old) => match &specific {
                None => specific = Some(StarNameDecision::Renamed(old)),
                Some(StarNameDecision::Renamed(existing))
                    if same_column_name(existing, &old, dialect) => {}
                Some(_) => return StarNameDecision::Ambiguous,
            },
            StarNameDecision::Replaced(node_id) => match specific {
                None => specific = Some(StarNameDecision::Replaced(node_id)),
                Some(StarNameDecision::Replaced(existing)) if existing == node_id => {}
                Some(_) => return StarNameDecision::Ambiguous,
            },
        }
    }
    if has_allowed && specific.is_some() {
        return StarNameDecision::Ambiguous;
    }
    if has_allowed {
        StarNameDecision::Allowed
    } else if let Some(specific) = specific {
        specific
    } else if has_denied {
        StarNameDecision::Denied
    } else {
        StarNameDecision::Allowed
    }
}
