use polyglot_sql::lineage::LineageNode;
use polyglot_sql::scope::SourceKind;
use polyglot_sql::{DialectType, Expression};

use crate::parser::manifest::Manifest;

use super::schema::build_schema_from_manifest;
use super::star_guard::StarGuard;
use super::{ColumnSource, TransformationType};

pub(super) struct LineageContext {
    pub(super) expanded_expr: Expression,
    schema: Option<polyglot_sql::MappingSchema>,
    dialect: DialectType,
    star_guard: StarGuard,
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
    let star_guard = StarGuard::new(&expanded_expr, Some(dialect));
    Ok(LineageContext {
        expanded_expr,
        schema,
        dialect,
        star_guard,
    })
}

pub(super) fn run_column_lineage(
    col_name: &str,
    ctx: &LineageContext,
) -> Result<ColumnLineageResult, String> {
    let modifier_expr =
        ctx.star_guard
            .materialize_star_modifier(&ctx.expanded_expr, col_name, ctx.dialect);
    let lineage_expr = modifier_expr.as_ref().unwrap_or(&ctx.expanded_expr);
    let lineage_result = lineage_node(col_name, lineage_expr, ctx);

    match lineage_result {
        Ok(node) => {
            if ctx.star_guard.rejects(&node) {
                Err(format!("Cannot find column '{}' in query", col_name))
            } else {
                Ok(extract_leaf_sources(&node))
            }
        }
        Err(e) => explicit_set_operand_lineage(col_name, lineage_expr, ctx, &e)
            .ok_or_else(|| format_lineage_error(&e)),
    }
}

fn lineage_node(
    col_name: &str,
    expr: &Expression,
    ctx: &LineageContext,
) -> Result<LineageNode, polyglot_sql::Error> {
    let dialect = Some(ctx.dialect);
    if let Some(ref schema) = ctx.schema {
        polyglot_sql::lineage::lineage_with_schema(
            col_name,
            expr,
            Some(schema as &dyn polyglot_sql::Schema),
            dialect,
            false,
        )
        .or_else(|_| polyglot_sql::lineage::lineage(col_name, expr, dialect, false))
    } else {
        polyglot_sql::lineage::lineage(col_name, expr, dialect, false)
    }
}

// TODO: this locates an operand's projection by ordinal (see
// `StarGuard::explicit_set_operands`) but then re-derives lineage by looking that
// projection's output *name* back up via `lineage_node`, instead of tracing the
// already-located projection directly. Unaliased expressions have no name to look
// up (e.g. `fee * 2` in `SELECT other.*, fee FROM a UNION ALL SELECT x, fee * 2
// FROM b`) and are silently dropped; duplicate output names (`SELECT a.id, b.id`)
// make the lookup ambiguous and can retrace the wrong projection. Redesign: keep
// projection-list position and output ordinal as distinct types and trace
// positionally instead of round-tripping through a name.
fn explicit_set_operand_lineage(
    col_name: &str,
    expr: &Expression,
    ctx: &LineageContext,
    original_error: &polyglot_sql::Error,
) -> Option<ColumnLineageResult> {
    let error = original_error.to_string();
    if !error.contains("Cannot find column") || !error.contains("in set operation") {
        return None;
    }

    // A set's leftmost star can make its output names unknowable to polyglot-sql.
    // Trace only intact operands that explicitly project the requested ordinal;
    // unresolved star operands contribute no source instead of a guessed one.
    let operands = StarGuard::explicit_set_operands(expr, col_name, Some(ctx.dialect))?;
    let mut results = operands.into_iter().filter_map(|(operand, operand_name)| {
        let node = lineage_node(operand_name, operand, ctx).ok()?;
        (!ctx.star_guard.rejects(&node)).then(|| extract_leaf_sources(&node))
    });
    let first = results.next()?;
    Some(results.fold(first, merge_lineage_results))
}

fn merge_lineage_results(
    mut left: ColumnLineageResult,
    right: ColumnLineageResult,
) -> ColumnLineageResult {
    if left.transformation == TransformationType::Direct
        && right.transformation != TransformationType::Direct
    {
        left.transformation = right.transformation;
    }
    left.sources.extend(right.sources);
    left.sources
        .sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
    left.sources.dedup();
    left
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

fn classify_transformation(node: &LineageNode) -> TransformationType {
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

fn classify_expression(expr: &Expression) -> TransformationType {
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

fn extract_leaf_sources(node: &LineageNode) -> ColumnLineageResult {
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

fn collect_leaves(node: &LineageNode, sources: &mut Vec<ColumnSource>) {
    if node.downstream.is_empty() {
        if node.source_kind == SourceKind::Virtual {
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
                table: node.source_name.clone(),
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
