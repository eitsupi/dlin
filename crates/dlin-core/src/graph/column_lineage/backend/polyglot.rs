//! Polyglot-sql backend implementation details for column lineage.
//!
//! This module keeps the raw polyglot-sql interaction behind a narrow
//! internal boundary.

use std::collections::BTreeSet;

use polyglot_sql::{self, Expression, Schema, expressions::DataType};
use rayon::prelude::*;

use crate::graph::column_lineage::ColumnSource;
use crate::graph::column_lineage::backend::catalog::CatalogSnapshot;
use crate::graph::column_lineage::backend::types::{
    AnalysisCompleteness, BackendAnalysis, BackendColumnFailure, BackendColumnOutcome,
    BackendColumnResult, BackendError, BackendErrorKind, BackendSource, BackendStatementResult,
    OutputColumnRequest, OutputDiscovery, OutputDiscoveryRequest, OutputName, OutputOrdinal,
    OutputTarget, ResolutionState,
};
use crate::graph::column_lineage::{TransformationType, schema};
use crate::parser::manifest::Manifest;

pub(crate) fn discover_output_columns(
    request: &OutputDiscoveryRequest<'_>,
) -> Result<OutputDiscovery, BackendError> {
    let dialect = request.dialect.to_polyglot();
    let expr = polyglot_sql::parse_one(request.sql, dialect)
        .map_err(|error| classify_polyglot_error(&error))?;
    let columns =
        infer_output_columns_with_expr(request.sql, dialect, request.catalog, Some(&expr));

    let mut duplicates = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut outputs = Vec::new();
    for name in columns {
        if !seen.insert(name.clone()) {
            duplicates.insert(name.clone());
        }
        outputs.push(
            crate::graph::column_lineage::backend::types::DiscoveredOutput {
                name: OutputName::Named(name),
            },
        );
    }

    Ok(OutputDiscovery {
        outputs,
        duplicate_names: duplicates,
    })
}

pub(crate) fn analyze(
    request: &crate::graph::column_lineage::backend::types::LineageRequest<'_>,
) -> Result<BackendAnalysis, BackendError> {
    let dialect = request.dialect.to_polyglot();
    let expr = polyglot_sql::parse_one(request.sql, dialect)
        .map_err(|error| classify_polyglot_error(&error))?;

    let schema = request.catalog.map(to_polyglot_schema);
    let mut expanded_expr = expr;
    polyglot_sql::lineage::expand_cte_stars(
        &mut expanded_expr,
        schema.as_ref().map(|s| s as &dyn polyglot_sql::Schema),
    );

    let context = LineageContext {
        expanded_expr,
        schema,
        dialect,
    };

    let results: Vec<_> = request
        .outputs
        .par_iter()
        .map(|o| analyze_single_output(o, &context, request.duplicate_output_names))
        .collect();

    let completeness = AnalysisCompleteness::Complete;

    Ok(BackendAnalysis {
        statements: vec![BackendStatementResult {
            statement_ordinal: 0,
            lineage_bearing: matches!(context.expanded_expr, Expression::Select(_)),
            completeness,
            has_unresolved_stars: has_unresolved_stars(&context.expanded_expr),
            columns: results,
        }],
    })
}

fn analyze_single_output(
    output: &OutputColumnRequest,
    context: &LineageContext,
    duplicate_output_names: &BTreeSet<String>,
) -> BackendColumnOutcome {
    let target = OutputTarget {
        ordinal: output.ordinal.clone(),
        name: OutputName::Named(output.name.clone()),
    };

    if duplicate_output_names.contains(&output.name) {
        return BackendColumnOutcome::Failed(BackendColumnFailure {
            target,
            resolution: ResolutionState::Ambiguous,
            error: BackendError {
                kind: BackendErrorKind::ColumnResolution {
                    state: ResolutionState::Ambiguous,
                },
                message: format!(
                    "cannot resolve output '{}' because the output name is duplicated",
                    output.name
                ),
            },
        });
    }

    match run_column_lineage_as_backend_result(&output.name, context) {
        Ok(mut result) => {
            result.target = target;
            BackendColumnOutcome::Resolved(result)
        }
        Err(error) => BackendColumnOutcome::Failed(BackendColumnFailure {
            target,
            resolution: match error.kind {
                BackendErrorKind::ColumnResolution { state } => state,
                _ => ResolutionState::Indeterminate,
            },
            error,
        }),
    }
}

