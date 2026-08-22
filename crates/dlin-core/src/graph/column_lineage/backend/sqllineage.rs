use sqllineage::{self, AnalyzeOptions, ColumnMapping, ColumnOrigin, StatementType, TransformKind};
use sqlparser::ast::{Expr, Query, SelectItem, SetExpr, Statement, TableFactor};
use sqlparser::parser::Parser;

use super::catalog_provider::{SqllineageCatalogProvider, identifiers_match};
use super::{
    AnalysisCompleteness, BackendAnalysis, BackendColumnFailure, BackendColumnOutcome,
    BackendColumnResult, BackendError, BackendErrorKind, BackendId, BackendSource,
    BackendStatementResult, LineageBackend, LineageRequest, OutputDiscovery,
    OutputDiscoveryRequest, OutputTarget, ResolutionState,
};
use crate::graph::column_lineage::TransformationType;

/// The sqllineage-backed lineage implementation.
pub struct SqllineageBackend;

impl SqllineageBackend {
    pub const fn new() -> Self {
        Self
    }
}

fn not_implemented() -> BackendError {
    BackendError {
        kind: BackendErrorKind::Internal,
        message: "sqllineage backend is not implemented yet".to_string(),
    }
}

impl LineageBackend for SqllineageBackend {
    fn id(&self) -> BackendId {
        BackendId::Sqllineage
    }

    fn discover_output_columns(
        &self,
        _request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError> {
        Err(not_implemented())
    }

    fn analyze(&self, request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError> {
        let catalog = request.catalog.map(|snapshot| {
            Box::new(SqllineageCatalogProvider::new(snapshot, request.dialect))
                as Box<dyn sqllineage::CatalogProvider>
        });
        let results = sqllineage::analyze(
            request.sql,
            AnalyzeOptions {
                dialect: request.dialect.to_sqllineage(),
                catalog,
                normalize_case: true,
            },
        )
        .map_err(|error| BackendError {
            kind: BackendErrorKind::Parse,
            message: error.to_string(),
        })?;

        let shape_check = check_set_operation_shapes(request.sql, request.dialect);

        Ok(BackendAnalysis {
            statements: results
                .into_iter()
                .enumerate()
                .map(|(statement_ordinal, result)| {
                    let guard_reason = match &shape_check {
                        Ok(statements) => statements
                            .get(statement_ordinal)
                            .and_then(dangerous_set_operation_reason),
                        Err(error) => Some(error.as_str()),
                    };
                    let has_unresolved_stars = result
                        .columns
                        .mappings
                        .iter()
                        .any(mapping_has_unresolved_star);
                    let columns = request
                        .outputs
                        .iter()
                        .map(|output| {
                            analyze_output(
                                output,
                                &result.columns.mappings,
                                request.dialect,
                                request.duplicate_output_names,
                                has_unresolved_stars,
                                guard_reason,
                            )
                        })
                        .collect();

                    BackendStatementResult {
                        statement_ordinal,
                        // Every sqllineage statement type except `Other` has a query/DML
                        // lineage graph. `Other` is deliberately non-lineage-bearing because
                        // sqllineage documents it as DDL/DCL/other input with empty lineage.
                        lineage_bearing: !matches!(result.statement_type, StatementType::Other),
                        completeness: match guard_reason {
                            Some(reason) => AnalysisCompleteness::Indeterminate {
                                reason: reason.to_string(),
                            },
                            None => AnalysisCompleteness::Complete,
                        },
                        has_unresolved_stars,
                        columns,
                    }
                })
                .collect(),
        })
    }
}

fn analyze_output(
    output: &super::types::OutputColumnRequest,
    mappings: &[ColumnMapping],
    dialect: super::dialect::DlinDialect,
    duplicate_output_names: &std::collections::BTreeSet<String>,
    has_unresolved_stars: bool,
    guard_reason: Option<&str>,
) -> BackendColumnOutcome {
    let target = OutputTarget {
        slot: output.slot.clone(),
        name: output.name.clone(),
    };

    if duplicate_output_names.contains(&output.name) {
        return failed_output(
            target,
            ResolutionState::Ambiguous,
            format!(
                "cannot resolve output '{}' because the output name is duplicated",
                output.name
            ),
        );
    }

    if let Some(reason) = guard_reason {
        // Conservatively reject every requested output for the statement. A more precise
        // version could track which outputs actually descend from the affected set operation.
        return failed_output(target, ResolutionState::Indeterminate, reason.to_string());
    }

    let matching: Vec<&ColumnMapping> = mappings
        .iter()
        .filter(|mapping| identifiers_match(&mapping.target.column, &output.name, dialect))
        .collect();

    let [mapping] = matching.as_slice() else {
        if matching.is_empty() {
            // A `*` mapping is never adopted for a named output because its sources belong to
            // unknown output columns, not necessarily to the requested name.
            if has_unresolved_stars {
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
                    format!(
                        "cannot match output '{}' because an unexpanded SELECT * leaves the output columns unknown",
                        output.name
                    ),
                );
            }
            return failed_output(
                target,
                ResolutionState::NotFound,
                format!("no sqllineage mapping for output '{}'", output.name),
            );
        }
        return failed_output(
            target,
            ResolutionState::Ambiguous,
            format!(
                "more than one sqllineage mapping matches output '{}'",
                output.name
            ),
        );
    };

    let mut sources = Vec::with_capacity(mapping.sources.len());
    for origin in &mapping.sources {
        match origin {
            // This table is the complete sqllineage-to-dlin translation table. Keep
            // each origin structural: empty Ambiguous candidates mean unresolved,
            // while non-empty candidates mean genuine ambiguity.
            ColumnOrigin::Concrete { table, column } => {
                sources.push(BackendSource::Concrete {
                    table: table.to_string(),
                    column: column.clone(),
                });
            }
            ColumnOrigin::Ambiguous { column, candidates } if candidates.is_empty() => {
                return failed_output(
                    target,
                    ResolutionState::NotFound,
                    format!("column '{}' has no visible binding", column),
                );
            }
            ColumnOrigin::Ambiguous { column, candidates } => {
                return failed_output(
                    target,
                    ResolutionState::Ambiguous,
                    format!(
                        "column '{}' is ambiguous between {}",
                        column,
                        candidates
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                );
            }
            ColumnOrigin::Wildcard { table } => {
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
                    format!("column origin remains wildcard for table '{}'", table),
                );
            }
            ColumnOrigin::Recursive { .. } => {
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
                    "column origin is recursive and cannot be represented by dlin".to_string(),
                );
            }
        }
    }

