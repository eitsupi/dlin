use std::collections::HashSet;

use polyglot_sql::lineage::LineageNode;
use polyglot_sql::scope::SourceKind;
use polyglot_sql::{DialectType, Expression};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TraceKey {
    Name,
    SetOrdinal(usize),
}

#[derive(Debug, Clone, Default)]
struct ProjectionKnowledge {
    explicit_names: Vec<String>,
    excluded_names: Vec<String>,
    unresolved_star_ordinal: Option<usize>,
}

/// Classifies projections before the lineage graph is walked.  A star left in
/// a CTE or set operand is only a dependency of that projection, not a failure
/// of an explicit projection at the query boundary.
pub(super) struct StarGuard {
    projections: Vec<(Expression, ProjectionKnowledge)>,
    cte_names: HashSet<String>,
    dialect: Option<DialectType>,
}

/// Expands CTE stars in set operands because polyglot-sql currently expands
/// only the leftmost set operand (upstream issue #368). TODO: remove this
/// compensation layer once the upstream expansion covers every operand.
pub(super) fn expand_known_cte_stars(expr: &mut Expression, dialect: DialectType) {
    let mut outputs = std::collections::HashMap::new();
    collect_cte_outputs(expr, &mut outputs, dialect);
    expand_cte_star_nodes(expr, &outputs, dialect);
}

