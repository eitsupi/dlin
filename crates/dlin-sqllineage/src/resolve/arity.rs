use std::collections::HashSet;

use crate::graph::RawGraph;
use crate::graph::node::{RawNode, StarBase, StarColumnName, StarOptions};
use crate::graph::scope::{Binding, OutputPlan, ScopeTree};
use crate::types::{CatalogProvider, SemanticError, TableRef};

/// Inspect the complete graph rather than only the root projection. A star
/// under a JOIN/CTE/derived-table boundary is still an unresolved schema
/// dependency and must be visible to consumers of the public result.
pub(super) fn graph_has_unresolved_stars(
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
) -> bool {
    graph.nodes.iter().any(|node| {
        let RawNode::Star {
            base,
            options,
            scope,
        } = node
        else {
            return false;
        };
        star_arity(base, options, *scope, graph, catalog, &mut HashSet::new()).is_none()
    })
}

pub(super) fn validate_set_arities(
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
) -> Result<(), SemanticError> {
    let mut active = HashSet::new();
    let _ = scope_arity(ScopeTree::root(), graph, catalog, &mut active)?;
    Ok(())
}

/// Return an exact output width when every star in a scope can be expanded;
/// otherwise return `None` and leave the eventual merge conservative.
fn scope_arity(
    scope: usize,
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
    active: &mut HashSet<usize>,
) -> Result<Option<usize>, SemanticError> {
    if !active.insert(scope) {
        return Ok(None);
    }
    let result = match graph.scopes.output_plan(scope).clone() {
        OutputPlan::Projection => {
            let mut width = 0;
            let mut exact = true;
            for col in graph.scopes.output_columns(scope) {
                if let RawNode::Star {
                    base,
                    options,
                    scope: star_scope,
                } = &graph.nodes[col.node_id]
                {
                    match star_arity(base, options, *star_scope, graph, catalog, active) {
                        Some(star_width) => width += star_width,
                        None => exact = false,
                    }
                } else {
                    width += 1;
                }
            }
            exact.then_some(width)
        }
        OutputPlan::Delegate(child) => scope_arity(child, graph, catalog, active)?,
        OutputPlan::SetOperation { left, right, .. } => {
            let left_width = scope_arity(left, graph, catalog, active)?;
            let right_width = scope_arity(right, graph, catalog, active)?;
            match (left_width, right_width) {
                (Some(left), Some(right)) if left != right => {
                    return Err(SemanticError::SetOperationArityMismatch { left, right });
                }
                (Some(width), Some(_)) => Some(width),
                _ => None,
            }
        }
    };
    active.remove(&scope);
    Ok(result)
}

fn star_arity(
    base: &StarBase,
    options: &StarOptions,
    scope: usize,
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
    active: &mut HashSet<usize>,
) -> Option<usize> {
    let target = resolve_star_target(base, scope, graph);
    let names = match target {
        StarTarget::All => {
            let mut names = Vec::new();
            for (binding_name, binding) in effective_bindings(scope, graph) {
                let binding_names = binding_column_names(&binding, graph, catalog, active)?;
                let qualifier = [binding_name];
                names.extend(apply_name_options(
                    binding_names,
                    options,
                    Some(base),
                    Some(&qualifier),
                ));
            }
            for &child in graph.scopes.anonymous_derived(scope) {
                let child_names = scope_output_names(child, graph, catalog, active)?;
                names.extend(apply_name_options(child_names, options, Some(base), None));
            }
            Some(names)
        }
        StarTarget::Binding(binding) => binding_column_names(&binding, graph, catalog, active)
            .map(|names| apply_name_options(names, options, Some(base), None)),
        StarTarget::Unknown(table) => catalog
            .and_then(|catalog| catalog.list_columns(&table))
            .map(|names| apply_name_options(names, options, Some(base), None)),
        StarTarget::FieldPath { .. } | StarTarget::Expr => None,
    };
    names.map(|names| names.len())
}

pub(super) enum StarTarget {
    All,
    Binding(Binding),
    Unknown(TableRef),
    FieldPath { binding: Binding, path: Vec<String> },
    Expr,
}

fn binding_column_names(
    binding: &Binding,
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
    active: &mut HashSet<usize>,
) -> Option<Vec<String>> {
    match binding {
        Binding::Table(table) => catalog.and_then(|catalog| catalog.list_columns(table)),
        Binding::Cte(child) | Binding::DerivedTable(child) => {
            scope_output_names(*child, graph, catalog, active)
        }
        Binding::VirtualSource(source) => Some(
            graph
                .scopes
                .virtual_source(*source)
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        ),
    }
}