    BackendColumnOutcome::Resolved(BackendColumnResult {
        target,
        resolution: ResolutionState::Resolved,
        transformation: transformation_from_sqllineage(&mapping.transform),
        sources,
    })
}

const DANGEROUS_SET_OPERATION_REASON: &str = "a set operation whose leading branch is SELECT * cannot be aligned with its other branches, so lineage for this statement cannot be trusted";

fn check_set_operation_shapes(
    sql: &str,
    dialect: super::dialect::DlinDialect,
) -> Result<Vec<Statement>, String> {
    // Obtain the parser dialect through sqllineage so this guard uses exactly the grammar that
    // produced the lineage result; maintaining a second hand-written dialect mapping could let
    // the guard inspect a different parse from sqllineage's.
    //
    // `sqllineage::analyze` makes this same `Parser::parse_sql` call with this same dialect and
    // emits one result per statement in order, so the statement list here matches the analysis
    // results position for position. Keep it that way: parsing by some other route would let the
    // two lists drift, and a statement whose shape went unchecked would silently lose its guard.
    let parser_dialect = dialect.to_sqllineage().to_sqlparser_dialect();
    Parser::parse_sql(&*parser_dialect, sql)
        .map_err(|error| format!("dlin could not verify the shape: {error}"))
}

fn dangerous_set_operation_reason(statement: &Statement) -> Option<&'static str> {
    statement_contains_dangerous_set_operation(statement).then_some(DANGEROUS_SET_OPERATION_REASON)
}

fn statement_contains_dangerous_set_operation(statement: &Statement) -> bool {
    match statement {
        Statement::Query(query) => query_contains_dangerous_set_operation(query),
        Statement::Insert(insert) => insert
            .source
            .as_ref()
            .is_some_and(|query| query_contains_dangerous_set_operation(query)),
        Statement::Directory { source, .. } => query_contains_dangerous_set_operation(source),
        Statement::CreateView(view) => query_contains_dangerous_set_operation(&view.query),
        _ => false,
    }
}

fn query_contains_dangerous_set_operation(query: &Query) -> bool {
    query.with.as_ref().is_some_and(|with| {
        with.cte_tables
            .iter()
            .any(|cte| query_contains_dangerous_set_operation(&cte.query))
    }) || set_expr_contains_dangerous_operation(&query.body)
}

