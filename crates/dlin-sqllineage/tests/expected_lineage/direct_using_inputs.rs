//! AST based inputs for the direct-reference versus star-derived invariant.

use std::collections::BTreeSet;
use std::ops::ControlFlow;

use dlin_sqllineage::{Ident, Quote, parse};
use sqlparser::ast::{
    Expr, JoinConstraint, JoinOperator, ObjectNamePart, Query, Select, SelectItem, SetExpr,
    Statement, TableFactor, TableWithJoins, Visit, Visitor, With,
};

use super::support::Case;

/// A top-level SELECT branch and its retained source scope.
pub struct Input {
    pub label: String,
    pub with_sql: String,
    pub from_sql: String,
    pub names: Vec<Ident>,
    /// The only named input boundary, if all output columns come through one
    /// CTE/derived range. Duplicate output names are ambiguous at that boundary.
    pub boundary: Option<dlin_sqllineage::Candidate>,
}

/// Extract top-level source SELECT branches from a fixture.
///
/// The query is parsed through the public API, serialized with `ast_json`, and
/// deserialized into sqlparser statements so this helper follows the same
/// public parse representation used by callers. Queries nested inside FROM,
/// expressions, or CTE definitions are deliberately retained as source text;
/// they are not lifted into independent inputs.
pub fn inputs(case: &Case) -> (Vec<Input>, Vec<String>) {
    let parsed = match parse(case.sql(), case.dialect()) {
        Ok(parsed) => parsed,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("{}: parser error: {error}", case.name)],
            );
        }
    };
    let json = match parsed.ast_json(false) {
        Ok(json) => json,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!(
                    "{}: AST JSON serialization error: {error}",
                    case.name
                )],
            );
        }
    };
    let statements: Vec<Statement> = match serde_json::from_str(&json) {
        Ok(statements) => statements,
        Err(error) => {
            return (
                Vec::new(),
                vec![format!("{}: AST deserialization error: {error}", case.name)],
            );
        }
    };
    if statements.len() != 1 {
        return (
            Vec::new(),
            vec![format!(
                "{}: expected one statement, found {}",
                case.name,
                statements.len()
            )],
        );
    }
    let query = match &statements[0] {
        Statement::Query(query) => Some(query.as_ref()),
        Statement::Insert(insert) => insert.source.as_deref(),
        Statement::CreateTable(create) => create.query.as_deref(),
        _ => None,
    };
    let Some(query) = query else {
        return (
            Vec::new(),
            vec![format!(
                "{}: statement has no supported Query source",
                case.name
            )],
        );
    };

    let mut inputs = Vec::new();
    let mut skips = Vec::new();
    let mut branch = 0usize;
    visit_query(query, &[], case, &mut branch, &mut inputs, &mut skips);
    if branch == 0 {
        skips.push(format!(
            "{}: source Query has no top-level SELECT branch (VALUES/TABLE/etc.)",
            case.name
        ));
    }
    (inputs, skips)
}

fn visit_query(
    query: &Query,
    inherited_with: &[With],
    case: &Case,
    branch: &mut usize,
    inputs: &mut Vec<Input>,
    skips: &mut Vec<String>,
) {
    let mut withs = inherited_with.to_vec();
    if let Some(with) = &query.with {
        withs.push(with.clone());
    }
    visit_set_expr(&query.body, &withs, case, branch, inputs, skips);
}

fn visit_set_expr(
    set: &SetExpr,
    withs: &[With],
    case: &Case,
    branch: &mut usize,
    inputs: &mut Vec<Input>,
    skips: &mut Vec<String>,
) {
    match set {
        SetExpr::Select(select) => {
            let index = *branch;
            *branch += 1;
            add_select(
                &case.name,
                index,
                select,
                withs,
                case.dialect(),
                inputs,
                skips,
            );
        }
        SetExpr::SetOperation { left, right, .. } => {
            visit_set_expr(left, withs, case, branch, inputs, skips);
            visit_set_expr(right, withs, case, branch, inputs, skips);
        }
        SetExpr::Query(query) => visit_query(query, withs, case, branch, inputs, skips),
        SetExpr::Values(_) => skips.push(format!(
            "{}: top-level VALUES set-expression is not a SELECT branch",
            case.name
        )),
        SetExpr::Table(_) => skips.push(format!(
            "{}: top-level TABLE set-expression is not a SELECT branch",
            case.name
        )),
        _ => skips.push(format!(
            "{}: unsupported top-level set-expression",
            case.name
        )),
    }
}

