use std::ops::ControlFlow;

use sqlparser::ast::{self, Expr, Query, SelectItem, SetExpr, TableFactor, Visit, Visitor};

use super::{
    Candidate, ColumnOccurrence, Ident, OutputName, OutputSlot, from_sql_ident,
    legacy_component_matches, output_name_matches, relation_from_object_name, sql_ident_matches,
    star_slot,
};

pub(super) fn query_output(query: &Query) -> Vec<OutputSlot> {
    query_output_in(query, &[])
}

fn query_output_in(query: &Query, outer_ctes: &[&ast::Cte]) -> Vec<OutputSlot> {
    let mut ctes = outer_ctes.to_vec();
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            ctes.push(cte);
        }
    }
    set_expr_output(&query.body, &ctes)
}

fn select_output(select: &ast::Select, ctes: &[&ast::Cte]) -> Vec<OutputSlot> {
    let bindings = select
        .from
        .iter()
        .flat_map(|from| {
            std::iter::once(&from.relation).chain(from.joins.iter().map(|join| &join.relation))
        })
        .filter_map(|factor| provider_binding(factor, ctes))
        .collect::<Vec<_>>();
    let single_factor = match select.from.as_slice() {
        [from] if from.joins.is_empty() => Some(&from.relation),
        _ => None,
    };
    let input = single_factor.and_then(|factor| {
        let mut input = factor_output(factor, ctes)?;
        apply_factor_column_aliases(factor, &mut input);
        Some(input)
    });
    let mut output = Vec::new();
    for item in &select.projection {
        let star_matches = match item {
            SelectItem::Wildcard(_) => true,
            SelectItem::QualifiedWildcard(
                ast::SelectItemQualifiedWildcardKind::ObjectName(name),
                _,
            ) => name
                .0
                .last()
                .and_then(|part| part.as_ident())
                .is_some_and(|name| bindings.len() == 1 && bindings[0].matches(Some(&name.value))),
            _ => false,
        };
        if star_matches && let Some(input) = &input {
            output.extend(input.iter().cloned());
            continue;
        }
        for mut slot in select_item_slots(item) {
            for occurrence in &mut slot.source_columns {
                occurrence.scope_candidates = bindings
                    .iter()
                    .filter(|binding| binding.matches(occurrence.qualifier.as_deref()))
                    .map(|binding| binding.candidate.clone())
                    .collect();
                if let [Candidate::Relation(relation)] = occurrence.scope_candidates.as_slice() {
                    occurrence.owner = Some(relation.clone());
                }
            }
            if let Some(input) = &input {
                slot.source_columns = slot
                    .source_columns
                    .into_iter()
                    .flat_map(|occurrence| {
                        if bindings.len() == 1
                            && bindings[0].matches(occurrence.qualifier.as_deref())
                        {
                            let mut matches = input.iter().filter(|source| {
                                output_name_matches(&source.name, &occurrence.ident.value)
                            });
                            if let Some(source) = matches.next()
                                && matches.next().is_none()
                            {
                                return source.source_columns.clone();
                            }
                        }
                        vec![occurrence]
                    })
                    .collect();
            }
            output.push(slot);
        }
    }
    output
}

struct ProviderBinding {
    qualifier: Option<String>,
    candidate: Candidate,
}

impl ProviderBinding {
    fn matches(&self, qualifier: Option<&str>) -> bool {
        qualifier.is_none_or(|qualifier| {
            self.qualifier
                .as_ref()
                .is_some_and(|name| legacy_component_matches(qualifier, name))
        })
    }
}

fn provider_binding(factor: &TableFactor, ctes: &[&ast::Cte]) -> Option<ProviderBinding> {
    let alias = table_factor_alias(factor);
    match factor {
        TableFactor::Table {
            name,
            alias: sql_alias,
            ..
        } => {
            let relation = relation_from_object_name(name)?;
            let cte = (name.0.len() == 1)
                .then(|| {
                    ctes.iter()
                        .rev()
                        .find(|cte| sql_ident_matches(&cte.alias.name, &relation.name))
                })
                .flatten();
            let candidate = if let Some(cte) = cte {
                Candidate::Cte(sql_alias.as_ref().map_or_else(
                    || from_sql_ident(&cte.alias.name),
                    |alias| from_sql_ident(&alias.name),
                ))
            } else {
                Candidate::Relation(relation.clone())
            };
            Some(ProviderBinding {
                qualifier: Some(alias.unwrap_or(&relation.name.value).to_owned()),
                candidate,
            })
        }
        TableFactor::Derived {
            alias: sql_alias, ..
        } => Some(ProviderBinding {
            qualifier: alias.map(str::to_owned),
            candidate: Candidate::Derived(
                sql_alias.as_ref().map(|alias| from_sql_ident(&alias.name)),
            ),
        }),
        _ => None,
    }
}

fn table_factor_alias(factor: &TableFactor) -> Option<&str> {
    match factor {
        TableFactor::Table { alias, .. } | TableFactor::Derived { alias, .. } => {
            alias.as_ref().map(|alias| alias.name.value.as_str())
        }
        _ => None,
    }
}

