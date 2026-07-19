use polyglot_sql::{DialectType, Expression};

use crate::parser::manifest::Manifest;

use super::schema::build_schema_from_manifest;
use super::{ColumnSource, TransformationType};

pub(super) struct LineageContext {
    pub(super) expanded_expr: Expression,
    schema: Option<polyglot_sql::MappingSchema>,
    dialect: DialectType,
}

pub(super) fn prepare_lineage_context(
    compiled_code: &str,
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: DialectType,
) -> Result<LineageContext, String> {
    prepare_lineage_context_with_expr(compiled_code, manifest, node, dialect, None)
}

pub(super) fn prepare_lineage_context_with_expr(
    compiled_code: &str,
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: DialectType,
    parsed_expr: Option<&Expression>,
) -> Result<LineageContext, String> {
    let expr = match parsed_expr {
        Some(e) => e.clone(),
        None => polyglot_sql::parse_one(compiled_code, dialect).map_err(|e| format!("{}", e))?,
    };

    let schema = build_schema_from_manifest(manifest, node, dialect);

    let mut expanded_expr = expr;
    polyglot_sql::lineage::expand_cte_stars(
        &mut expanded_expr,
        schema.as_ref().map(|s| s as &dyn polyglot_sql::Schema),
    );

    Ok(LineageContext {
        expanded_expr,
        schema,
        dialect,
    })
}

pub(super) fn run_column_lineage(
    col_name: &str,
    ctx: &LineageContext,
) -> Result<ColumnLineageResult, String> {
    let dialect = Some(ctx.dialect);
    let lineage_result = if let Some(ref s) = ctx.schema {
        polyglot_sql::lineage::lineage_with_schema(
            col_name,
            &ctx.expanded_expr,
            Some(s as &dyn polyglot_sql::Schema),
            dialect,
            false,
        )
        .or_else(|_| polyglot_sql::lineage::lineage(col_name, &ctx.expanded_expr, dialect, false))
    } else {
        polyglot_sql::lineage::lineage(col_name, &ctx.expanded_expr, dialect, false)
    };

    match lineage_result {
        Ok(node) => {
            if lineage_depends_on_unresolved_star(&node, true, false, Some(ctx.dialect)) {
                Err(format!("Cannot find column '{}' in query", col_name))
            } else {
                Ok(extract_leaf_sources(&node))
            }
        }
        Err(e) => Err(format_lineage_error(&e)),
    }
}

/// Return whether lineage for this column relied on polyglot-sql's schema-less
/// star passthrough. A star elsewhere in the query is not sufficient: explicit
/// output expressions must remain traceable independently.
fn lineage_depends_on_unresolved_star(
    node: &polyglot_sql::lineage::LineageNode,
    is_root: bool,
    is_set_operation_branch: bool,
    dialect: Option<DialectType>,
) -> bool {
    let column_name = node
        .reference_node_name
        .rsplit_once('.')
        .map_or(node.name.as_str(), |(_, name)| name);

    let source_has_star = has_unresolved_stars(&node.source);
    let source_has_explicit_column = select_has_explicit_output(&node.source, column_name, dialect);

    if is_root {
        if matches!(&node.expression, polyglot_sql::Expression::Column(c) if c.table.is_some())
            && source_has_star
            && !source_has_explicit_column
        {
            return true;
        }
    } else if (is_set_operation_branch
        || matches!(
            &node.source,
            polyglot_sql::Expression::Select(_)
                | polyglot_sql::Expression::Union(_)
                | polyglot_sql::Expression::Intersect(_)
                | polyglot_sql::Expression::Except(_)
                | polyglot_sql::Expression::Subquery(_)
                | polyglot_sql::Expression::Cte(_)
                | polyglot_sql::Expression::Paren(_)
        ))
        && source_has_star
        && !source_has_explicit_column
    {
        return true;
    }

    let is_set_operation = matches!(
        &node.source,
        polyglot_sql::Expression::Union(_)
            | polyglot_sql::Expression::Intersect(_)
            | polyglot_sql::Expression::Except(_)
    );
    node.downstream.iter().any(|child| {
        lineage_depends_on_unresolved_star(
            child,
            false,
            is_set_operation_branch || is_set_operation,
            dialect,
        )
    })
}

