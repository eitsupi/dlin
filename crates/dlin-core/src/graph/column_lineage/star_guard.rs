use std::collections::HashSet;

use polyglot_sql::expressions::{Alias, Column, With};
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

// Searches every nested query for a star modifier (REPLACE/RENAME) whose target
// equals `name`. `name` is the single requested output name and is not translated
// across query boundaries, so a modifier reachable only through an outer alias
// (e.g. `WITH c AS (SELECT * RENAME(id AS wanted) FROM t) SELECT wanted AS final
// FROM c`, requesting `final`) will not be found. TODO: this needs a query-scoped
// projection model that follows aliases through CTE/subquery boundaries before
// searching for the modifier, rather than a flat name-string search.
fn materialize_star_modifier(expr: &mut Expression, name: &str, dialect: DialectType) -> bool {
    match expr {
        Expression::Select(select) => {
            let mut changed = false;
            for expression in &mut select.expressions {
                changed |= materialize_projection_modifier(expression, name, dialect);
            }
            if let Some(with) = &mut select.with {
                for cte in &mut with.ctes {
                    changed |= materialize_star_modifier(&mut cte.this, name, dialect);
                }
            }
            if let Some(from) = &mut select.from {
                for source in &mut from.expressions {
                    changed |= materialize_star_modifier(source, name, dialect);
                }
            }
            for join in &mut select.joins {
                changed |= materialize_star_modifier(&mut join.this, name, dialect);
            }
            changed
        }
        Expression::Union(set) => {
            let mut changed = false;
            if let Some(with) = &mut set.with {
                for cte in &mut with.ctes {
                    changed |= materialize_star_modifier(&mut cte.this, name, dialect);
                }
            }
            changed |= materialize_star_modifier(&mut set.left, name, dialect);
            changed |= materialize_star_modifier(&mut set.right, name, dialect);
            changed
        }
        Expression::Intersect(set) => {
            let mut changed = false;
            if let Some(with) = &mut set.with {
                for cte in &mut with.ctes {
                    changed |= materialize_star_modifier(&mut cte.this, name, dialect);
                }
            }
            changed |= materialize_star_modifier(&mut set.left, name, dialect);
            changed |= materialize_star_modifier(&mut set.right, name, dialect);
            changed
        }
        Expression::Except(set) => {
            let mut changed = false;
            if let Some(with) = &mut set.with {
                for cte in &mut with.ctes {
                    changed |= materialize_star_modifier(&mut cte.this, name, dialect);
                }
            }
            changed |= materialize_star_modifier(&mut set.left, name, dialect);
            changed |= materialize_star_modifier(&mut set.right, name, dialect);
            changed
        }
        Expression::Subquery(node) => materialize_star_modifier(&mut node.this, name, dialect),
        Expression::Paren(node) => materialize_star_modifier(&mut node.this, name, dialect),
        Expression::Annotated(node) => materialize_star_modifier(&mut node.this, name, dialect),
        _ => false,
    }
}

fn materialize_projection_modifier(
    expression: &mut Expression,
    name: &str,
    dialect: DialectType,
) -> bool {
    if let Expression::Annotated(node) = expression {
        return materialize_projection_modifier(&mut node.this, name, dialect);
    }
    let Expression::Star(star) = expression else {
        return false;
    };
    let normalized = normalize_name(name, Some(dialect));
    if star.except.as_ref().is_some_and(|except| {
        except
            .iter()
            .any(|identifier| normalize_name(&identifier.name, Some(dialect)) == normalized)
    }) {
        return false;
    }
    if let Some(alias) = star.replace.as_ref().and_then(|replace| {
        replace
            .iter()
            .find(|alias| normalize_name(&alias.alias.name, Some(dialect)) == normalized)
    }) {
        *expression = Expression::Alias(Box::new(alias.clone()));
        return true;
    }
    if let Some((source, target)) = star.rename.as_ref().and_then(|rename| {
        rename
            .iter()
            .find(|(_, target)| normalize_name(&target.name, Some(dialect)) == normalized)
    }) {
        let qualifier = star.table.clone();
        *expression = Expression::Alias(Box::new(Alias::new(
            Expression::Column(Box::new(Column {
                name: source.clone(),
                table: qualifier,
                join_mark: false,
                trailing_comments: Vec::new(),
                span: None,
                inferred_type: None,
            })),
            target.clone(),
        )));
        return true;
    }
    false
}