fn factor_output(factor: &TableFactor, ctes: &[&ast::Cte]) -> Option<Vec<OutputSlot>> {
    match factor {
        TableFactor::Table { name, .. } => {
            if name.0.len() != 1 {
                return None;
            }
            let relation_name = name.0.last()?.as_ident()?;
            let (position, cte) = ctes.iter().enumerate().rev().find(|(_, cte)| {
                sql_ident_matches(&cte.alias.name, &from_sql_ident(relation_name))
            })?;
            let mut output = query_output_in(&cte.query, &ctes[..position]);
            for (slot, column) in output.iter_mut().zip(&cte.alias.columns) {
                slot.name = OutputName::Explicit(from_sql_ident(&column.name));
            }
            Some(output)
        }
        TableFactor::Derived { subquery, .. } => Some(query_output_in(subquery, ctes)),
        _ => None,
    }
}

fn apply_factor_column_aliases(factor: &TableFactor, output: &mut [OutputSlot]) {
    let alias = match factor {
        TableFactor::Table { alias, .. } | TableFactor::Derived { alias, .. } => alias.as_ref(),
        _ => None,
    };
    let Some(alias) = alias else {
        return;
    };
    for (slot, column) in output.iter_mut().zip(&alias.columns) {
        slot.name = OutputName::Explicit(from_sql_ident(&column.name));
    }
}

fn set_expr_output(body: &SetExpr, ctes: &[&ast::Cte]) -> Vec<OutputSlot> {
    match body {
        SetExpr::Select(select) => select_output(select, ctes),
        SetExpr::SetOperation { left, right, .. } => {
            let mut output = set_expr_output(left, ctes);
            let right = set_expr_output(right, ctes);
            for (left_slot, right_slot) in output.iter_mut().zip(right) {
                left_slot.source_columns.extend(right_slot.source_columns);
            }
            output
        }
        SetExpr::Query(query) => query_output_in(query, ctes),
        SetExpr::Values(values) => values
            .rows
            .first()
            .map(|row| row.iter().map(|expr| expression_slot(expr, None)).collect())
            .unwrap_or_default(),
        SetExpr::Table(_)
        | SetExpr::Update(_)
        | SetExpr::Insert(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_) => Vec::new(),
    }
}

fn select_item_slots(item: &SelectItem) -> Vec<OutputSlot> {
    match item {
        SelectItem::UnnamedExpr(expr) => vec![expression_slot(expr, None)],
        SelectItem::ExprWithAlias { expr, alias } => {
            vec![expression_slot(expr, Some(from_sql_ident(alias)))]
        }
        SelectItem::ExprWithAliases { expr, aliases } => aliases
            .iter()
            .map(|alias| expression_slot(expr, Some(from_sql_ident(alias))))
            .collect(),
        SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(_, _) => vec![star_slot()],
    }
}

fn expression_slot(expr: &Expr, explicit: Option<Ident>) -> OutputSlot {
    let row_value = match expr {
        Expr::Identifier(ident) => Some(from_sql_ident(ident)),
        Expr::Nested(inner) => match inner.as_ref() {
            Expr::Identifier(ident) => Some(from_sql_ident(ident)),
            _ => None,
        },
        _ => None,
    };
    let name = if let Some(name) = explicit {
        OutputName::Explicit(name)
    } else if let Some(name) = direct_column_ident(expr) {
        OutputName::Explicit(name)
    } else if let Some(name) = inferred_expression_ident(expr) {
        OutputName::Inferred(name)
    } else {
        OutputName::Anonymous
    };
    OutputSlot {
        name,
        is_star: false,
        row_value,
        source_columns: expression_columns(expr),
    }
}

fn expression_columns(expr: &Expr) -> Vec<ColumnOccurrence> {
    #[derive(Default)]
    struct Collector(Vec<ColumnOccurrence>);

    impl Visitor for Collector {
        type Break = ();

        fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
            match expr {
                Expr::Identifier(column) => self.0.push(ColumnOccurrence {
                    ident: from_sql_ident(column),
                    qualifier: None,
                    owner: None,
                    scope_candidates: Vec::new(),
                }),
                Expr::CompoundIdentifier(parts) => {
                    if let Some(column) = parts.last() {
                        self.0.push(ColumnOccurrence {
                            ident: from_sql_ident(column),
                            qualifier: parts
                                .get(parts.len().saturating_sub(2))
                                .map(|part| part.value.clone()),
                            owner: None,
                            scope_candidates: Vec::new(),
                        });
                    }
                }
                _ => {}
            }
            ControlFlow::Continue(())
        }
    }

    let mut collector = Collector::default();
    let _ = expr.visit(&mut collector);
    collector.0
}

fn direct_column_ident(expr: &Expr) -> Option<Ident> {
    match expr {
        Expr::Identifier(ident) => Some(from_sql_ident(ident)),
        Expr::CompoundIdentifier(parts) => parts.last().map(from_sql_ident),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => direct_column_ident(expr),
        _ => None,
    }
}

fn inferred_expression_ident(expr: &Expr) -> Option<Ident> {
    match expr {
        Expr::Function(function) => function
            .name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .map(from_sql_ident),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => inferred_expression_ident(expr),
        _ => None,
    }
}
