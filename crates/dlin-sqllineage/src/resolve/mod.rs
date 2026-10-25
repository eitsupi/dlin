mod catalog;
mod topo;

#[cfg(test)]
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::graph::RawGraph;
use crate::graph::edge::EdgeKind;
use crate::graph::node::{NodeId, RawNode, StarBase, StarColumnName, StarOptions};
use crate::graph::scope::{Binding, OutputPlan, ScopeTree, VirtualColumnState, VirtualSourceId};
use crate::types::{
    AnalyzeResult, CatalogProvider, ColumnLineage, ColumnMapping, ColumnOrigin, ColumnRef,
    ParseError, StatementType, TableRef, TransformKind, Warning, WarningKind,
};

/// Resolve `RawGraph` into `AnalyzeResult`.
#[allow(clippy::if_not_else, clippy::useless_let_if_seq)]
pub(crate) fn resolve(
    mut graph: RawGraph,
    catalog: Option<&dyn CatalogProvider>,
    mut warnings: Vec<Warning>,
    statement_type: StatementType,
) -> Result<AnalyzeResult, ParseError> {
    graph.tables.inputs.sort();
    graph.tables.inputs.dedup();

    if graph.nodes.is_empty() {
        return Ok(AnalyzeResult {
            statement_type,
            tables: graph.tables,
            columns: ColumnLineage {
                mappings: Vec::new(),
                has_unresolved_stars: false,
            },
            warnings,
        });
    }

    if topo::topological_sort(&graph.nodes, &graph.edges).is_err() {
        warnings.push(Warning {
            kind: WarningKind::UnexpectedCycle,
            location: None,
        });
        return Ok(AnalyzeResult {
            statement_type,
            tables: graph.tables,
            columns: ColumnLineage {
                mappings: Vec::new(),
                has_unresolved_stars: false,
            },
            warnings,
        });
    }

    validate_set_arities(&graph, catalog)?;
    let has_unresolved_stars = graph_has_unresolved_stars(&graph, catalog);

    let mut incoming: Vec<Vec<usize>> = vec![vec![]; graph.nodes.len()];
    for (idx, edge) in graph.edges.iter().enumerate() {
        incoming[edge.to].push(idx);
    }

    let mut resolved: Vec<Option<ColumnOrigin>> = vec![None; graph.nodes.len()];

    let root = ScopeTree::root();
    let output_table = graph.tables.output.clone();
    let mut mapping_cache = ScopeMappingCache::default();
    // Scope mappings are cached in canonical form without an output table.
    // Internal CTE/derived references need that form, while the root output
    // table is only presentation metadata and is attached once here. Keeping
    // it out of the cache key avoids materializing the same scope once per
    // output column and cannot alter source resolution.
    let mut mappings = resolve_scope_mappings(
        root,
        &graph,
        &mut resolved,
        &incoming,
        catalog,
        &mut mapping_cache,
    )
    .iter()
    .cloned()
    .collect::<Vec<_>>();
    for mapping in &mut mappings {
        mapping.target.table.clone_from(&output_table);
    }

    Ok(AnalyzeResult {
        statement_type,
        tables: graph.tables,
        columns: ColumnLineage {
            mappings,
            has_unresolved_stars,
        },
        warnings,
    })
}