fn infer_output_columns_with_expr(
    sql: &str,
    dialect: polyglot_sql::DialectType,
    catalog: Option<&CatalogSnapshot>,
    parsed_expr: Option<&Expression>,
) -> Vec<String> {
    if let Some(parsed_expr) = parsed_expr {
        return extract_select_columns_from_expr(parsed_expr, catalog);
    }
    let expr = match polyglot_sql::parse_one(sql, dialect) {
        Ok(expr) => expr,
        Err(_) => return Vec::new(),
    };
    extract_select_columns_from_expr(&expr, catalog)
}

#[derive(Debug, Clone)]
pub(super) struct LineageContext {
    pub(super) expanded_expr: Expression,
    schema: Option<polyglot_sql::MappingSchema>,
    dialect: polyglot_sql::DialectType,
}

pub(super) fn prepare_lineage_context(
    compiled_code: &str,
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: polyglot_sql::DialectType,
) -> Result<LineageContext, String> {
    prepare_lineage_context_with_expr(compiled_code, manifest, node, dialect, None)
}

pub(super) fn prepare_lineage_context_with_expr(
    compiled_code: &str,
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: polyglot_sql::DialectType,
    parsed_expr: Option<&Expression>,
) -> Result<LineageContext, String> {
    let expr = match parsed_expr {
        Some(e) => e.clone(),
        None => polyglot_sql::parse_one(compiled_code, dialect).map_err(|e| format!("{}", e))?,
    };

    let schema =
        schema::build_schema_from_manifest(manifest, node, dialect).map(|s| to_polyglot_schema(&s));

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
    run_column_lineage_raw(col_name, ctx).map_err(|error| format_lineage_error(&error))
}

fn run_column_lineage_raw(
    col_name: &str,
    ctx: &LineageContext,
) -> Result<ColumnLineageResult, polyglot_sql::Error> {
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

    lineage_result.map(|node| extract_leaf_sources(&node))
}

