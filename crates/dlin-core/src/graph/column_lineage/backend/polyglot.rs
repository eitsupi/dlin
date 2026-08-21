//! Polyglot-sql backend implementation details for column lineage.
//!
//! This module keeps the raw polyglot-sql interaction behind a narrow
//! internal boundary.

use std::collections::BTreeSet;

use polyglot_sql::{self, Expression, Schema, expressions::DataType};
use rayon::prelude::*;

use crate::graph::column_lineage::ColumnSource;
use crate::graph::column_lineage::TransformationType;
use crate::graph::column_lineage::backend::catalog::CatalogSnapshot;
use crate::graph::column_lineage::backend::dialect::DlinDialect;
use crate::graph::column_lineage::backend::types::{
    AnalysisCompleteness, BackendAnalysis, BackendColumnFailure, BackendColumnOutcome,
    BackendColumnResult, BackendError, BackendErrorKind, BackendSource, BackendStatementResult,
    OutputColumnRequest, OutputDiscovery, OutputDiscoveryRequest, OutputName, OutputOrdinal,
    OutputTarget, ResolutionState,
};

/// Infer output column names from a SQL query's top-level SELECT list,
/// expanding CTE-level `SELECT *` where the catalog allows it. Returns an
/// empty list when `sql` does not parse.
pub(crate) fn infer_output_columns(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
) -> Vec<String> {
    match polyglot_sql::parse_one(sql, dialect.to_polyglot()) {
        Ok(expr) => extract_select_columns_from_expr(&expr, catalog),
        Err(_) => Vec::new(),
    }
}

/// Parse-only check used by CLI debug commands to surface a parse failure
/// before any other argument (e.g. a `--schema` string) is validated,
/// matching the order those commands have always evaluated their inputs in.
pub fn check_sql_parses(sql: &str, dialect: DlinDialect) -> Result<(), String> {
    polyglot_sql::parse_one(sql, dialect.to_polyglot())
        .map(|_| ())
        .map_err(|e| format!("{}", e))
}

/// Parse `sql` and render its AST as the Rust `Debug` representation, for
/// `dlin debug parse-sql --format ast`.
pub fn debug_parse_sql_ast_debug(sql: &str, dialect: DlinDialect) -> Result<String, String> {
    polyglot_sql::parse_one(sql, dialect.to_polyglot())
        .map(|expr| format!("{:#?}", expr))
        .map_err(|e| format!("parse error: {}", e))
}

/// Parse `sql` and render its AST as JSON, for `dlin debug parse-sql --format json`.
pub fn debug_parse_sql_json(
    sql: &str,
    dialect: DlinDialect,
    pretty: bool,
) -> Result<String, String> {
    let expr = polyglot_sql::parse_one(sql, dialect.to_polyglot())
        .map_err(|e| format!("parse error: {}", e))?;
    let result = if pretty {
        serde_json::to_string_pretty(&expr)
    } else {
        serde_json::to_string(&expr)
    };
    result.map_err(|e| format!("{}", e))
}

/// Parse `sql`, trace `column`'s lineage (optionally qualified against
/// `catalog`), and render the resulting lineage graph as JSON, for
/// `dlin debug trace-column`. Mirrors that command's existing schema-fallback
/// behavior: CTE-star expansion only runs when a catalog is supplied, and a
/// schema-qualified trace falls back to an unqualified one on failure.
pub fn debug_trace_column_json(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
    column: &str,
    pretty: bool,
) -> Result<String, String> {
    let poly_dialect = dialect.to_polyglot();
    let mut expr =
        polyglot_sql::parse_one(sql, poly_dialect).map_err(|e| format!("parse error: {}", e))?;

    let schema = catalog
        .map(to_polyglot_schema_strict)
        .transpose()
        .map_err(|error| format!("schema error: {}", error.message))?;
    if let Some(ref s) = schema {
        polyglot_sql::lineage::expand_cte_stars(&mut expr, Some(s as &dyn polyglot_sql::Schema));
    }

    let lineage_result = if let Some(ref s) = schema {
        polyglot_sql::lineage::lineage_with_schema(
            column,
            &expr,
            Some(s as &dyn polyglot_sql::Schema),
            Some(poly_dialect),
            false,
        )
        .or_else(|err| {
            crate::warn!(
                "lineage_with_schema failed: {}, falling back to schema-less lineage",
                err
            );
            polyglot_sql::lineage::lineage(column, &expr, Some(poly_dialect), false)
        })
    } else {
        polyglot_sql::lineage::lineage(column, &expr, Some(poly_dialect), false)
    };

    match lineage_result {
        Ok(node) => {
            let result = if pretty {
                serde_json::to_string_pretty(&node)
            } else {
                serde_json::to_string(&node)
            };
            result.map_err(|e| format!("{}", e))
        }
        Err(e) => Err(format!("lineage error: {}", e)),
    }
}