/// Inspect the complete graph rather than only the root projection. A star
/// under a JOIN/CTE/derived-table boundary is still an unresolved schema
/// dependency and must be visible to consumers of the public result.
fn graph_has_unresolved_stars(graph: &RawGraph, catalog: Option<&dyn CatalogProvider>) -> bool {
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

const SET_ARITY_ERROR_PREFIX: &str = "set operation arity mismatch";

fn validate_set_arities(
    graph: &RawGraph,
    catalog: Option<&dyn CatalogProvider>,
) -> Result<(), ParseError> {
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
) -> Result<Option<usize>, ParseError> {
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
                    return Err(ParseError {
                        message: format!(
                            "{SET_ARITY_ERROR_PREFIX}: left has {left} columns, right has {right} columns"
                        ),
                    });
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

enum StarTarget {
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

fn same_column_name(left: &str, right: &str) -> bool {
    left == right
}

fn ilike_matches(pattern: &str, value: &str) -> bool {
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

fn resolve_star_target(base: &StarBase, scope: usize, graph: &RawGraph) -> StarTarget {
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

fn effective_bindings(scope: usize, graph: &RawGraph) -> Vec<(String, Binding)> {
    let immediate = graph.scopes.immediate_bindings(scope);
    if immediate.is_empty() {
        graph.scopes.visible_bindings(scope)
    } else {
        immediate
    }
}

fn wildcard_mapping(output_table: Option<&TableRef>, source_table: TableRef) -> ColumnMapping {
    ColumnMapping {
        target: ColumnRef {
            table: output_table.cloned(),
            column: "*".to_string(),
        },
        sources: vec![ColumnOrigin::Wildcard {
            table: source_table,
        }],
        transform: TransformKind::Direct,
    }
}

#[allow(clippy::too_many_arguments)]
fn expand_virtual_source(
    source: VirtualSourceId,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
    mappings: &mut Vec<ColumnMapping>,
) {
    for column in &graph.scopes.virtual_source(source).columns {
        let mut origins = Vec::new();
        let mut visited = HashSet::new();
        for &dependency in &column.dependencies {
            let (dependency_origins, _, _) = collect_leaf_origins(
                dependency,
                graph,
                resolved,
                incoming,
                &mut visited,
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
        mappings.push(ColumnMapping {
            target: ColumnRef {
                table: None,
                column: column.name.clone(),
            },
            sources: origins,
            transform: TransformKind::Direct,
        });
    }
}

fn known_empty_dependency(node_id: NodeId, graph: &RawGraph) -> bool {
    let (name, binding) = match &graph.nodes[node_id] {
        RawNode::Ref { name, binding, .. } | RawNode::Unqualified { name, binding, .. } => {
            (name, binding.as_ref())
        }
        _ => return false,
    };
    let Some(Binding::VirtualSource(source)) = binding else {
        return false;
    };
    let Some(column) = graph
        .scopes
        .virtual_source(*source)
        .columns
        .iter()
        .find(|column| column.name == *name)
    else {
        return false;
    };
    matches!(column.state, VirtualColumnState::KnownEmpty)
        && column
            .dependencies
            .iter()
            .all(|&dependency| known_empty_dependency(dependency, graph))
}

#[derive(Default)]
struct ScopeMappingCache {
    entries: HashMap<usize, ScopeMappingEntry>,
}

enum ScopeMappingEntry {
    Computing,
    Resolved(Arc<[ColumnMapping]>),
}

#[cfg(test)]
thread_local! {
    static SCOPE_MAPPING_COMPUTATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
fn reset_scope_mapping_stats() {
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(0));
}

#[cfg(test)]
fn scope_mapping_computations() -> usize {
    SCOPE_MAPPING_COMPUTATIONS.with(Cell::get)
}

/// Resolve a scope's output plan into ordered mappings. Set-operation
/// branches are resolved independently so catalog expansion happens before
/// their positional merge.
#[allow(clippy::too_many_arguments)]
fn resolve_scope_mappings(
    scope: usize,
    graph: &RawGraph,
    resolved: &mut Vec<Option<ColumnOrigin>>,
    incoming: &[Vec<usize>],
    catalog: Option<&dyn CatalogProvider>,
    mapping_cache: &mut ScopeMappingCache,
) -> Arc<[ColumnMapping]> {
    if let Some(entry) = mapping_cache.entries.get(&scope) {
        return match entry {
            ScopeMappingEntry::Resolved(mappings) => mappings.clone(),
            // A recursive scope cannot safely publish a partially materialized
            // result. Returning no mappings preserves the existing fallback to
            // raw output resolution and, importantly, does not cache an
            // incomplete entry as resolved.
            ScopeMappingEntry::Computing => Arc::from([]),
        };
    }
    mapping_cache
        .entries
        .insert(scope, ScopeMappingEntry::Computing);
    #[cfg(test)]
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(computations.get() + 1));

    let mappings = match graph.scopes.output_plan(scope).clone() {
        OutputPlan::Projection => {
            let mut mappings = Vec::new();
            for col in graph.scopes.output_columns(scope) {
                match &graph.nodes[col.node_id] {
                    RawNode::Output { name, .. } => {
                        let mut visited = HashSet::new();
                        let (sources, edge_kinds, has_back, inherited_transform) =
                            collect_output_sources(
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
                                column: name.clone(),
                            },
                            sources: if has_back {
                                vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }]
                            } else {
                                sources
                            },
                            transform,
                        });
                    }
                    RawNode::Star {
                        base,
                        options,
                        scope,
                    } => expand_star(
                        base,
                        options,
                        *scope,
                        graph,
                        resolved,
                        incoming,
                        catalog,
                        mapping_cache,
                        &mut mappings,
                        &mut HashSet::new(),
                    ),
                    _ => {}
                }
            }
            if let Some(cat) = catalog {
                catalog::apply_catalog(&mut mappings, cat);
            }
            mappings
        }
        OutputPlan::Delegate(child) => {
            resolve_scope_mappings(child, graph, resolved, incoming, catalog, mapping_cache)
                .iter()
                .cloned()
                .collect()
        }
        OutputPlan::SetOperation {
            left,
            right,
            recursive,
        } => {
            let left_mappings =
                resolve_scope_mappings(left, graph, resolved, incoming, catalog, mapping_cache)
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>();
            let right_mappings = if recursive {
                Vec::new()
            } else {
                resolve_scope_mappings(right, graph, resolved, incoming, catalog, mapping_cache)
                    .iter()
                    .cloned()
                    .collect()
            };
            // A wildcard without catalog metadata is a variable-width slot.
            // Positional alignment at or after it would fabricate an ordinal
            // and lose the remaining branch columns. Merge only the exact
            // prefix before the first wildcard, then retain both tails and
            // expose the Wildcard origin.
            if !recursive
                && (mappings_have_unknown_shape(&left_mappings)
                    || mappings_have_unknown_shape(&right_mappings))
            {
                merge_unknown_shape_mappings(left_mappings, right_mappings)
            } else {
                // The branch resolvers expand stars independently. Preserve the
                // left branch's names and order, as SQL set operations do.
                let mut merged = Vec::with_capacity(left_mappings.len());
                for (idx, left_mapping) in left_mappings.into_iter().enumerate() {
                    if let Some(right_mapping) = right_mappings.get(idx) {
                        let (sources, transform) =
                            merge_branch_sources(&left_mapping, right_mapping);
                        if recursive {
                            merged.push(ColumnMapping {
                                target: left_mapping.target,
                                sources: vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }],
                                transform,
                            });
                        } else {
                            merged.push(ColumnMapping {
                                target: left_mapping.target,
                                sources,
                                transform,
                            });
                        }
                    } else {
                        let sources = left_mapping.sources;
                        let transform = left_mapping.transform;
                        if recursive {
                            merged.push(ColumnMapping {
                                target: left_mapping.target,
                                sources: vec![ColumnOrigin::Recursive {
                                    base_sources: sources,
                                }],
                                transform,
                            });
                        } else {
                            merged.push(ColumnMapping {
                                target: left_mapping.target,
                                sources,
                                transform,
                            });
                        }
                    }
                }
                merged
            }
        }
    };
    let mappings: Arc<[ColumnMapping]> = mappings.into();
    mapping_cache
        .entries
        .insert(scope, ScopeMappingEntry::Resolved(mappings.clone()));
    mappings
}

