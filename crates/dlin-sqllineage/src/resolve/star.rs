use std::collections::HashSet;

use super::arity::{
    StarTarget, effective_bindings, ilike_matches, resolve_star_target, same_column_name,
};
use super::binding::{derive_transform, resolve_captured_binding};
use super::lineage::{
    collect_leaf_origins, collect_output_sources, expand_scope_columns,
    named_wildcard_sources_for_star,
};
use super::mapping::{
    ScopeMappingCache, expand_virtual_source, mappings_have_unknown_shape, merge_transform,
    wildcard_mapping,
};

use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode, StarBase, StarColumnName, StarOptions};
use crate::graph::scope::{Binding, OutputPlan};
use crate::types::{
    CatalogProvider, ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind,
};

/// Expand a Star node (qualified or unqualified) into `ColumnMapping`s.
#[allow(clippy::too_many_arguments)]
pub(super) fn expand_star(
    base: &StarBase,
    options: &StarOptions,
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
    visited_scopes: &mut HashSet<usize>,
) {
    let start = mappings.len();
    match resolve_star_target(base, scope, graph) {
        StarTarget::All => {
            for (_, binding) in effective_bindings(scope, graph) {
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
        StarTarget::Binding(binding) => expand_star_binding(
            &binding,
            graph,
            resolved,
            incoming,
            catalog,
            mapping_cache,
            mappings,
            visited_scopes,
        ),
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
            sources.push(ColumnOrigin::Ambiguous {
                column: "*".to_string(),
                candidates: Vec::new(),
            });
            mappings.push(ColumnMapping {
                target: ColumnRef {
                    table: None,
                    column: "*".to_string(),
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
            if !sources.iter().any(
                |source| matches!(source, ColumnOrigin::Ambiguous { column, .. } if column == "*"),
            ) {
                sources.push(ColumnOrigin::Ambiguous {
                    column: "*".to_string(),
                    candidates: Vec::new(),
                });
            }
            mappings.push(ColumnMapping {
                target: ColumnRef {
                    table: None,
                    column: "*".to_string(),
                },
                sources,
                transform: TransformKind::Direct,
            });
        }
    }
    apply_star_options(
        mappings,
        start,
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

#[allow(clippy::too_many_arguments)]
fn expand_star_binding(
    binding: &Binding,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
    visited_scopes: &mut HashSet<usize>,
) {
    match binding {
        Binding::Table(table) => {
            if let Some(columns) = catalog.and_then(|catalog| catalog.list_columns(table)) {
                mappings.extend(columns.into_iter().map(|column| ColumnMapping {
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
    catalog: Option<&dyn CatalogProvider>,
    mappings: &mut Vec<ColumnMapping>,
) {
    if let Some(columns) = catalog.and_then(|catalog| catalog.list_columns(&table)) {
        mappings.extend(columns.into_iter().map(|column| ColumnMapping {
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
    base: &StarBase,
    scope: usize,
    options: &StarOptions,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) {
    if mappings_have_unknown_shape(&mappings[start..]) {
        append_unknown_star_options(
            mappings,
            start,
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
    for mapping in original {
        let name = &mapping.target.column;
        let excluded = options
            .exclude
            .iter()
            .any(|excluded| excluded_matches_mapping(excluded, name, &mapping, base));
        let ilike_mismatch = options
            .ilike
            .as_deref()
            .is_some_and(|pattern| !ilike_matches(pattern, name));
        if !excluded && !ilike_mismatch {
            retained.push(mapping);
        }
    }
    for replacement in &options.replace {
        let Some(index) = retained
            .iter()
            .position(|mapping| same_column_name(&mapping.target.column, &replacement.column))
        else {
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
            .find(|mapping| same_column_name(&mapping.target.column, old))
        {
            mapping.target.column.clone_from(new);
        }
    }
    mappings.extend(retained);
}

#[allow(clippy::too_many_arguments)]
fn append_unknown_star_options(
    mappings: &mut Vec<ColumnMapping>,
    start: usize,
    base: &StarBase,
    scope: usize,
    options: &StarOptions,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) {
    let unknown_sources = mappings[start..].to_vec();
    for replacement in &options.replace {
        if !name_passes_star_filters(&replacement.column, options, base) {
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
        if !name_passes_star_filters(old, options, base) {
            continue;
        }
        let sources =
            named_wildcard_sources_for_star(&unknown_sources, old, options, base, scope, graph);
        if !sources
            .iter()
            .any(|source| matches!(source, ColumnOrigin::NamedWildcard { .. }))
        {
            continue;
        }
        mappings.push(ColumnMapping {
            target: ColumnRef {
                table: None,
                column: new.clone(),
            },
            sources,
            transform: TransformKind::Direct,
        });
    }
}

fn name_passes_star_filters(name: &str, options: &StarOptions, base: &StarBase) -> bool {
    !options
        .exclude
        .iter()
        .any(|excluded| excluded_matches_name(excluded, name, base))
        && options
            .ilike
            .as_deref()
            .is_none_or(|pattern| ilike_matches(pattern, name))
}

fn excluded_matches_name(excluded: &StarColumnName, name: &str, base: &StarBase) -> bool {
    let Some((excluded_column, qualifier)) = excluded.parts.split_last() else {
        return false;
    };
    same_column_name(name, excluded_column)
        && (qualifier.is_empty()
            || matches!(base, StarBase::Qualified(parts) if parts == qualifier))
}

fn excluded_matches_mapping(
    excluded: &StarColumnName,
    column: &str,
    mapping: &ColumnMapping,
    base: &StarBase,
) -> bool {
    let Some((excluded_column, qualifier)) = excluded.parts.split_last() else {
        return false;
    };
    if !same_column_name(column, excluded_column) {
        return false;
    }
    if qualifier.is_empty() {
        return true;
    }
    if let StarBase::Qualified(parts) = base
        && parts == qualifier
    {
        return true;
    }
    mapping.sources.iter().any(|source| {
        let table = match source {
            ColumnOrigin::Concrete { table, .. }
            | ColumnOrigin::Wildcard { table }
            | ColumnOrigin::NamedWildcard { table, .. } => table,
            ColumnOrigin::Recursive { base_sources } => {
                return base_sources
                    .iter()
                    .any(|source| relation_matches_qualifier(source, qualifier));
            }
            _ => return false,
        };
        table_matches_qualifier(table, qualifier)
    })
}

fn relation_matches_qualifier(source: &ColumnOrigin, qualifier: &[String]) -> bool {
    match source {
        ColumnOrigin::Concrete { table, .. }
        | ColumnOrigin::Wildcard { table }
        | ColumnOrigin::NamedWildcard { table, .. } => table_matches_qualifier(table, qualifier),
        _ => false,
    }
}

pub(super) fn table_matches_qualifier(table: &TableRef, qualifier: &[String]) -> bool {
    match qualifier {
        [name] => table.table == *name,
        [schema, name] => table.schema.as_deref() == Some(schema) && table.table == *name,
        [catalog, schema, name] => {
            table.catalog.as_deref() == Some(catalog)
                && table.schema.as_deref() == Some(schema)
                && table.table == *name
        }
        _ => false,
    }
}

#[derive(Clone)]
pub(super) enum StarNameDecision {
    Allowed,
    Denied,
    Renamed(String),
    Replaced(NodeId),
    Ambiguous,
}

pub(super) fn star_name_decision(
    options: &StarOptions,
    name: &str,
    base: Option<&StarBase>,
) -> StarNameDecision {
    if options.exclude.iter().any(|excluded| {
        excluded.parts.len() == 1 && same_column_name(name, &excluded.parts[0])
            || base.is_some_and(|base| excluded_matches_name(excluded, name, base))
    }) || options
        .ilike
        .as_deref()
        .is_some_and(|pattern| !ilike_matches(pattern, name))
    {
        return StarNameDecision::Denied;
    }
    if let Some(replacement) = options
        .replace
        .iter()
        .find(|replacement| same_column_name(name, &replacement.column))
    {
        return StarNameDecision::Replaced(replacement.node_id);
    }
    if let Some((old, _)) = options
        .rename
        .iter()
        .find(|(_, new)| same_column_name(name, new))
    {
        return StarNameDecision::Renamed(old.clone());
    }
    if options
        .rename
        .iter()
        .any(|(old, _)| same_column_name(name, old))
    {
        return StarNameDecision::Denied;
    }
    StarNameDecision::Allowed
}

pub(super) fn scope_star_name_decision(
    scope: usize,
    name: &str,
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
                        Some(star_name_decision(options, name, Some(base)))
                    } else {
                        None
                    }
                })
                .collect(),
        ),
        OutputPlan::Delegate(child) => scope_star_name_decision(*child, name, graph, visited),
        // The left branch defines set-operation output names and modifiers.
        OutputPlan::SetOperation { left, .. } => {
            scope_star_name_decision(*left, name, graph, visited)
        }
    }
}

fn combine_star_name_decisions(decisions: Vec<StarNameDecision>) -> StarNameDecision {
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
                Some(StarNameDecision::Renamed(existing)) if existing == &old => {}
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