fn select_has_explicit_output(
    expr: &polyglot_sql::Expression,
    column_name: &str,
    dialect: Option<DialectType>,
) -> bool {
    let normalized_column_name = polyglot_sql::normalize_name(column_name, dialect, false, true);

    match expr {
        polyglot_sql::Expression::Annotated(annotated) => {
            select_has_explicit_output(&annotated.this, column_name, dialect)
        }
        polyglot_sql::Expression::Select(select) => select.expressions.iter().any(|expr| {
            let output_name = explicit_output_name(expr);
            output_name.is_some_and(|name| {
                name != "*"
                    && polyglot_sql::normalize_name(name, dialect, false, true)
                        == normalized_column_name
            })
        }),
        polyglot_sql::Expression::Union(union) => {
            select_has_explicit_output(&union.left, column_name, dialect)
        }
        polyglot_sql::Expression::Intersect(intersect) => {
            select_has_explicit_output(&intersect.left, column_name, dialect)
        }
        polyglot_sql::Expression::Except(except) => {
            select_has_explicit_output(&except.left, column_name, dialect)
        }
        polyglot_sql::Expression::Subquery(subquery) => {
            select_has_explicit_output(&subquery.this, column_name, dialect)
        }
        polyglot_sql::Expression::Cte(cte) => {
            select_has_explicit_output(&cte.this, column_name, dialect)
        }
        polyglot_sql::Expression::Paren(paren) => {
            select_has_explicit_output(&paren.this, column_name, dialect)
        }
        _ => false,
    }
}

fn explicit_output_name(expr: &polyglot_sql::Expression) -> Option<&str> {
    match expr {
        polyglot_sql::Expression::Annotated(annotated) => explicit_output_name(&annotated.this),
        polyglot_sql::Expression::Alias(alias) => Some(alias.alias.name.as_str()),
        polyglot_sql::Expression::Column(column) => Some(column.name.name.as_str()),
        polyglot_sql::Expression::Identifier(identifier) => Some(identifier.name.as_str()),
        _ => None,
    }
}

pub(super) fn has_unresolved_stars(expr: &Expression) -> bool {
    match expr {
        Expression::Select(select) => {
            let outer_has_star = select.expressions.iter().any(expression_is_star);
            if outer_has_star {
                return true;
            }
            if let Some(with) = &select.with
                && with.ctes.iter().any(|cte| has_unresolved_stars(&cte.this))
            {
                return true;
            }
            if let Some(from) = &select.from
                && from.expressions.iter().any(has_unresolved_stars)
            {
                return true;
            }
            if select.joins.iter().any(|j| has_unresolved_stars(&j.this)) {
                return true;
            }
            false
        }
        Expression::Subquery(subq) => has_unresolved_stars(&subq.this),
        Expression::Cte(cte) => has_unresolved_stars(&cte.this),
        Expression::Annotated(annotated) => has_unresolved_stars(&annotated.this),
        Expression::Union(union) => {
            has_unresolved_stars(&union.left) || has_unresolved_stars(&union.right)
        }
        Expression::Intersect(intersect) => {
            has_unresolved_stars(&intersect.left) || has_unresolved_stars(&intersect.right)
        }
        Expression::Except(except) => {
            has_unresolved_stars(&except.left) || has_unresolved_stars(&except.right)
        }
        _ => false,
    }
}

fn expression_is_star(expr: &Expression) -> bool {
    match expr {
        Expression::Star(_) => true,
        Expression::Column(column) => column.name.name == "*",
        Expression::Annotated(annotated) => expression_is_star(&annotated.this),
        _ => false,
    }
}