fn mappings_have_unknown_shape(mappings: &[ColumnMapping]) -> bool {
    mappings.iter().any(|mapping| {
        // A named output that happens to flow through an unresolved star has
        // a known ordinal/name and must not act as a variable-width barrier.
        mapping.target.column == "*"
            && mapping.sources.iter().any(|source| match source {
                ColumnOrigin::Wildcard { .. } => true,
                ColumnOrigin::Ambiguous { column, .. } if column == "*" => true,
                ColumnOrigin::Recursive { base_sources } => base_sources.iter().any(|source| {
                    matches!(source, ColumnOrigin::Wildcard { .. })
                        || matches!(source, ColumnOrigin::Ambiguous { column, .. } if column == "*")
                }),
                _ => false,
            })
    })
}

fn merge_unknown_shape_mappings(
    left: Vec<ColumnMapping>,
    right: Vec<ColumnMapping>,
) -> Vec<ColumnMapping> {
    let left_barrier = first_unknown_mapping(&left).unwrap_or(left.len());
    let right_barrier = first_unknown_mapping(&right).unwrap_or(right.len());

    // A leading unknown star determines the output names for the set
    // operation. When the left side is already a merged set (and therefore
    // contains named slots contributed by an earlier operand), a right-only
    // tail would publish names that the leading operand never declared. Keep
    // the left candidates and their existing wildcard provenance; a direct
    // two-branch `SELECT * UNION SELECT a, b` still retains the right branch
    // names because there are no prior named slots to preserve.
    if left_barrier == 0 && left.len() > 1 && right_barrier == right.len() {
        return left;
    }

    let prefix_len = left_barrier.min(right_barrier);
    let left_unknown = wildcard_sources(&left);
    let right_unknown = wildcard_sources(&right);
    let mut merged = Vec::with_capacity(left.len() + right.len() - prefix_len);

    for (left_mapping, right_mapping) in left.iter().zip(right.iter()).take(prefix_len) {
        let (sources, transform) = merge_branch_sources(left_mapping, right_mapping);
        merged.push(ColumnMapping {
            target: left_mapping.target.clone(),
            sources,
            transform,
        });
    }
    merged.extend(
        left.into_iter()
            .skip(prefix_len)
            .map(|mapping| append_unknown_sources(mapping, &right_unknown)),
    );
    merged.extend(
        right
            .into_iter()
            .skip(prefix_len)
            .map(|mapping| append_unknown_sources(mapping, &left_unknown)),
    );
    merged
}