fn set_expr_contains_dangerous_operation(body: &SetExpr) -> bool {
    match body {
        SetExpr::SetOperation { left, right, .. } => {
            leading_select_has_wildcard(left)
                || set_expr_contains_dangerous_operation(left)
                || set_expr_contains_dangerous_operation(right)
        }
        SetExpr::Query(query) => set_expr_contains_dangerous_operation(&query.body),
        SetExpr::Select(select) => {
            select.from.iter().any(|table| {
                table_factor_contains_dangerous_set_operation(&table.relation)
                    || table
                        .joins
                        .iter()
                        .any(|join| table_factor_contains_dangerous_set_operation(&join.relation))
            }) || select
                .projection
                .iter()
                .any(select_item_contains_dangerous_set_operation)
                || select
                    .selection
                    .as_ref()
                    .is_some_and(expr_contains_dangerous_set_operation)
                || select
                    .having
                    .as_ref()
                    .is_some_and(expr_contains_dangerous_set_operation)
                || select
                    .qualify
                    .as_ref()
                    .is_some_and(expr_contains_dangerous_set_operation)
        }
        SetExpr::Values(_)
        | SetExpr::Table(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_) => false,
    }
}

fn table_factor_contains_dangerous_set_operation(factor: &TableFactor) -> bool {
    match factor {
        TableFactor::Derived { subquery, .. } => query_contains_dangerous_set_operation(subquery),
        TableFactor::NestedJoin {
            table_with_joins, ..
        } => {
            table_factor_contains_dangerous_set_operation(&table_with_joins.relation)
                || table_with_joins
                    .joins
                    .iter()
                    .any(|join| table_factor_contains_dangerous_set_operation(&join.relation))
        }
        _ => false,
    }
}

fn select_item_contains_dangerous_set_operation(item: &SelectItem) -> bool {
    match item {
        SelectItem::UnnamedExpr(expr)
        | SelectItem::ExprWithAlias { expr, .. }
        | SelectItem::ExprWithAliases { expr, .. } => expr_contains_dangerous_set_operation(expr),
        SelectItem::QualifiedWildcard(..) | SelectItem::Wildcard(..) => false,
    }
}

fn expr_contains_dangerous_set_operation(expr: &Expr) -> bool {
    match expr {
        Expr::Exists { subquery, .. } | Expr::Subquery(subquery) => {
            query_contains_dangerous_set_operation(subquery)
        }
        Expr::InSubquery { expr, subquery, .. } => {
            expr_contains_dangerous_set_operation(expr)
                || query_contains_dangerous_set_operation(subquery)
        }
        Expr::Nested(expr) | Expr::UnaryOp { expr, .. } => {
            expr_contains_dangerous_set_operation(expr)
        }
        Expr::BinaryOp { left, right, .. } => {
            expr_contains_dangerous_set_operation(left)
                || expr_contains_dangerous_set_operation(right)
        }
        _ => false,
    }
}

fn leading_select_has_wildcard(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select.projection.iter().any(|item| {
            matches!(
                item,
                SelectItem::Wildcard(_) | SelectItem::QualifiedWildcard(..)
            )
        }),
        SetExpr::SetOperation { left, .. } => leading_select_has_wildcard(left),
        SetExpr::Query(query) => leading_select_has_wildcard(&query.body),
        SetExpr::Values(_)
        | SetExpr::Table(_)
        | SetExpr::Insert(_)
        | SetExpr::Update(_)
        | SetExpr::Delete(_)
        | SetExpr::Merge(_) => false,
    }
}

fn failed_output(
    target: OutputTarget,
    state: ResolutionState,
    message: String,
) -> BackendColumnOutcome {
    BackendColumnOutcome::Failed(BackendColumnFailure {
        target,
        resolution: state,
        error: BackendError {
            kind: BackendErrorKind::ColumnResolution { state },
            message,
        },
    })
}

fn transformation_from_sqllineage(kind: &TransformKind) -> TransformationType {
    // The decided translation table is Direct->Direct, Aggregation->Aggregation,
    // Expression->Expression, Conditional->Conditional, Unknown->Unknown, and
    // Window->Expression. sqllineage intentionally classifies CAST as Direct,
    // so dlin's Cast variant is unreachable through this backend.
    match kind {
        TransformKind::Direct => TransformationType::Direct,
        TransformKind::Aggregation => TransformationType::Aggregation,
        TransformKind::Expression => TransformationType::Expression,
        TransformKind::Conditional => TransformationType::Conditional,
        TransformKind::Unknown => TransformationType::Unknown,
        TransformKind::Window => TransformationType::Expression,
    }
}

fn mapping_has_unresolved_star(mapping: &ColumnMapping) -> bool {
    mapping.sources.iter().any(origin_has_unresolved_star)
}

fn origin_has_unresolved_star(origin: &ColumnOrigin) -> bool {
    match origin {
        ColumnOrigin::Concrete { .. } | ColumnOrigin::Ambiguous { .. } => false,
        ColumnOrigin::Wildcard { .. } => true,
        ColumnOrigin::Recursive { base_sources } => {
            base_sources.iter().any(origin_has_unresolved_star)
        }
    }
}
