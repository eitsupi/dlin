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
    let expr = polyglot_sql::parse_one(compiled_code, dialect).map_err(|e| format!("{}", e))?;

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
        Ok(node) => Ok(extract_leaf_sources(&node)),
        Err(e) => Err(format_lineage_error(&e)),
    }
}

pub(super) fn has_unresolved_stars(expr: &Expression) -> bool {
    match expr {
        Expression::Select(select) => {
            let outer_has_star = select.expressions.iter().any(|e| {
                matches!(e, Expression::Star(_))
                    || matches!(e, Expression::Column(c) if c.name.name == "*")
            });
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
    classify_expression(&node.expression)
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
        let name = &node.name;
        if let Some((table, column)) = name.rsplit_once('.') {
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