/// Merge two positional branch mappings while retaining the fact that one
/// branch was source-free. An empty source list is meaningful for literals;
/// dropping it would overclaim complete lineage from the other branch.
fn merge_branch_sources(
    left: &ColumnMapping,
    right: &ColumnMapping,
) -> (Vec<ColumnOrigin>, TransformKind) {
    let mut sources = left.sources.clone();
    sources.extend(right.sources.clone());
    if left.sources.is_empty() != right.sources.is_empty() {
        sources.push(ColumnOrigin::SourceFree {
            column: left.target.column.clone(),
        });
    }
    (sources, merge_transform(&left.transform, &right.transform))
}

fn first_unknown_mapping(mappings: &[ColumnMapping]) -> Option<usize> {
    mappings
        .iter()
        .position(|mapping| mappings_have_unknown_shape(std::slice::from_ref(mapping)))
}

fn wildcard_sources(mappings: &[ColumnMapping]) -> Vec<ColumnOrigin> {
    let mut sources = Vec::new();
    for mapping in mappings {
        for source in &mapping.sources {
            match source {
                ColumnOrigin::Wildcard { .. } => sources.push(source.clone()),
                ColumnOrigin::Ambiguous { column, .. } if column == "*" => {
                    sources.push(source.clone());
                }
                ColumnOrigin::Recursive { base_sources } => sources.extend(
                    base_sources
                        .iter()
                        .filter(|source| {
                            matches!(source, ColumnOrigin::Wildcard { .. })
                                || matches!(source, ColumnOrigin::Ambiguous { column, .. } if column == "*")
                        })
                        .cloned(),
                ),
                _ => {}
            }
        }
    }
    sources
}