impl StarGuard {
    pub(super) fn new(expr: &Expression, dialect: Option<DialectType>) -> Self {
        let mut projections = Vec::new();
        let mut cte_names = HashSet::new();
        visit_queries(expr, &mut |query| {
            if let Some(with) = query_with(query) {
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
        let key = rejects_trace_key(&node.source, name, self.dialect);
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

    pub(super) fn materialize_star_modifier(
        &self,
        expr: &Expression,
        name: &str,
        dialect: DialectType,
    ) -> Option<Expression> {
        let mut candidate = expr.clone();
        materialize_star_modifier(&mut candidate, name, dialect).then_some(candidate)
    }

    pub(super) fn explicit_set_operands<'a>(
        expr: &'a Expression,
        name: &str,
        dialect: Option<DialectType>,
    ) -> Option<Vec<(&'a Expression, &'a str)>> {
        if !is_set_operation(unwrap_query(expr)) {
            return None;
        }

        // A WITH attached to a set operation scopes over all of its operands.
        // Standalone operand lineage would lose that scope, so leave those cases
        // to the normal lineage path rather than risk resolving a CTE as a table.
        let ordinal = explicit_set_ordinal(expr, name, dialect)?;
        let mut operands = Vec::new();
        collect_explicit_set_operands(expr, ordinal, &mut operands).then_some(operands)
    }

    fn knowledge(&self, expr: &Expression) -> Option<&ProjectionKnowledge> {
        self.projections
            .iter()
            .find(|(candidate, _)| candidate == expr)
            .map(|(_, knowledge)| knowledge)
    }
}

fn rejects_trace_key(expr: &Expression, name: &str, dialect: Option<DialectType>) -> TraceKey {
    if is_set_operation(expr) {
        TraceKey::SetOrdinal(
            explicit_ordinal(expr, name, dialect)
                .or_else(|| synthetic_ordinal(name))
                .unwrap_or(usize::MAX),
        )
    } else {
        TraceKey::Name
    }
}

fn collect_explicit_set_operands<'a>(
    expr: &'a Expression,
    ordinal: usize,
    operands: &mut Vec<(&'a Expression, &'a str)>,
) -> bool {
    match expr {
        Expression::Union(set) => {
            set.with.is_none()
                && collect_explicit_set_operands(&set.left, ordinal, operands)
                && collect_explicit_set_operands(&set.right, ordinal, operands)
        }
        Expression::Intersect(set) => {
            set.with.is_none()
                && collect_explicit_set_operands(&set.left, ordinal, operands)
                && collect_explicit_set_operands(&set.right, ordinal, operands)
        }
        Expression::Except(set) => {
            set.with.is_none()
                && collect_explicit_set_operands(&set.left, ordinal, operands)
                && collect_explicit_set_operands(&set.right, ordinal, operands)
        }
        Expression::Annotated(node) => collect_explicit_set_operands(&node.this, ordinal, operands),
        Expression::Subquery(node) => collect_explicit_set_operands(&node.this, ordinal, operands),
        Expression::Cte(node) => collect_explicit_set_operands(&node.this, ordinal, operands),
        Expression::Paren(node) => collect_explicit_set_operands(&node.this, ordinal, operands),
        Expression::Select(select) => {
            if let Some(expression) = select.expressions.get(ordinal)
                && let Some(output) = explicit_output_name(expression)
            {
                operands.push((expr, output));
            }
            true
        }
        _ => false,
    }
}

fn unwrap_query(expr: &Expression) -> &Expression {
    match expr {
        Expression::Annotated(node) => unwrap_query(&node.this),
        Expression::Subquery(node) => unwrap_query(&node.this),
        Expression::Cte(node) => unwrap_query(&node.this),
        Expression::Paren(node) => unwrap_query(&node.this),
        _ => expr,
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
            if let Some(with) = &set.with {
                for cte in &with.ctes {
                    visit_queries(&cte.this, visitor);
                }
            }
            visit_queries(&set.left, visitor);
            visit_queries(&set.right, visitor);
        }
        Expression::Intersect(set) => {
            if let Some(with) = &set.with {
                for cte in &with.ctes {
                    visit_queries(&cte.this, visitor);
                }
            }
            visit_queries(&set.left, visitor);
            visit_queries(&set.right, visitor);
        }
        Expression::Except(set) => {
            if let Some(with) = &set.with {
                for cte in &with.ctes {
                    visit_queries(&cte.this, visitor);
                }
            }
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

// Returns the `WITH` clause attached directly to `expr`, whether it belongs to a
// `SELECT` or to a set operation (`UNION`/`INTERSECT`/`EXCEPT` each carry their own
// `WITH` field rather than inheriting one from either operand).
fn query_with(expr: &Expression) -> Option<&With> {
    match expr {
        Expression::Select(select) => select.with.as_ref(),
        Expression::Union(set) => set.with.as_ref(),
        Expression::Intersect(set) => set.with.as_ref(),
        Expression::Except(set) => set.with.as_ref(),
        _ => None,
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

// Set output names follow the leftmost operand. This is the ordinal policy used
// by rejects(), and is intentionally separate from explicit_set_ordinal().
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

// The fallback must only trace operands when every explicit declaration of the
// requested name agrees on one ordinal and the set has no attached WITH.
fn explicit_set_ordinal(
    expr: &Expression,
    name: &str,
    dialect: Option<DialectType>,
) -> Option<usize> {
    let normalized = polyglot_sql::normalize_name(name, dialect, false, true);
    let mut ordinal = None;
    collect_explicit_ordinal(expr, &normalized, dialect, &mut ordinal).then_some(ordinal)?
}

fn collect_explicit_ordinal(
    expr: &Expression,
    normalized_name: &str,
    dialect: Option<DialectType>,
    ordinal: &mut Option<usize>,
) -> bool {
    match expr {
        Expression::Union(set) => {
            set.with.is_none()
                && collect_explicit_ordinal(&set.left, normalized_name, dialect, ordinal)
                && collect_explicit_ordinal(&set.right, normalized_name, dialect, ordinal)
        }
        Expression::Intersect(set) => {
            set.with.is_none()
                && collect_explicit_ordinal(&set.left, normalized_name, dialect, ordinal)
                && collect_explicit_ordinal(&set.right, normalized_name, dialect, ordinal)
        }
        Expression::Except(set) => {
            set.with.is_none()
                && collect_explicit_ordinal(&set.left, normalized_name, dialect, ordinal)
                && collect_explicit_ordinal(&set.right, normalized_name, dialect, ordinal)
        }
        Expression::Annotated(node) => {
            collect_explicit_ordinal(&node.this, normalized_name, dialect, ordinal)
        }
        Expression::Subquery(node) => {
            collect_explicit_ordinal(&node.this, normalized_name, dialect, ordinal)
        }
        Expression::Cte(node) => {
            collect_explicit_ordinal(&node.this, normalized_name, dialect, ordinal)
        }
        Expression::Paren(node) => {
            collect_explicit_ordinal(&node.this, normalized_name, dialect, ordinal)
        }
        Expression::Select(select) => {
            for (candidate_ordinal, expression) in select.expressions.iter().enumerate() {
                if explicit_output_name(expression).is_some_and(|output| {
                    polyglot_sql::normalize_name(output, dialect, false, true) == normalized_name
                }) {
                    if ordinal.is_some_and(|existing| existing != candidate_ordinal) {
                        return false;
                    }
                    *ordinal = Some(candidate_ordinal);
                }
            }
            true
        }
        _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_keeps_leftmost_ordinal_when_other_operands_conflict() {
        let expr = polyglot_sql::parse_one(
            "SELECT 1 AS target, 2 AS keep FROM left_table \
             UNION ALL SELECT 3 AS keep, 4 AS target FROM right_table",
            DialectType::Generic,
        )
        .unwrap();

        assert_eq!(
            rejects_trace_key(&expr, "target", Some(DialectType::Generic)),
            TraceKey::SetOrdinal(0)
        );
        assert_eq!(
            explicit_set_ordinal(&expr, "target", Some(DialectType::Generic)),
            None
        );
    }
}