fn run_column_lineage_as_backend_result(
    col_name: &str,
    ctx: &LineageContext,
) -> Result<BackendColumnResult, BackendError> {
    let raw =
        run_column_lineage_raw(col_name, ctx).map_err(|error| classify_polyglot_error(&error))?;
    Ok(BackendColumnResult {
        target: OutputTarget {
            ordinal: OutputOrdinal(0),
            name: OutputName::Named(String::new()),
        },
        resolution: ResolutionState::Resolved,
        transformation: raw.transformation,
        sources: raw
            .sources
            .into_iter()
            .map(backend_source_from_legacy)
            .collect(),
    })
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

pub(crate) fn extract_select_columns_from_expr(
    expr: &Expression,
    schema: Option<&CatalogSnapshot>,
) -> Vec<String> {
    let schema = schema.map(to_polyglot_schema);
    let mut owned = expr.clone();
    polyglot_sql::lineage::expand_cte_stars(
        &mut owned,
        schema.as_ref().map(|s| s as &dyn polyglot_sql::Schema),
    );
    match &owned {
        Expression::Select(select) => select
            .expressions
            .iter()
            .filter_map(|e| match e {
                Expression::Alias(a) => Some(a.alias.name.clone()),
                Expression::Column(c) => {
                    if c.name.name == "*" {
                        None
                    } else {
                        Some(c.name.name.clone())
                    }
                }
                Expression::Identifier(id) => Some(id.name.clone()),
                Expression::Star(_) => None,
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

// The schema is built without a dialect on purpose. `MappingSchema::normalize_name`
// branches on the schema's own dialect, preserving case for BigQuery tables and
// upper-casing for Snowflake, while a dialect-less schema always lower-cases. Column
// lineage has always built these schemas dialect-less, so setting one here would change
// table resolution for those two dialects.
pub(crate) fn to_polyglot_schema(snapshot: &CatalogSnapshot) -> polyglot_sql::MappingSchema {
    let mut schema = polyglot_sql::MappingSchema::new();
    for table in snapshot.tables() {
        let cols = table
            .columns
            .iter()
            .map(|name| (name.clone(), DataType::Unknown))
            .collect::<Vec<_>>();
        let _ = schema.add_table(&table.name, &cols, None);
    }
    schema
}

pub(crate) fn format_lineage_error(e: &polyglot_sql::Error) -> String {
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

pub(crate) fn classify_polyglot_error(error: &polyglot_sql::Error) -> BackendError {
    let message = format_lineage_error(error);
    let state = if message.contains("Cannot find column") {
        ResolutionState::NotFound
    } else {
        ResolutionState::Indeterminate
    };
    BackendError {
        kind: BackendErrorKind::ColumnResolution { state },
        message,
    }
}

fn classify_transformation(node: &polyglot_sql::lineage::LineageNode) -> TransformationType {
    let t = classify_expression(&node.expression);
    if t != TransformationType::Direct || node.downstream.is_empty() {
        return t;
    }
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
        Expression::Function(_) => TransformationType::Expression,
        Expression::Upper(_)
        | Expression::Lower(_)
        | Expression::Length(_)
        | Expression::Concat(_) => TransformationType::Expression,
        _ => TransformationType::Unknown,
    }
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

fn backend_source_from_legacy(source: ColumnSource) -> BackendSource {
    BackendSource::Concrete {
        table: source.table,
        column: source.column,
    }
}

#[derive(Debug)]
pub(super) struct ColumnLineageResult {
    pub(super) sources: Vec<ColumnSource>,
    pub(super) transformation: TransformationType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use polyglot_sql::DialectType;

    #[test]
    fn test_extract_from_expr_cte_star() {
        let sql = r#"with
source as (select * from "raw"."raw_orders"),
renamed as (
    select id as order_id, customer as customer_id, ordered_at
    from source
)
select * from renamed"#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();
        let cols = extract_select_columns_from_expr(&expr, None);
        assert_eq!(cols, vec!["order_id", "customer_id", "ordered_at"]);
    }

    #[test]
    fn test_to_polyglot_schema_lower_cases_table_names_regardless_of_dialect() {
        // The converted schema must stay dialect-less. A schema built with
        // `MappingSchema::with_dialect` preserves case for BigQuery tables and
        // upper-cases for Snowflake, which would change table resolution for those
        // dialects. Column lineage has always resolved against lower-cased names.
        use polyglot_sql::Schema as _;

        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("Raw.Orders", ["id".to_string()]);

        let schema = to_polyglot_schema(&snapshot);

        assert!(
            schema.column_names("raw.orders").is_ok(),
            "table names must normalize to lower case"
        );
    }

    #[test]
    fn test_extract_from_expr_cte_star_with_cast() {
        let sql = r#"with
source as (
    select * from "jaffle_shop"."raw"."raw_orders"
),
renamed as (
    select
        id as order_id,
        store_id as location_id,
        customer as customer_id,
        subtotal as subtotal_cents,
        tax_paid as tax_paid_cents,
        order_total as order_total_cents,
        (subtotal / 100)::numeric(16, 2) as subtotal,
        (tax_paid / 100)::numeric(16, 2) as tax_paid,
        (order_total / 100)::numeric(16, 2) as order_total,
        date_trunc('day', ordered_at) as ordered_at
    from source
)
select * from renamed"#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();
        let cols = extract_select_columns_from_expr(&expr, None);
        assert!(cols.contains(&"order_id".to_string()), "cols: {:?}", cols);
        assert!(
            cols.contains(&"customer_id".to_string()),
            "cols: {:?}",
            cols
        );
        assert!(cols.contains(&"ordered_at".to_string()), "cols: {:?}", cols);
        assert!(
            cols.contains(&"order_total".to_string()),
            "cols: {:?}",
            cols
        );
        assert_eq!(cols.len(), 10, "cols: {:?}", cols);
    }
}