fn add_select(
    case_name: &str,
    branch: usize,
    select: &Select,
    withs: &[With],
    dialect: dlin_sqllineage::Dialect,
    inputs: &mut Vec<Input>,
    skips: &mut Vec<String>,
) {
    let label = format!("{case_name}/branch_{branch}");
    if select.from.is_empty() {
        skips.push(format!("{label}: SELECT has no FROM"));
        return;
    }
    let with_sql = match format_with_context(withs, dialect) {
        Ok(sql) => sql,
        Err(reason) => {
            skips.push(format!("{label}: {reason}"));
            return;
        }
    };
    let from_sql = select
        .from
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let mut names = Vec::new();
    let mut keys = BTreeSet::new();
    let mut prefixes = Vec::new();
    for from in &select.from {
        collect_from_prefixes(from, &mut prefixes);
    }
    for item in &select.projection {
        let expr = match item {
            SelectItem::UnnamedExpr(expr)
            | SelectItem::ExprWithAlias { expr, .. }
            | SelectItem::ExprWithAliases { expr, .. } => Some(expr),
            _ => None,
        };
        if let Some(expr) = expr {
            let mut visitor = ProjectionNames {
                query_depth: 0,
                refs: Vec::new(),
            };
            let _ = expr.visit(&mut visitor);
            for reference in visitor.refs {
                match reference {
                    Expr::Identifier(ident) => {
                        push_name(from_ast_ident(&ident), dialect, &mut keys, &mut names);
                    }
                    Expr::CompoundIdentifier(parts) => {
                        let prefix = parts[..parts.len().saturating_sub(1)]
                            .iter()
                            .map(from_ast_ident)
                            .collect::<Vec<_>>();
                        if prefix_matches(&prefix, &prefixes, dialect) {
                            if let Some(ident) = parts.last() {
                                push_name(from_ast_ident(ident), dialect, &mut keys, &mut names);
                            }
                        } else {
                            skips.push(format!(
                                "{label}: projection path `{}` does not match a FROM relation/alias prefix",
                                Expr::CompoundIdentifier(parts)
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    for from in &select.from {
        collect_using_names(from, dialect, &mut keys, &mut names, skips, &label);
    }
    let boundary = if select.from.len() == 1 && select.from[0].joins.is_empty() {
        match &select.from[0].relation {
            TableFactor::Derived { alias, .. } => Some(dlin_sqllineage::Candidate::Derived(
                alias.as_ref().map(|alias| from_ast_ident(&alias.name)),
            )),
            TableFactor::Table { name, alias, .. } if name.0.len() == 1 => name.0[0]
                .as_ident()
                .filter(|name| {
                    withs.iter().flat_map(|with| &with.cte_tables).any(|cte| {
                        from_ast_ident(&cte.alias.name).column_key(dialect)
                            == from_ast_ident(name).column_key(dialect)
                    })
                })
                .map(|name| {
                    dlin_sqllineage::Candidate::Cte(from_ast_ident(
                        alias.as_ref().map_or(name, |alias| &alias.name),
                    ))
                }),
            _ => None,
        }
    } else {
        None
    };
    inputs.push(Input {
        label: format!("branch_{branch}"),
        with_sql,
        from_sql,
        names,
        boundary,
    });
}

struct ProjectionNames {
    query_depth: usize,
    refs: Vec<Expr>,
}

impl Visitor for ProjectionNames {
    type Break = ();

    fn pre_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.query_depth += 1;
        ControlFlow::Continue(())
    }

    fn post_visit_query(&mut self, _query: &Query) -> ControlFlow<Self::Break> {
        self.query_depth -= 1;
        ControlFlow::Continue(())
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        if self.query_depth == 0
            && matches!(expr, Expr::Identifier(_) | Expr::CompoundIdentifier(_))
        {
            self.refs.push(expr.clone());
        }
        ControlFlow::Continue(())
    }
}

fn collect_from_prefixes(from: &TableWithJoins, prefixes: &mut Vec<Vec<Ident>>) {
    collect_factor_prefixes(&from.relation, prefixes);
    for join in &from.joins {
        collect_factor_prefixes(&join.relation, prefixes);
    }
}

fn collect_using_names(
    from: &TableWithJoins,
    dialect: dlin_sqllineage::Dialect,
    keys: &mut BTreeSet<String>,
    names: &mut Vec<Ident>,
    skips: &mut Vec<String>,
    label: &str,
) {
    collect_factor_using_names(&from.relation, dialect, keys, names, skips, label);
    for join in &from.joins {
        let constraint = match &join.join_operator {
            JoinOperator::Join(constraint)
            | JoinOperator::Inner(constraint)
            | JoinOperator::Left(constraint)
            | JoinOperator::LeftOuter(constraint)
            | JoinOperator::Right(constraint)
            | JoinOperator::RightOuter(constraint)
            | JoinOperator::FullOuter(constraint)
            | JoinOperator::CrossJoin(constraint)
            | JoinOperator::Semi(constraint)
            | JoinOperator::LeftSemi(constraint)
            | JoinOperator::RightSemi(constraint)
            | JoinOperator::Anti(constraint)
            | JoinOperator::LeftAnti(constraint)
            | JoinOperator::RightAnti(constraint)
            | JoinOperator::StraightJoin(constraint) => Some(constraint),
            JoinOperator::AsOf { constraint, .. } => Some(constraint),
            _ => None,
        };
        if let Some(JoinConstraint::Using(columns)) = constraint {
            for column in columns {
                if let Some(ident) = column.0.last().and_then(ObjectNamePart::as_ident) {
                    push_name(from_ast_ident(ident), dialect, keys, names);
                } else {
                    skips.push(format!(
                        "{label}: USING column `{column}` has no terminal identifier"
                    ));
                }
            }
        }
        collect_factor_using_names(&join.relation, dialect, keys, names, skips, label);
    }
}

fn collect_factor_using_names(
    factor: &TableFactor,
    dialect: dlin_sqllineage::Dialect,
    keys: &mut BTreeSet<String>,
    names: &mut Vec<Ident>,
    skips: &mut Vec<String>,
    label: &str,
) {
    if let TableFactor::NestedJoin {
        table_with_joins, ..
    } = factor
    {
        collect_using_names(table_with_joins, dialect, keys, names, skips, label);
    }
}

fn collect_factor_prefixes(factor: &TableFactor, prefixes: &mut Vec<Vec<Ident>>) {
    match factor {
        TableFactor::Table { name, alias, .. } => {
            if let Some(alias) = alias {
                prefixes.push(vec![from_ast_ident(&alias.name)]);
            } else if name.0.len() > 1 {
                prefixes.push(
                    name.0
                        .iter()
                        .filter_map(ObjectNamePart::as_ident)
                        .map(from_ast_ident)
                        .collect(),
                );
                if let Some(last) = name.0.last().and_then(ObjectNamePart::as_ident) {
                    prefixes.push(vec![from_ast_ident(last)]);
                }
            } else if let Some(last) = name.0.last().and_then(ObjectNamePart::as_ident) {
                prefixes.push(vec![from_ast_ident(last)]);
            }
        }
        TableFactor::Derived { alias, .. }
        | TableFactor::TableFunction { alias, .. }
        | TableFactor::Function { alias, .. }
        | TableFactor::UNNEST { alias, .. } => {
            if let Some(alias) = alias {
                prefixes.push(vec![from_ast_ident(&alias.name)]);
            }
        }
        TableFactor::NestedJoin {
            table_with_joins,
            alias,
        } => {
            if let Some(alias) = alias {
                prefixes.push(vec![from_ast_ident(&alias.name)]);
            } else {
                collect_from_prefixes(table_with_joins, prefixes);
            }
        }
        _ => {}
    }
}

fn prefix_matches(
    prefix: &[Ident],
    candidates: &[Vec<Ident>],
    dialect: dlin_sqllineage::Dialect,
) -> bool {
    !prefix.is_empty()
        && candidates.iter().any(|candidate| {
            candidate.len() == prefix.len()
                && candidate
                    .iter()
                    .zip(prefix)
                    .all(|(left, right)| left.column_key(dialect) == right.column_key(dialect))
        })
}

fn push_name(
    name: Ident,
    dialect: dlin_sqllineage::Dialect,
    keys: &mut BTreeSet<String>,
    names: &mut Vec<Ident>,
) {
    let key = name.column_key(dialect).into_owned();
    if keys.insert(key) {
        names.push(name);
    }
}

fn from_ast_ident(ident: &sqlparser::ast::Ident) -> Ident {
    let quote = ident.quote_style.map(|style| match style {
        '"' => Quote::Double,
        '`' => Quote::Backtick,
        '[' | ']' => Quote::Bracket,
        '\'' => Quote::Single,
        _ => Quote::Double,
    });
    Ident {
        value: ident.value.clone(),
        quote,
    }
}

#[test]
fn nested_join_using_names_are_input_names() {
    let case = Case::new(
        "nested_using_input_name",
        "SELECT * FROM a CROSS JOIN (b JOIN c USING (inner_key))",
        dlin_sqllineage::Dialect::Generic,
        vec![],
    );
    let (inputs, skips) = inputs(&case);
    assert!(skips.is_empty(), "{skips:?}");
    assert_eq!(inputs.len(), 1);
    assert_eq!(inputs[0].label, "branch_0");
    assert_eq!(inputs[0].names, [Ident::unquoted("inner_key")]);
}

fn format_with_context(
    withs: &[With],
    dialect: dlin_sqllineage::Dialect,
) -> Result<String, String> {
    if withs.len() > 1 {
        return Err(
            "nested WITH boundaries cannot be flattened without changing name visibility".into(),
        );
    }
    let mut ctes = Vec::new();
    let mut names = BTreeSet::new();
    let mut recursive = false;
    for with in withs {
        recursive |= with.recursive;
        for cte in &with.cte_tables {
            // Flattening nested WITH scopes would change shadowing semantics.
            // Explicitly skip that uncommon case rather than altering SQL.
            if !names.insert(
                from_ast_ident(&cte.alias.name)
                    .column_key(dialect)
                    .into_owned(),
            ) {
                return Err("nested WITH shadows a CTE name; flattening would change scope".into());
            }
            ctes.push(cte.to_string());
        }
    }
    if ctes.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!(
            "WITH {}{} ",
            if recursive { "RECURSIVE " } else { "" },
            ctes.join(", ")
        ))
    }
}

#[test]
fn probe_names_are_input_references_not_aliases_or_inner_query_columns() {
    let case = Case::new(
        "input_names",
        "WITH source AS (SELECT id, amount FROM a) SELECT x.id AS renamed, SUM(x.amount) AS total, (SELECT b.secret FROM b) AS scalar FROM source x",
        dlin_sqllineage::Dialect::Generic,
        vec![],
    );
    let (inputs, skips) = inputs(&case);
    assert!(skips.is_empty(), "{skips:?}");
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        inputs[0].names,
        [Ident::unquoted("id"), Ident::unquoted("amount")]
    );
    assert_eq!(inputs[0].from_sql, "source x");
    assert!(inputs[0].with_sql.contains("WITH source AS"));
    assert_eq!(
        inputs[0].boundary,
        Some(dlin_sqllineage::Candidate::Cte(Ident::unquoted("x")))
    );
}