fn append_unknown_sources(
    mut mapping: ColumnMapping,
    unknown_sources: &[ColumnOrigin],
) -> ColumnMapping {
    if mapping.sources.is_empty() && !unknown_sources.is_empty() {
        mapping.sources.push(ColumnOrigin::SourceFree {
            column: mapping.target.column.clone(),
        });
    }
    mapping.sources.extend(unknown_sources.iter().cloned());
    mapping
}

fn merge_transform(left: &TransformKind, right: &TransformKind) -> TransformKind {
    if matches!(left, TransformKind::Aggregation) || matches!(right, TransformKind::Aggregation) {
        TransformKind::Aggregation
    } else if matches!(left, TransformKind::Conditional)
        || matches!(right, TransformKind::Conditional)
    {
        TransformKind::Conditional
    } else if matches!(left, TransformKind::Expression)
        || matches!(right, TransformKind::Expression)
    {
        TransformKind::Expression
    } else if matches!(left, TransformKind::Window) || matches!(right, TransformKind::Window) {
        TransformKind::Window
    } else if matches!(left, TransformKind::Unknown) || matches!(right, TransformKind::Unknown) {
        TransformKind::Unknown
    } else {
        TransformKind::Direct
    }
}

/// Expand a Star node (qualified or unqualified) into `ColumnMapping`s.
#[allow(clippy::too_many_arguments)]
fn expand_star(
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

fn table_matches_qualifier(table: &TableRef, qualifier: &[String]) -> bool {
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

/// Recursively expand a scope's output columns into `ColumnMapping`s.
#[allow(clippy::too_many_arguments)]
fn expand_scope_columns(
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

fn collect_output_sources(
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

fn collect_leaf_origins(
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

    if let Some((sources, has_back, transform)) =
        resolve_named_scope_reference(node_id, graph, resolved, incoming, catalog, mapping_cache)
    {
        return (sources, has_back, transform);
    }

    if let Some((target_output, scope)) = find_cte_redirect(node_id, graph) {
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
                    qualifier
                        .as_deref()
                        .and_then(|qualifier| graph.scopes.lookup(*scope, qualifier).cloned())
                })
                .or_else(|| {
                    graph
                        .scopes
                        .lookup(*scope, name)
                        .filter(|binding| matches!(binding, Binding::VirtualSource(_)))
                        .cloned()
                }),
            *scope,
        ),
        RawNode::Unqualified {
            name,
            scope,
            binding,
        } => (
            name,
            binding.clone().or_else(|| {
                graph
                    .scopes
                    .lookup(*scope, name)
                    .filter(|binding| matches!(binding, Binding::VirtualSource(_)))
                    .cloned()
            }),
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
    name: &str,
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
        .find(|column| column.name == name)?;
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

fn virtual_column_origin(
    name: &str,
    source: VirtualSourceId,
    graph: &RawGraph,
) -> Option<ColumnOrigin> {
    let column = graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .find(|column| column.name == name)?;
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
    .or_else(|| qualifier.and_then(|qualifier| graph.scopes.lookup(scope, qualifier).cloned()))
    .or_else(|| find_single_binding(scope, graph));
    let Some(Binding::Cte(target_scope) | Binding::DerivedTable(target_scope)) = binding else {
        return None;
    };
    let mappings = resolve_scope_mappings(
        target_scope,
        graph,
        resolved,
        incoming,
        catalog,
        mapping_cache,
    );
    if let Some(mapping) = mappings
        .iter()
        .find(|mapping| mapping.target.column == *name)
    {
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
fn named_wildcard_sources_for_scope(
    scope: usize,
    column: &str,
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
                let source_column = match star_name_decision(options, column, Some(base)) {
                    StarNameDecision::Allowed => column.to_string(),
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
                    column: column.to_string(),
                    candidates: Vec::new(),
                });
            } else {
                sources.extend(right_sources);
            }
            sources
        }
    }
}

fn named_wildcard_sources_for_star(
    mappings: &[ColumnMapping],
    column: &str,
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
                            column: column.to_string(),
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
                                    column: column.to_string(),
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
            column: column.to_string(),
            candidates: Vec::new(),
        });
    }
    sources
}