pub(super) fn format_lineage_error(e: &polyglot_sql::Error) -> String {
    let msg = e.to_string();
    if let Some(rest) = msg
        .strip_prefix("Parse error at line 0, column 0: ")
        .or_else(|| msg.strip_prefix("Syntax error at line 0, column 0: "))
    {
        format!("lineage failed: {}", rest)
    } else if msg.starts_with("Internal error: ") {
        format!(
            "lineage failed: {}",
            msg.strip_prefix("Internal error: ").unwrap()
        )
    } else {
        msg
    }
}

fn classify_transformation(node: &polyglot_sql::lineage::LineageNode) -> TransformationType {
    let t = classify_expression(&node.expression);
    if t != TransformationType::Direct || node.downstream.is_empty() {
        return t;
    }
    // Direct pass-through: traverse downstream to find the actual transformation type.
    // This handles the case where a CTE computes an expression and a later SELECT
    // references the result by name (which looks like Column → Direct at the surface).
    for child in &node.downstream {
        let child_t = classify_transformation(child);
        if child_t != TransformationType::Direct {
            return child_t;
        }
    }
    TransformationType::Direct
}

fn classify_expression(expr: &polyglot_sql::Expression) -> TransformationType {
    use polyglot_sql::Expression;
    match expr {
        Expression::Column(_) | Expression::Identifier(_) => TransformationType::Direct,
        Expression::Alias(alias) => classify_expression(&alias.this),
        Expression::Count(_)
        | Expression::Sum(_)
        | Expression::Avg(_)
        | Expression::Min(_)
        | Expression::Max(_) => TransformationType::Aggregation,
        Expression::Cast(_) => TransformationType::Cast,
        Expression::Case(_) => TransformationType::Conditional,
        Expression::Add(_) | Expression::Sub(_) | Expression::Mul(_) | Expression::Div(_) => {
            TransformationType::Expression
        }
        Expression::Anonymous(_) | Expression::Coalesce(_) | Expression::NullIf(_) => {
            TransformationType::Expression
        }
        // Generic function calls (covers CONCAT, NULLIF, SPLIT, etc. parsed as Function)
        Expression::Function(_) => TransformationType::Expression,
        // Specialized scalar function variants with their own AST types
        Expression::Upper(_)
        | Expression::Lower(_)
        | Expression::Length(_)
        | Expression::Concat(_) => TransformationType::Expression,
        _ => TransformationType::Unknown,
    }
}

pub(super) struct ColumnLineageResult {
    pub(super) sources: Vec<ColumnSource>,
    pub(super) transformation: TransformationType,
}

fn extract_leaf_sources(node: &polyglot_sql::lineage::LineageNode) -> ColumnLineageResult {
    let transformation = classify_transformation(node);
    let mut sources = Vec::new();
    collect_leaves(node, &mut sources);
    sources.sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
    sources.dedup();
    ColumnLineageResult {
        sources,
        transformation,
    }
}

fn collect_leaves(node: &polyglot_sql::lineage::LineageNode, sources: &mut Vec<ColumnSource>) {
    if node.downstream.is_empty() {
        if node.source_kind == polyglot_sql::scope::SourceKind::Virtual {
            return;
        }
        let name = &node.name;
        if let Some((alias, column)) = name.rsplit_once('.') {
            // polyglot-sql puts the SQL alias in node.name (e.g. "c.customer_id") and the
            // actual table name in source_name (e.g. "stg_customers"). Use source_name
            // when it genuinely differs from the alias, including fully-qualified forms
            // (e.g. "shop.main.stg_orders" when the SQL uses the short name "stg_orders").
            let table = if !node.source_name.is_empty() && node.source_name != alias {
                node.source_name.as_str()
            } else {
                alias
            };
            sources.push(ColumnSource {
                table: table.to_string(),
                column: column.to_string(),
                model_path: vec![],
            });
        } else {
            sources.push(ColumnSource {
                table: String::new(),
                column: name.to_string(),
                model_path: vec![],
            });
        }
    } else {
        for child in &node.downstream {
            collect_leaves(child, sources);
        }
    }
}