fn collect_cte_outputs(
    expr: &Expression,
    outputs: &mut std::collections::HashMap<String, Vec<String>>,
    dialect: DialectType,
) {
    if let Some(select) = unwrap_select(expr)
        && let Some(with) = &select.with
    {
        for cte in &with.ctes {
            let names = unwrap_select(&cte.this)
                .map(|body| {
                    body.expressions
                        .iter()
                        .filter_map(explicit_output_name)
                        .map(|name| polyglot_sql::normalize_name(name, Some(dialect), false, true))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if !names.is_empty() {
                outputs.insert(cte.alias.name.to_lowercase(), names);
            }
            collect_cte_outputs(&cte.this, outputs, dialect);
        }
    }
    match expr {
        Expression::Union(set) => {
            collect_cte_outputs(&set.left, outputs, dialect);
            collect_cte_outputs(&set.right, outputs, dialect);
        }
        Expression::Intersect(set) => {
            collect_cte_outputs(&set.left, outputs, dialect);
            collect_cte_outputs(&set.right, outputs, dialect);
        }
        Expression::Except(set) => {
            collect_cte_outputs(&set.left, outputs, dialect);
            collect_cte_outputs(&set.right, outputs, dialect);
        }
        Expression::Subquery(node) => collect_cte_outputs(&node.this, outputs, dialect),
        Expression::Paren(node) => collect_cte_outputs(&node.this, outputs, dialect),
        Expression::Annotated(node) => collect_cte_outputs(&node.this, outputs, dialect),
        _ => {}
    }
}

fn expand_cte_star_nodes(
    expr: &mut Expression,
    outputs: &std::collections::HashMap<String, Vec<String>>,
    dialect: DialectType,
) {
    match expr {
        Expression::Select(select) => {
            if let Some(from) = &select.from
                && let Some(Expression::Table(table)) = from.expressions.first()
                && let Some(names) = outputs.get(&table.name.name.to_lowercase())
            {
                let mut expanded = Vec::new();
                for expression in select.expressions.drain(..) {
                    if expression_is_star(&expression) {
                        expanded.extend(
                            names.iter().filter_map(|name| {
                                parse_projection(&table.name.name, name, dialect)
                            }),
                        );
                    } else {
                        expanded.push(expression);
                    }
                }
                select.expressions = expanded;
            }
            if let Some(with) = &mut select.with {
                for cte in &mut with.ctes {
                    expand_cte_star_nodes(&mut cte.this, outputs, dialect);
                }
            }
        }
        Expression::Union(set) => {
            expand_cte_star_nodes(&mut set.left, outputs, dialect);
            expand_cte_star_nodes(&mut set.right, outputs, dialect);
        }
        Expression::Intersect(set) => {
            expand_cte_star_nodes(&mut set.left, outputs, dialect);
            expand_cte_star_nodes(&mut set.right, outputs, dialect);
        }
        Expression::Except(set) => {
            expand_cte_star_nodes(&mut set.left, outputs, dialect);
            expand_cte_star_nodes(&mut set.right, outputs, dialect);
        }
        Expression::Subquery(node) => expand_cte_star_nodes(&mut node.this, outputs, dialect),
        Expression::Paren(node) => expand_cte_star_nodes(&mut node.this, outputs, dialect),
        Expression::Annotated(node) => expand_cte_star_nodes(&mut node.this, outputs, dialect),
        _ => {}
    }
}

impl StarGuard {
    pub(super) fn new(expr: &Expression, dialect: Option<DialectType>) -> Self {
        let mut projections = Vec::new();
        let mut cte_names = HashSet::new();
        visit_queries(expr, &mut |query| {
            if let Some(select) = unwrap_select(query)
                && let Some(with) = &select.with
            {
                cte_names.extend(with.ctes.iter().map(|cte| cte.alias.name.to_lowercase()));
            }
            let knowledge = projection_knowledge(query, dialect);
            projections.push((query.clone(), knowledge));
        });
        Self {
            projections,
            cte_names,
            dialect,
        }
    }

    pub(super) fn rejects(&self, node: &LineageNode) -> bool {
        let name = node
            .reference_node_name
            .rsplit_once('.')
            .map_or(node.name.as_str(), |(_, name)| name);
        if name == "*" {
            return node.downstream.iter().any(|child| self.rejects(child));
        }
        let knowledge = self
            .knowledge(&node.source)
            .cloned()
            .unwrap_or_else(|| projection_knowledge(&node.source, self.dialect));
        let key = if is_set_operation(&node.source) {
            TraceKey::SetOrdinal(
                explicit_ordinal(&node.source, name, self.dialect)
                    .or_else(|| synthetic_ordinal(name))
                    .unwrap_or(usize::MAX),
            )
        } else {
            TraceKey::Name
        };
        let here = (matches!(key, TraceKey::Name)
            && knowledge.unresolved_star_ordinal.is_some()
            && !knowledge.explicit_names.iter().any(|output| {
                polyglot_sql::normalize_name(output, self.dialect, false, true)
                    == polyglot_sql::normalize_name(name, self.dialect, false, true)
            })
            && matches!(node.source_kind, SourceKind::Unknown | SourceKind::Root)
            && matches!(node.expression, Expression::Column(ref c) if c.table.is_some()))
            || (matches!(key, TraceKey::Name)
                && matches!(node.source_kind, SourceKind::Unknown | SourceKind::Root)
                && knowledge.unresolved_star_ordinal.is_some()
                && !knowledge.explicit_names.iter().any(|output| {
                    polyglot_sql::normalize_name(output, self.dialect, false, true)
                        == polyglot_sql::normalize_name(name, self.dialect, false, true)
                }))
            || (matches!(key, TraceKey::SetOrdinal(ordinal) if ordinal != usize::MAX)
                && !set_has_explicit_name(&node.source, name, self.dialect)
                && set_branch_depends_on_unknown_star(
                    &node.source,
                    match key {
                        TraceKey::SetOrdinal(n) => n,
                        TraceKey::Name => 0,
                    },
                    &self.cte_names,
                ));
        here || node.downstream.iter().any(|child| self.rejects(child))
    }

    pub(super) fn materialize_set_star(
        &self,
        expr: &Expression,
        name: &str,
        dialect: DialectType,
    ) -> Option<Expression> {
        if !contains_set_operation(expr) {
            return None;
        }
        let mut candidate = expr.clone();
        if !materialize_set_stars(&mut candidate, name, dialect) {
            return None;
        }
        Some(candidate)
    }

    pub(super) fn synthetic_set_lineage(
        &self,
        expr: &Expression,
        name: &str,
        dialect: DialectType,
    ) -> Option<LineageNode> {
        if !contains_set_operation(expr)
            || !set_has_explicit_name_anywhere(expr, name, self.dialect)
        {
            return None;
        }
        let Expression::Select(select) =
            polyglot_sql::parse_one(&format!("SELECT synthetic_source.{name}"), dialect).ok()?
        else {
            return None;
        };
        let source = select.from.as_ref()?.expressions.first()?.clone();
        let expression = select.expressions.into_iter().next()?;
        Some(LineageNode::new(
            format!("synthetic_source.{name}"),
            expression,
            source,
        ))
    }

    fn knowledge(&self, expr: &Expression) -> Option<&ProjectionKnowledge> {
        self.projections
            .iter()
            .find(|(candidate, _)| candidate == expr)
            .map(|(_, knowledge)| knowledge)
    }
}

fn contains_set_operation(expr: &Expression) -> bool {
    match expr {
        Expression::Union(_) | Expression::Intersect(_) | Expression::Except(_) => true,
        Expression::Select(select) => select.with.as_ref().is_some_and(|with| {
            with.ctes
                .iter()
                .any(|cte| contains_set_operation(&cte.this))
        }),
        Expression::Subquery(node) => contains_set_operation(&node.this),
        Expression::Paren(node) => contains_set_operation(&node.this),
        Expression::Annotated(node) => contains_set_operation(&node.this),
        _ => false,
    }
}

fn set_has_explicit_name_anywhere(
    expr: &Expression,
    name: &str,
    dialect: Option<DialectType>,
) -> bool {
    match expr {
        Expression::Select(select) => select.with.as_ref().is_some_and(|with| {
            with.ctes
                .iter()
                .any(|cte| set_has_explicit_name_anywhere(&cte.this, name, dialect))
        }),
        Expression::Union(set) => {
            set_has_explicit_name(expr, name, dialect)
                || set_has_explicit_name_anywhere(&set.left, name, dialect)
                || set_has_explicit_name_anywhere(&set.right, name, dialect)
        }
        Expression::Intersect(set) => {
            set_has_explicit_name(expr, name, dialect)
                || set_has_explicit_name_anywhere(&set.left, name, dialect)
                || set_has_explicit_name_anywhere(&set.right, name, dialect)
        }
        Expression::Except(set) => {
            set_has_explicit_name(expr, name, dialect)
                || set_has_explicit_name_anywhere(&set.left, name, dialect)
                || set_has_explicit_name_anywhere(&set.right, name, dialect)
        }
        Expression::Subquery(node) => set_has_explicit_name_anywhere(&node.this, name, dialect),
        Expression::Paren(node) => set_has_explicit_name_anywhere(&node.this, name, dialect),
        Expression::Annotated(node) => set_has_explicit_name_anywhere(&node.this, name, dialect),
        _ => false,
    }
}

fn materialize_set_stars(expr: &mut Expression, name: &str, dialect: DialectType) -> bool {
    match expr {
        Expression::Select(select) => {
            let mut changed = false;
            let source_name = select
                .from
                .as_ref()
                .and_then(|from| from.expressions.first())
                .and_then(|source| match source {
                    Expression::Table(table) => Some(table.name.name.as_str()),
                    _ => None,
                });
            for expression in &mut select.expressions {
                if expression_is_star(expression)
                    && let Some(replacement) =
                        parse_projection(source_name.unwrap_or("star_source"), name, dialect)
                {
                    *expression = replacement;
                    changed = true;
                }
            }
            if let Some(with) = &mut select.with {
                for cte in &mut with.ctes {
                    changed |= materialize_set_stars(&mut cte.this, name, dialect);
                }
            }
            changed
        }
        Expression::Union(set) => {
            let left = materialize_set_stars(&mut set.left, name, dialect);
            let right = materialize_set_stars(&mut set.right, name, dialect);
            left || right
        }
        Expression::Intersect(set) => {
            let left = materialize_set_stars(&mut set.left, name, dialect);
            let right = materialize_set_stars(&mut set.right, name, dialect);
            left || right
        }
        Expression::Except(set) => {
            let left = materialize_set_stars(&mut set.left, name, dialect);
            let right = materialize_set_stars(&mut set.right, name, dialect);
            left || right
        }
        Expression::Subquery(node) => materialize_set_stars(&mut node.this, name, dialect),
        Expression::Paren(node) => materialize_set_stars(&mut node.this, name, dialect),
        Expression::Annotated(node) => materialize_set_stars(&mut node.this, name, dialect),
        _ => false,
    }
}

fn set_branch_depends_on_unknown_star(
    expr: &Expression,
    ordinal: usize,
    cte_names: &HashSet<String>,
) -> bool {
    match expr {
        Expression::Union(set) => {
            set_branch_depends_on_unknown_star(&set.left, ordinal, cte_names)
                || set_branch_depends_on_unknown_star(&set.right, ordinal, cte_names)
        }
        Expression::Intersect(set) => {
            set_branch_depends_on_unknown_star(&set.left, ordinal, cte_names)
                || set_branch_depends_on_unknown_star(&set.right, ordinal, cte_names)
        }
        Expression::Except(set) => {
            set_branch_depends_on_unknown_star(&set.left, ordinal, cte_names)
                || set_branch_depends_on_unknown_star(&set.right, ordinal, cte_names)
        }
        Expression::Annotated(node) => set_branch_depends_on_unknown_star(&node.this, ordinal, cte_names),
        Expression::Subquery(node) => set_branch_depends_on_unknown_star(&node.this, ordinal, cte_names),
        Expression::Cte(node) => set_branch_depends_on_unknown_star(&node.this, ordinal, cte_names),
        Expression::Paren(node) => set_branch_depends_on_unknown_star(&node.this, ordinal, cte_names),
        Expression::Select(select) => {
            select
                .expressions
                .iter()
                .position(expression_is_star)
                .is_some_and(|first_star| ordinal >= first_star)
                && select.from.as_ref().is_some_and(|from| {
                    from.expressions
                        .iter()
                        .any(|source| matches!(source, Expression::Table(table) if !cte_names.contains(&table.name.name.to_lowercase())))
                })
        }
        _ => false,
    }
}

fn set_has_explicit_name(expr: &Expression, name: &str, dialect: Option<DialectType>) -> bool {
    let normalized = polyglot_sql::normalize_name(name, dialect, false, true);
    match expr {
        Expression::Union(set) => {
            set_has_explicit_name(&set.left, name, dialect)
                || set_has_explicit_name(&set.right, name, dialect)
        }
        Expression::Intersect(set) => {
            set_has_explicit_name(&set.left, name, dialect)
                || set_has_explicit_name(&set.right, name, dialect)
        }
        Expression::Except(set) => {
            set_has_explicit_name(&set.left, name, dialect)
                || set_has_explicit_name(&set.right, name, dialect)
        }
        Expression::Annotated(node) => set_has_explicit_name(&node.this, name, dialect),
        Expression::Subquery(node) => set_has_explicit_name(&node.this, name, dialect),
        Expression::Cte(node) => set_has_explicit_name(&node.this, name, dialect),
        Expression::Paren(node) => set_has_explicit_name(&node.this, name, dialect),
        Expression::Select(select) => select.expressions.iter().any(|expression| {
            explicit_output_name(expression).is_some_and(|output| {
                polyglot_sql::normalize_name(output, dialect, false, true) == normalized
            })
        }),
        _ => false,
    }
}

fn projection_knowledge(expr: &Expression, dialect: Option<DialectType>) -> ProjectionKnowledge {
    let Some(select) = unwrap_select(expr) else {
        return ProjectionKnowledge::default();
    };
    let mut knowledge = ProjectionKnowledge::default();
    for (ordinal, expression) in select.expressions.iter().enumerate() {
        if expression_is_star(expression) {
            knowledge.unresolved_star_ordinal.get_or_insert(ordinal);
            if let Expression::Star(star) = expression {
                if let Some(replace) = &star.replace {
                    knowledge.explicit_names.extend(
                        replace
                            .iter()
                            .map(|alias| normalize_name(&alias.alias.name, dialect)),
                    );
                }
                if let Some(rename) = &star.rename {
                    knowledge.explicit_names.extend(
                        rename
                            .iter()
                            .map(|(_, target)| normalize_name(&target.name, dialect)),
                    );
                }
                if let Some(except) = &star.except {
                    knowledge.excluded_names.extend(
                        except
                            .iter()
                            .map(|name| normalize_name(&name.name, dialect)),
                    );
                }
            }
        } else if let Some(name) = explicit_output_name(expression) {
            knowledge
                .explicit_names
                .push(polyglot_sql::normalize_name(name, dialect, false, true));
        }
    }
    knowledge
}

fn normalize_name(name: &str, dialect: Option<DialectType>) -> String {
    polyglot_sql::normalize_name(name, dialect, false, true)
}

fn parse_projection(table: &str, name: &str, dialect: DialectType) -> Option<Expression> {
    let parsed = polyglot_sql::parse_one(&format!("SELECT {table}.{name}"), dialect).ok()?;
    let Expression::Select(parsed) = parsed else {
        return None;
    };
    parsed.expressions.into_iter().next()
}

fn visit_queries(expr: &Expression, visitor: &mut impl FnMut(&Expression)) {
    if unwrap_select(expr).is_some() || is_set_operation(expr) {
        visitor(expr);
    }
    match expr {
        Expression::Select(select) => {
            if let Some(with) = &select.with {
                for cte in &with.ctes {
                    visit_queries(&cte.this, visitor);
                }
            }
            if let Some(from) = &select.from {
                for source in &from.expressions {
                    visit_queries(source, visitor);
                }
            }
            for join in &select.joins {
                visit_queries(&join.this, visitor);
            }
        }
        Expression::Union(set) => {
            visit_queries(&set.left, visitor);
            visit_queries(&set.right, visitor);
        }
        Expression::Intersect(set) => {
            visit_queries(&set.left, visitor);
            visit_queries(&set.right, visitor);
        }
        Expression::Except(set) => {
            visit_queries(&set.left, visitor);
            visit_queries(&set.right, visitor);
        }
        Expression::Subquery(subquery) => visit_queries(&subquery.this, visitor),
        Expression::Cte(cte) => visit_queries(&cte.this, visitor),
        Expression::Paren(paren) => visit_queries(&paren.this, visitor),
        Expression::Annotated(annotated) => visit_queries(&annotated.this, visitor),
        _ => {}
    }
}

fn unwrap_select(expr: &Expression) -> Option<&polyglot_sql::expressions::Select> {
    match expr {
        Expression::Select(select) => Some(select),
        Expression::Annotated(node) => unwrap_select(&node.this),
        Expression::Subquery(node) => unwrap_select(&node.this),
        Expression::Cte(node) => unwrap_select(&node.this),
        Expression::Paren(node) => unwrap_select(&node.this),
        _ => None,
    }
}

fn is_set_operation(expr: &Expression) -> bool {
    matches!(
        expr,
        Expression::Union(_) | Expression::Intersect(_) | Expression::Except(_)
    )
}

fn explicit_ordinal(expr: &Expression, name: &str, dialect: Option<DialectType>) -> Option<usize> {
    let mut current = expr;
    loop {
        current = match current {
            Expression::Union(set) => &set.left,
            Expression::Intersect(set) => &set.left,
            Expression::Except(set) => &set.left,
            Expression::Annotated(node) => &node.this,
            Expression::Subquery(node) => &node.this,
            Expression::Cte(node) => &node.this,
            Expression::Paren(node) => &node.this,
            Expression::Select(select) => {
                return select.expressions.iter().position(|expr| {
                    explicit_output_name(expr).is_some_and(|output| {
                        polyglot_sql::normalize_name(output, dialect, false, true)
                            == polyglot_sql::normalize_name(name, dialect, false, true)
                    })
                });
            }
            _ => return None,
        };
    }
}

fn synthetic_ordinal(name: &str) -> Option<usize> {
    name.strip_prefix('_')?.parse().ok()
}

fn explicit_output_name(expr: &Expression) -> Option<&str> {
    match expr {
        Expression::Annotated(node) => explicit_output_name(&node.this),
        Expression::Alias(alias) => Some(alias.alias.name.as_str()),
        Expression::Column(column) => Some(column.name.name.as_str()),
        Expression::Identifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

fn expression_is_star(expr: &Expression) -> bool {
    match expr {
        Expression::Star(_) => true,
        Expression::Column(column) => column.name.name == "*",
        Expression::Annotated(node) => expression_is_star(&node.this),
        _ => false,
    }
}

pub(super) fn has_unresolved_stars(expr: &Expression) -> bool {
    let mut found = false;
    visit_queries(expr, &mut |query| {
        if unwrap_select(query)
            .is_some_and(|select| select.expressions.iter().any(expression_is_star))
        {
            found = true;
        }
    });
    found
}