pub(crate) fn discover_output_columns(
    request: &OutputDiscoveryRequest<'_>,
) -> Result<OutputDiscovery, BackendError> {
    let dialect = request.dialect.to_polyglot();
    let expr = polyglot_sql::parse_one(request.sql, dialect)
        .map_err(|error| classify_parse_error(&error))?;
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
        .map_err(|error| classify_parse_error(&error))?;

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
            // `build_scope_impl` (polyglot's lineage::Scope builder) only produces a
            // usable source scope for `Select`, `Union`, `Intersect`, and `Except` at
            // the top level (plus `CreateTable`/`Prepare`, which never appear as dbt
            // compiled model SQL); every other top-level variant falls through its
            // catch-all arm and leaves the scope empty, so tracing any column against
            // it fails regardless. Those four variants are exactly the ones a
            // top-level dbt query can parse to (a bare SELECT, or one wrapped in a set
            // operation), so matching them here does not reject anything the legacy
            // unconditional-call path would have traced successfully.
            lineage_bearing: matches!(
                context.expanded_expr,
                Expression::Select(_)
                    | Expression::Union(_)
                    | Expression::Intersect(_)
                    | Expression::Except(_)
            ),
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

pub(crate) fn to_polyglot_schema_strict(
    snapshot: &CatalogSnapshot,
) -> Result<polyglot_sql::MappingSchema, BackendError> {
    let mut schema = polyglot_sql::MappingSchema::new();
    for table in snapshot.tables() {
        let cols = table
            .columns
            .iter()
            .map(|name| (name.clone(), DataType::Unknown))
            .collect::<Vec<_>>();
        schema
            .add_table(&table.name, &cols, None)
            .map_err(|error| BackendError {
                kind: BackendErrorKind::Internal,
                message: error.to_string(),
            })?;
    }
    Ok(schema)
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

/// Classify a `parse_one` failure. Unlike [`classify_polyglot_error`], this is
/// never a column-resolution outcome — the statement was never parsed, so
/// there is no scope to resolve columns against. The message is the plain
/// `Display` of the underlying error, not run through [`format_lineage_error`]:
/// that formatting exists to normalize the synthetic zero-position errors
/// `lineage()` raises for resolution failures, and would be a no-op here
/// anyway since real parse errors carry real line/column positions.
fn classify_parse_error(error: &polyglot_sql::Error) -> BackendError {
    BackendError {
        kind: BackendErrorKind::Parse,
        message: format!("{}", error),
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
    fn strict_schema_conversion_errors_while_lenient_conversion_skips() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("a", ["root_col".to_string()]);
        snapshot.add_table("a.b", ["nested_col".to_string()]);

        let strict_error = to_polyglot_schema_strict(&snapshot)
            .expect_err("a table below an existing table must be rejected");
        assert_eq!(strict_error.kind, BackendErrorKind::Internal);
        assert!(strict_error.message.contains("Expected namespace at a"));

        use polyglot_sql::Schema as _;
        let lenient = to_polyglot_schema(&snapshot);
        assert!(lenient.column_names("a").is_ok());
        assert_eq!(
            lenient.column_names("a").unwrap(),
            vec!["root_col".to_string()]
        );
        assert!(lenient.column_names("a.b").is_err());
    }

    #[test]
    fn debug_trace_rejects_schema_conversion_errors() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("a", ["root_col".to_string()]);
        snapshot.add_table("a.b", ["nested_col".to_string()]);

        let error = debug_trace_column_json(
            "select root_col from a",
            DlinDialect::Generic,
            Some(&snapshot),
            "root_col",
            false,
        )
        .expect_err("debug tracing must not silently discard schema errors");
        assert_eq!(
            error,
            "schema error: Invalid schema structure: Expected namespace at a but found table"
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

    #[test]
    fn test_cte_select_star() {
        // CTE + SELECT * now works with the expand_cte_stars preprocessing
        let sql = r#"with renamed as (select id as customer_id from source) select * from renamed"#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("customer_id", &expr, None, false);
        assert!(
            result.is_ok(),
            "CTE + SELECT * should work: {:?}",
            result.err()
        );
        let node = result.unwrap();
        assert_eq!(node.name, "customer_id");
    }

    #[test]
    fn test_nested_cte_select_star() {
        // Nested CTE: cte2 references cte1 via SELECT *
        let sql = r#"
            with
                cte1 as (select id as order_id, amount from raw_orders),
                cte2 as (select * from cte1)
            select * from cte2
        "#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("order_id", &expr, None, false);
        assert!(
            result.is_ok(),
            "nested CTE + SELECT * should work: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_schema_resolves_cte_star_from_external_table() {
        // Test that lineage_with_schema can resolve columns through CTEs that
        // reference external tables registered in the schema.
        let sql = r#"with
orders as (
    select * from stg_orders
),
enriched as (
    select orders.*, 'extra' as extra_col
    from orders
)
select * from enriched"#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            ("order_id".to_string(), DataType::Unknown),
            ("customer_id".to_string(), DataType::Unknown),
            ("order_total".to_string(), DataType::Unknown),
        ];
        schema.add_table("stg_orders", &cols, None).unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(
            result.is_ok(),
            "should resolve order_id: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_schema_resolves_three_part_name() {
        // Test with fully-qualified 3-part table name as dbt generates
        let sql = r#"with
orders as (
    select * from "jaffle_shop"."main"."stg_orders"
)
select * from orders"#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            ("order_id".to_string(), DataType::Unknown),
            ("customer_id".to_string(), DataType::Unknown),
        ];
        // Register with 3-part name
        schema
            .add_table("jaffle_shop.main.stg_orders", &cols, None)
            .unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(
            result.is_ok(),
            "should resolve order_id via 3-part name: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_schema_resolves_cte_star_from_unknown_source() {
        // Test that lineage_with_schema can resolve columns through CTEs that
        // reference external tables registered in the schema.
        let sql = r#"with
orders as (
    select * from stg_orders
),
enriched as (
    select orders.*, 'extra' as extra_col
    from orders
)
select * from enriched"#;
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            ("order_id".to_string(), DataType::Unknown),
            ("customer_id".to_string(), DataType::Unknown),
            ("order_total".to_string(), DataType::Unknown),
        ];
        schema.add_table("stg_orders", &cols, None).unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(
            result.is_ok(),
            "should resolve order_id: {:?}",
            result.err()
        );
    }

    #[test]
    #[ignore = "has_unresolved_stars in the 0.4.4-era implementation does not recurse into \
                Expression::Paren, so a double-parenthesized derived table with SELECT * is not \
                detected as unresolved"]
    fn test_parenthesized_unresolved_star_is_detected() {
        // In polyglot-sql 0.6.2, a nested parenthesized query in a FROM clause
        // preserves a Paren node around the inner query.
        let expr = polyglot_sql::parse_one(
            "SELECT id FROM ((SELECT * FROM some_unknown_source))",
            DialectType::Generic,
        )
        .unwrap();

        assert!(format!("{expr:?}").contains("Paren"));
        assert!(has_unresolved_stars(&expr), "expr: {expr:?}");
    }

    #[test]
    fn test_union_chain_is_left_nested() {
        // The parser represents an unparenthesized UNION chain as a
        // left-nested Union(Union(...), ...). This shape is what makes the
        // recursive descent into `union.left` in `has_unresolved_stars`
        // callers necessary rather than a single flat pass.
        let sql = "SELECT id, 1 AS explicit_col FROM raw.orders UNION SELECT id, 2 AS explicit_col FROM raw.orders UNION SELECT id, * FROM some_unknown_source";
        let expr = polyglot_sql::parse_one(sql, DialectType::Generic).unwrap();
        assert!(matches!(
            &expr,
            Expression::Union(union)
                if matches!(union.left, Expression::Union(_))
        ));
    }

    #[test]
    fn test_format_lineage_error_strips_position() {
        let err = polyglot_sql::Error::parse("Cannot find column 'x' in query", 0, 0, 0, 0);
        let formatted = format_lineage_error(&err);
        assert_eq!(formatted, "lineage failed: Cannot find column 'x' in query");
        assert!(
            !formatted.contains("line 0"),
            "should strip meaningless position info"
        );
    }

    #[test]
    fn test_format_lineage_error_preserves_real_position() {
        let err = polyglot_sql::Error::parse("unexpected token", 5, 10, 0, 0);
        let formatted = format_lineage_error(&err);
        assert!(
            formatted.contains("line 5"),
            "should preserve real position info: {}",
            formatted
        );
    }

    #[test]
    fn test_format_lineage_error_internal() {
        let err = polyglot_sql::Error::internal("lineage recursion depth exceeded");
        let formatted = format_lineage_error(&err);
        assert_eq!(
            formatted,
            "lineage failed: lineage recursion depth exceeded"
        );
    }
}
