use super::model::RangeRef;

use crate::graph::RawGraph;
use crate::graph::node::{StarBase, StarColumnName};
use crate::graph::scope::Binding;
use crate::keys::ident_parts_equal;
use crate::types::TableRef;
use crate::{Dialect, Ident};

pub(super) enum StarTarget {
    Ambiguous(Vec<crate::Candidate>),
    All,
    Binding { name: Ident, binding: Binding },
    Unknown(TableRef),
    FieldPath { binding: Binding, path: Vec<Ident> },
    Expr,
}

pub(super) fn same_column_name(left: &Ident, right: &Ident, dialect: Dialect) -> bool {
    left.column_key(dialect) == right.column_key(dialect)
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
            let mut best: Option<(usize, Ident, Binding)> = None;
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
                    if parts.len() >= candidate.len()
                        && if candidate.len() == 1 {
                            parts[0].column_key(graph.dialect)
                                == candidate[0].column_key(graph.dialect)
                        } else {
                            ident_parts_equal(&parts[..candidate.len()], &candidate, graph.dialect)
                        }
                    {
                        let matched = candidate.len();
                        if best.as_ref().is_none_or(|(length, _, _)| matched > *length) {
                            best = Some((matched, name.clone(), binding.clone()));
                        }
                    }
                }
            }
            if let Some((matched, name, binding)) = best {
                if matched == 1
                    && let Some(candidates) =
                        graph.scopes.ambiguous_binding_candidates(scope, &parts[0])
                {
                    return StarTarget::Ambiguous(candidates);
                }
                if matched == parts.len() {
                    StarTarget::Binding { name, binding }
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

fn table_ref_from_parts(parts: &[Ident]) -> TableRef {
    match parts {
        [table] => TableRef {
            catalog: None,
            schema: None,
            table: table.clone(),
        },
        [schema, table] => TableRef {
            catalog: None,
            schema: Some(schema.clone()),
            table: table.clone(),
        },
        [catalog, schema, table] => TableRef {
            catalog: Some(catalog.clone()),
            schema: Some(schema.clone()),
            table: table.clone(),
        },
        _ => TableRef::new(
            parts
                .iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>()
                .join("."),
        ),
    }
}

pub(super) fn effective_bindings(scope: usize, graph: &RawGraph) -> Vec<(Ident, Binding)> {
    let immediate = graph.scopes.immediate_bindings(scope);
    if immediate.is_empty() {
        graph.scopes.visible_bindings(scope)
    } else {
        immediate
    }
}

/// EXCLUDE resolves range variables, independently of value lineage.
pub(super) fn excluded_matches_name(
    excluded: &StarColumnName,
    name: &Ident,
    base: Option<&StarBase>,
    range_variables: &[RangeRef],
    dialect: Dialect,
) -> bool {
    let Some((excluded_column, qualifier)) = excluded.parts.split_last() else {
        return false;
    };
    same_column_name(name, excluded_column, dialect)
        && (qualifier.is_empty()
            || matches!(qualifier, [name] if range_variables.iter().any(|variable| same_column_name(&variable.name, name, dialect)))
            || base.is_some_and(|base| match base {
                StarBase::Qualified(parts) => match (parts.as_slice(), qualifier) {
                    ([left], [right]) => same_column_name(left, right, dialect),
                    _ => ident_parts_equal(parts, qualifier, dialect),
                },
                _ => false,
            }))
}