fn scope_output_names(
    scope: usize,
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
    active: &mut HashSet<usize>,
) -> Option<Vec<String>> {
    if !active.insert(scope) {
        return None;
    }
    let result = match graph.scopes.output_plan(scope) {
        OutputPlan::Projection => {
            let mut names = Vec::new();
            for column in graph.scopes.output_columns(scope) {
                match &graph.nodes[column.node_id] {
                    RawNode::Star {
                        base,
                        options,
                        scope: star_scope,
                    } => {
                        let target = resolve_star_target(base, *star_scope, graph);
                        let child_names = match target {
                            StarTarget::All => {
                                let mut values = Vec::new();
                                for (binding_name, binding) in
                                    effective_bindings(*star_scope, graph)
                                {
                                    let binding_names =
                                        binding_column_names(&binding, graph, catalog, active)?;
                                    let qualifier = [binding_name];
                                    values.extend(apply_name_options(
                                        binding_names,
                                        options,
                                        Some(base),
                                        Some(&qualifier),
                                    ));
                                }
                                Some(values)
                            }
                            StarTarget::Binding(binding) => binding_column_names(
                                &binding, graph, catalog, active,
                            )
                            .map(|names| apply_name_options(names, options, Some(base), None)),
                            StarTarget::Unknown(table) => catalog
                                .and_then(|catalog| catalog.list_columns(&table))
                                .map(|names| apply_name_options(names, options, Some(base), None)),
                            StarTarget::FieldPath { .. } | StarTarget::Expr => None,
                        }?;
                        names.extend(child_names);
                    }
                    _ => names.push(column.name.clone()),
                }
            }
            Some(names)
        }
        OutputPlan::Delegate(child) => scope_output_names(*child, graph, catalog, active),
        OutputPlan::SetOperation { left, .. } => scope_output_names(*left, graph, catalog, active),
    };
    active.remove(&scope);
    result
}

fn apply_name_options(
    mut names: Vec<String>,
    options: &StarOptions,
    base: Option<&StarBase>,
    relation_qualifier: Option<&[String]>,
) -> Vec<String> {
    names.retain(|name| {
        !options.exclude.iter().any(|excluded| {
            excluded_matches_name_with_context(excluded, name, base, relation_qualifier)
        }) && options
            .ilike
            .as_deref()
            .is_none_or(|pattern| ilike_matches(pattern, name))
    });
    for (old, new) in &options.rename {
        if let Some(name) = names.iter_mut().find(|name| same_column_name(name, old)) {
            name.clone_from(new);
        }
    }
    names
}

fn excluded_matches_name_with_context(
    excluded: &StarColumnName,
    name: &str,
    base: Option<&StarBase>,
    relation_qualifier: Option<&[String]>,
) -> bool {
    let Some((excluded_column, qualifier)) = excluded.parts.split_last() else {
        return false;
    };
    same_column_name(name, excluded_column)
        && (qualifier.is_empty()
            || base.is_some_and(
                |base| matches!(base, StarBase::Qualified(parts) if parts == qualifier),
            )
            || relation_qualifier.is_some_and(|parts| parts == qualifier))
}

pub(super) fn same_column_name(left: &str, right: &str) -> bool {
    left == right
}

pub(super) fn ilike_matches(pattern: &str, value: &str) -> bool {
    fn matches(pattern: &[char], value: &[char]) -> bool {
        match pattern.split_first() {
            None => value.is_empty(),
            Some(('%', rest)) => {
                matches(rest, value) || value.first().is_some_and(|_| matches(pattern, &value[1..]))
            }
            Some(('_', rest)) => value
                .split_first()
                .is_some_and(|(_, tail)| matches(rest, tail)),
            Some((part, rest)) => value.split_first().is_some_and(|(value, tail)| {
                part.eq_ignore_ascii_case(value) && matches(rest, tail)
            }),
        }
    }
    matches(
        &pattern.chars().collect::<Vec<_>>(),
        &value.chars().collect::<Vec<_>>(),
    )
}

pub(super) fn resolve_star_target(base: &StarBase, scope: usize, graph: &RawGraph) -> StarTarget {
    match base {
        StarBase::Unqualified => StarTarget::All,
        StarBase::Expr(_) => StarTarget::Expr,
        StarBase::Qualified(parts) => {
            let mut best: Option<(usize, Binding)> = None;
            for (name, binding) in graph.scopes.visible_bindings(scope) {
                let candidates = match &binding {
                    Binding::Table(table) => {
                        let mut values = Vec::new();
                        if let Some(catalog) = &table.catalog {
                            values.push(catalog.clone());
                        }
                        if let Some(schema) = &table.schema {
                            values.push(schema.clone());
                        }
                        values.push(table.table.clone());
                        vec![values, vec![name.clone()]]
                    }
                    _ => vec![vec![name.clone()]],
                };
                for candidate in candidates {
                    if parts.len() >= candidate.len() && parts[..candidate.len()] == candidate[..] {
                        let matched = candidate.len();
                        if best.as_ref().is_none_or(|(length, _)| matched > *length) {
                            best = Some((matched, binding.clone()));
                        }
                    }
                }
            }
            if let Some((matched, binding)) = best {
                if matched == parts.len() {
                    StarTarget::Binding(binding)
                } else {
                    StarTarget::FieldPath {
                        binding,
                        path: parts[matched..].to_vec(),
                    }
                }
            } else {
                StarTarget::Unknown(table_ref_from_parts(parts))
            }
        }
    }
}

fn table_ref_from_parts(parts: &[String]) -> TableRef {
    match parts {
        [table] => TableRef::new(table.clone()),
        [schema, table] => TableRef::with_schema(schema.clone(), table.clone()),
        [catalog, schema, table] => TableRef {
            catalog: Some(catalog.clone()),
            schema: Some(schema.clone()),
            table: table.clone(),
        },
        _ => TableRef::new(parts.join(".")),
    }
}

pub(super) fn effective_bindings(scope: usize, graph: &RawGraph) -> Vec<(String, Binding)> {
    let immediate = graph.scopes.immediate_bindings(scope);
    if immediate.is_empty() {
        graph.scopes.visible_bindings(scope)
    } else {
        immediate
    }
}