fn excluded_matches_source(
    excluded: &StarColumnName,
    column: &str,
    source: &ColumnOrigin,
    base: &StarBase,
    scope: usize,
    graph: &RawGraph,
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
    if qualifier.len() == 1
        && relation_from_origin(source).is_some_and(|table| {
            graph.scopes.visible_bindings(scope).iter().any(|(name, binding)| {
                name == &qualifier[0]
                    && matches!(binding, Binding::Table(binding_table) if binding_table == table)
            })
        })
    {
        return true;
    }
    relation_from_origin(source).is_some_and(|table| table_matches_qualifier(table, qualifier))
}

fn relation_from_origin(source: &ColumnOrigin) -> Option<&TableRef> {
    match source {
        ColumnOrigin::Wildcard { table }
        | ColumnOrigin::Concrete { table, .. }
        | ColumnOrigin::NamedWildcard { table, .. } => Some(table),
        _ => None,
    }
}

fn flatten_mapping_sources(sources: &[ColumnOrigin]) -> (Vec<ColumnOrigin>, bool) {
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

#[derive(Clone)]
enum StarNameDecision {
    Allowed,
    Denied,
    Renamed(String),
    Replaced(NodeId),
    Ambiguous,
}

fn star_name_decision(
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

fn scope_star_name_decision(
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

fn resolve_node(
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
                qualifier
                    .as_deref()
                    .and_then(|qual| graph.scopes.lookup(*scope, qual).cloned())
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
            } else if let Some(qual) = qualifier {
                Some(ColumnOrigin::Concrete {
                    table: TableRef::new(qual.as_str()),
                    column: name.clone(),
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
            if let Some(binding) = binding.clone().or_else(|| {
                graph
                    .scopes
                    .lookup(*scope, name)
                    .filter(|binding| matches!(binding, Binding::VirtualSource(_)))
                    .cloned()
            }) {
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

#[allow(clippy::too_many_arguments)]
fn resolve_row_value_candidate(
    name: &str,
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
            column: name.to_string(),
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
                    column: name.to_string(),
                })
            } else {
                Some(ColumnOrigin::Ambiguous {
                    column: name.to_string(),
                    candidates: Vec::new(),
                })
            }
        }
        Binding::Cte(target_scope) | Binding::DerivedTable(target_scope) => {
            let is_named_column = graph
                .scopes
                .output_columns(target_scope)
                .iter()
                .any(|column| column.name == name);
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
                    column: name.to_string(),
                    candidates: Vec::new(),
                })
            }
        }
        Binding::VirtualSource(_) => Some(ColumnOrigin::Ambiguous {
            column: name.to_string(),
            candidates: Vec::new(),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_captured_binding(
    name: &str,
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
            column: name.to_string(),
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

fn find_cte_redirect(node_id: NodeId, graph: &RawGraph) -> Option<(NodeId, usize)> {
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
                    qualifier
                        .as_deref()
                        .and_then(|qual| graph.scopes.lookup(*scope, qual).cloned())
                })
                .or_else(|| find_single_binding(*scope, graph));
            match binding {
                Some(Binding::Cte(s) | Binding::DerivedTable(s)) => graph
                    .scopes
                    .output_columns(s)
                    .iter()
                    .find(|c| c.name == *name)
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
                .or_else(|| find_single_binding(*scope, graph));
            match binding {
                Some(Binding::Cte(s) | Binding::DerivedTable(s)) => graph
                    .scopes
                    .output_columns(s)
                    .iter()
                    .find(|c| c.name == *name)
                    .map(|c| (c.node_id, s)),
                _ => None,
            }
        }
        _ => None,
    }
}

fn find_single_binding(scope: usize, graph: &RawGraph) -> Option<Binding> {
    let bindings = effective_bindings(scope, graph);
    if bindings.len() == 1 {
        Some(bindings[0].1.clone())
    } else {
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_unqualified(
    name: &str,
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
    name: &str,
    bindings: &[(String, Binding)],
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
                column: name.to_string(),
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
            column: name.to_string(),
            candidates: Vec::new(),
        })
    } else {
        let mut table_candidates = Vec::new();
        for (_, binding) in bindings {
            match binding {
                Binding::Cte(s) | Binding::DerivedTable(s) => {
                    if graph
                        .scopes
                        .output_columns(*s)
                        .iter()
                        .any(|c| c.name == name)
                    {
                        return resolve_through_scope(
                            name,
                            *s,
                            graph,
                            resolved,
                            incoming,
                            visited,
                            catalog,
                            mapping_cache,
                        );
                    }
                }
                Binding::Table(t) => table_candidates.push(t.clone()),
                Binding::VirtualSource(_) => {}
            }
        }
        if table_candidates.len() == 1 {
            Some(ColumnOrigin::Concrete {
                table: table_candidates.into_iter().next().unwrap(),
                column: name.to_string(),
            })
        } else {
            Some(ColumnOrigin::Ambiguous {
                column: name.to_string(),
                candidates: table_candidates,
            })
        }
    }
}

fn virtual_has_column(name: &str, source: VirtualSourceId, graph: &RawGraph) -> bool {
    graph
        .scopes
        .virtual_source(source)
        .columns
        .iter()
        .any(|column| column.name == name)
}

fn find_virtual_sources_for_column(
    scope: usize,
    name: &str,
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
    column_name: &str,
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
    if let Some(mapping) = mappings
        .iter()
        .find(|mapping| mapping.target.column == column_name)
    {
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
                    column: column_name.to_string(),
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
                        column: column_name.to_string(),
                        candidates: Vec::new(),
                    })
                };
            }
            StarNameDecision::Renamed(old) => old,
            StarNameDecision::Allowed => column_name.to_string(),
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
        .find(|c| c.name == column_name)
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
            column: column_name.to_string(),
            candidates: Vec::new(),
        })
    }
}

fn derive_transform(node: &RawNode, edge_kinds: &[EdgeKind]) -> TransformKind {
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

#[cfg(test)]
mod tests {
    use super::{reset_scope_mapping_stats, scope_mapping_computations};
    use crate::analyze;
    use crate::types::{AnalyzeOptions, ColumnOrigin, Dialect};

    #[test]
    fn explicit_projection_reuses_scope_mappings() {
        let columns = (0..10).map(|index| format!("c{index}")).collect::<Vec<_>>();
        let names = columns.join(", ");
        let base_projection = (0..10)
            .map(|index| format!("id + 1 AS c{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "WITH base AS (SELECT {base_projection} FROM external_table), \
             cte0 AS (SELECT {names} FROM base), \
             cte1 AS (SELECT {names} FROM cte0), \
             cte2 AS (SELECT {names} FROM cte1), \
             cte3 AS (SELECT {names} FROM cte2), \
             cte4 AS (SELECT {names} FROM cte3) \
             SELECT {names} FROM cte4"
        );

        reset_scope_mapping_stats();
        let results = analyze(
            &sql,
            AnalyzeOptions {
                dialect: Dialect::Generic,
                ..Default::default()
            },
        )
        .expect("explicit projection should resolve");

        assert_eq!(results.len(), 1);
        let mappings = &results[0].columns.mappings;
        assert_eq!(mappings.len(), 10);
        assert!(mappings.iter().all(|mapping| {
            mapping.sources.iter().any(|source| {
                matches!(
                    source,
                    ColumnOrigin::Concrete { table, column }
                        if table.table == "external_table" && column == "id"
                )
            })
        }));
        // One materialization per scope, independent of the ten requested
        // columns. The exact scope count is an implementation detail, but it
        // must remain bounded by the five wrappers plus the base and root.
        assert!(scope_mapping_computations() <= 8);
    }
}
