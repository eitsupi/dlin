use sqllineage::{self, AnalyzeOptions, ColumnMapping, ColumnOrigin, StatementType, TransformKind};
use sqlparser::ast::{Query, SetExpr, Statement, Visit, Visitor};
use sqlparser::parser::Parser;
use std::ops::ControlFlow;

use super::catalog_provider::{
    SqllineageCatalogProvider, column_identifiers_match, resolve_table_for_sqllineage,
};
use super::{
    AnalysisCompleteness, BackendAnalysis, BackendColumnFailure, BackendColumnOutcome,
    BackendColumnResult, BackendError, BackendErrorKind, BackendId, BackendSource,
    BackendStatementResult, LineageBackend, LineageRequest, OutputDiscovery,
    OutputDiscoveryRequest, OutputTarget, ResolutionState,
};
use crate::graph::column_lineage::TransformationType;
use crate::graph::column_lineage::relation::RelationRef;

/// The sqllineage-backed lineage implementation.
pub struct SqllineageBackend;

impl SqllineageBackend {
    pub const fn new() -> Self {
        Self
    }
}

/// Parse-only check used by the CLI debug commands before validating any
/// optional schema arguments.
pub fn check_sql_parses(sql: &str, dialect: super::dialect::DlinDialect) -> Result<(), String> {
    parse_statements(sql, dialect).map(|_| ())
}

/// Parse SQL and render its sqlparser AST using Rust's debug representation.
pub fn debug_parse_sql_ast_debug(
    sql: &str,
    dialect: super::dialect::DlinDialect,
) -> Result<String, String> {
    parse_statements(sql, dialect)
        .map(|statements| format!("{statements:#?}"))
        .map_err(|error| format!("parse error: {error}"))
}

/// Parse SQL and render its sqlparser AST as JSON.
pub fn debug_parse_sql_json(
    sql: &str,
    dialect: super::dialect::DlinDialect,
    pretty: bool,
) -> Result<String, String> {
    let statements =
        parse_statements(sql, dialect).map_err(|error| format!("parse error: {error}"))?;
    if pretty {
        serde_json::to_string_pretty(&statements)
    } else {
        serde_json::to_string(&statements)
    }
    .map_err(|error| error.to_string())
}

/// Analyze SQL and render the requested column's sqllineage mappings as JSON.
/// The result intentionally exposes the upstream analyzer's public shape,
/// including statement-wide unresolved-star information.
pub fn debug_trace_column_json(
    sql: &str,
    dialect: super::dialect::DlinDialect,
    catalog: Option<&super::CatalogSnapshot>,
    column: &str,
    pretty: bool,
) -> Result<String, String> {
    let mut results = analyze_sql(sql, dialect, catalog).map_err(|error| error.message)?;
    if results.len() != 1 {
        let message = if results.is_empty() {
            "no statements in analysis result".to_string()
        } else {
            format!("expected exactly one statement, found {}", results.len())
        };
        return Err(message);
    }
    let result = results.pop().expect("result length checked above");
    let mappings = result
        .columns
        .mappings
        .iter()
        .filter(|mapping| column_identifiers_match(&mapping.target.column, column, dialect))
        .collect::<Vec<_>>();
    if mappings.is_empty() {
        return Err(format!("lineage error: no mapping for column '{column}'"));
    }
    let value = serde_json::json!({
        "column": column,
        "mappings": mappings,
        "has_unresolved_stars": result.columns.has_unresolved_stars,
    });
    if pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|error| error.to_string())
}

fn parse_statements(
    sql: &str,
    dialect: super::dialect::DlinDialect,
) -> Result<Vec<Statement>, String> {
    let parser_dialect = dialect
        .to_sqllineage()
        .map_err(|error| error.message)?
        .to_sqlparser_dialect();
    Parser::parse_sql(&*parser_dialect, sql).map_err(|error| error.to_string())
}

impl LineageBackend for SqllineageBackend {
    fn id(&self) -> BackendId {
        BackendId::Sqllineage
    }

    fn discover_output_columns(
        &self,
        request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError> {
        let mut results = analyze_sql(request.sql, request.dialect, request.catalog)?;
        if results.len() != 1 {
            let message = if results.is_empty() {
                "no statements in analysis result".to_string()
            } else {
                format!("expected exactly one statement, found {}", results.len())
            };
            return Err(BackendError {
                kind: BackendErrorKind::IncompleteAnalysis,
                message,
            });
        }

        let result = results.pop().expect("result length checked above");
        if matches!(result.statement_type, StatementType::Other) {
            return Err(BackendError {
                kind: BackendErrorKind::UnsupportedStatement,
                message: "statement is not lineage-bearing".to_string(),
            });
        }

        let mut duplicates = std::collections::BTreeSet::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut outputs = Vec::new();
        for mapping in result.columns.mappings {
            // Drop sqllineage's unexpanded-`SELECT *` sentinel. It takes both signals
            // to identify: the target is named "*" and the origins are an unresolved
            // wildcard. The name alone would discard a column legitimately aliased to
            // "*", and the origins alone would discard a properly named column whose
            // lineage happens to run through a star.
            if mapping.target.column == "*" && mapping_has_unresolved_star(&mapping) {
                continue;
            }
            let name = mapping.target.column;
            if !seen.insert(name.clone()) {
                duplicates.insert(name.clone());
            }
            outputs.push(super::DiscoveredOutput {
                name: super::OutputName::Named(name),
            });
        }

        Ok(OutputDiscovery {
            outputs,
            duplicate_names: duplicates,
        })
    }

    fn analyze(&self, request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError> {
        let shape_check = check_statement_shapes(request.sql, request.dialect);
        if let Err(error) = &shape_check
            && matches!(&error.kind, BackendErrorKind::UnsupportedDialect)
        {
            return Err(error.clone());
        }

        if let Ok(statements) = &shape_check
            && statements.iter().any(statement_has_empty_projection)
        {
            return Err(BackendError {
                kind: BackendErrorKind::Parse,
                message: "dlin found a SELECT with an empty projection".to_owned(),
            });
        }

        let results = analyze_sql(request.sql, request.dialect, request.catalog)?;

        Ok(BackendAnalysis {
            statements: results
                .into_iter()
                .enumerate()
                .map(|(statement_ordinal, result)| {
                    // The public result flag includes unresolved stars nested in
                    // expressions (for example, inside a JOIN) that do not produce a
                    // column mapping. Do not infer completeness from mappings alone.
                    let has_unresolved_stars = result.columns.has_unresolved_stars;
                    let columns = request
                        .outputs
                        .iter()
                        .map(|output| {
                            analyze_output(
                                output,
                                &result.columns.mappings,
                                request.dialect,
                                request.catalog,
                                request.duplicate_output_names,
                                has_unresolved_stars,
                            )
                        })
                        .collect();

                    BackendStatementResult {
                        statement_ordinal,
                        // Every sqllineage statement type except `Other` has a query/DML
                        // lineage graph. `Other` is deliberately non-lineage-bearing because
                        // sqllineage documents it as DDL/DCL/other input with empty lineage.
                        lineage_bearing: !matches!(result.statement_type, StatementType::Other),
                        completeness: AnalysisCompleteness::Complete,
                        has_unresolved_stars,
                        columns,
                    }
                })
                .collect(),
        })
    }
}

fn analyze_sql(
    sql: &str,
    dialect: super::dialect::DlinDialect,
    catalog: Option<&super::CatalogSnapshot>,
) -> Result<Vec<sqllineage::AnalyzeResult>, BackendError> {
    let sqllineage_dialect = dialect.to_sqllineage()?;
    let catalog = catalog.map(|snapshot| {
        Box::new(SqllineageCatalogProvider::new(snapshot, dialect))
            as Box<dyn sqllineage::CatalogProvider>
    });
    sqllineage::analyze(
        sql,
        AnalyzeOptions {
            dialect: sqllineage_dialect,
            catalog,
            normalize_case: true,
        },
    )
    .map_err(|error| BackendError {
        kind: BackendErrorKind::Parse,
        message: error.to_string(),
    })
}

fn analyze_output(
    output: &super::types::OutputColumnRequest,
    mappings: &[ColumnMapping],
    dialect: super::dialect::DlinDialect,
    catalog: Option<&super::catalog::CatalogSnapshot>,
    duplicate_output_names: &std::collections::BTreeSet<String>,
    has_unresolved_stars: bool,
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

    let matching: Vec<&ColumnMapping> = mappings
        .iter()
        .filter(|mapping| column_identifiers_match(&mapping.target.column, &output.name, dialect))
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
                let raw_relation = RelationRef::from_backend(
                    table.catalog.as_deref(),
                    table.schema.as_deref(),
                    &table.table,
                );
                let relation = catalog
                    .and_then(|catalog| {
                        resolve_table_for_sqllineage(catalog, &raw_relation, dialect)
                    })
                    .map(|catalog_table| catalog_table.relation.clone())
                    .unwrap_or(raw_relation);
                sources.push(BackendSource::Concrete {
                    relation,
                    column: column.clone(),
                });
            }
            ColumnOrigin::Ambiguous { column, candidates } if candidates.is_empty() => {
                // sqllineage uses this same origin for an unbound output and for an output
                // whose source is hidden behind an unexpanded star. The latter exists but has
                // incomplete lineage; Indeterminate is the only label that is true for both.
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
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
            ColumnOrigin::NamedWildcard { table, column } => {
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
                    format!(
                        "column '{}' remains behind an unexpanded wildcard for table '{}'",
                        column, table
                    ),
                );
            }
            // A source-free branch (for example `CAST(NULL AS STRING)` in a set
            // operation) is known to contribute no upstream column.  It must not
            // make a concrete origin from another branch indeterminate; simply
            // omit it from dlin's source list.  If it is the only origin, the
            // resolved result correctly has no sources, just like a literal
            // expression outside a set operation.
            ColumnOrigin::SourceFree { .. } => {}
            ColumnOrigin::Recursive { .. } => {
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
                    "column origin is recursive and cannot be represented by dlin".to_string(),
                );
            }
            _ => {
                return failed_output(
                    target,
                    ResolutionState::Indeterminate,
                    "column origin is not recognized by this dlin adapter".to_string(),
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

fn check_statement_shapes(
    sql: &str,
    dialect: super::dialect::DlinDialect,
) -> Result<Vec<Statement>, BackendError> {
    // This is the one deliberate second parse for statement-shape checks: keep all such
    // structural checks here so their statements stay aligned with the lineage results.
    // Obtain the parser dialect through sqllineage so these statement-shape checks use exactly
    // the grammar that produced the lineage result; maintaining a second hand-written dialect
    // mapping could let the shape check inspect a different parse from sqllineage's.
    //
    // `sqllineage::analyze` makes this same `Parser::parse_sql` call with this same dialect and
    // emits one result per statement in order, so the statement list here matches the analysis
    // results position for position. Keep it that way: parsing by some other route would let the
    // two lists drift, and a statement whose shape went unchecked could bypass validation.
    let parser_dialect = dialect.to_sqllineage()?.to_sqlparser_dialect();
    Parser::parse_sql(&*parser_dialect, sql).map_err(|error| BackendError {
        kind: BackendErrorKind::Parse,
        message: format!("dlin could not verify the shape: {error}"),
    })
}

fn statement_has_empty_projection(statement: &Statement) -> bool {
    let mut visitor = EmptyProjectionVisitor::default();
    let _ = statement.visit(&mut visitor);
    visitor.empty
}

#[derive(Default)]
struct EmptyProjectionVisitor {
    empty: bool,
}

impl Visitor for EmptyProjectionVisitor {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        self.empty |= set_expr_contains_empty_projection(&query.body);
        ControlFlow::Continue(())
    }
}

fn set_expr_contains_empty_projection(body: &SetExpr) -> bool {
    match body {
        SetExpr::Select(select) => select.projection.is_empty(),
        SetExpr::SetOperation { left, right, .. } => {
            set_expr_contains_empty_projection(left) || set_expr_contains_empty_projection(right)
        }
        SetExpr::Query(_)
        | SetExpr::Values(_)
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
        ColumnOrigin::Concrete { .. }
        | ColumnOrigin::Ambiguous { .. }
        | ColumnOrigin::SourceFree { .. } => false,
        ColumnOrigin::Wildcard { .. } | ColumnOrigin::NamedWildcard { .. } => true,
        ColumnOrigin::Recursive { base_sources } => {
            base_sources.iter().any(origin_has_unresolved_star)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{debug_parse_sql_ast_debug, debug_trace_column_json};
    use crate::graph::column_lineage::DlinDialect;

    #[test]
    fn debug_parse_error_has_a_single_prefix() {
        let error = debug_parse_sql_ast_debug("SELECT FROM", DlinDialect::Generic)
            .expect_err("invalid SQL should fail to parse");
        assert!(error.starts_with("parse error: "));
        assert!(!error.contains("parse error: parse error:"));
    }

    #[test]
    fn debug_trace_rejects_multiple_statements() {
        let error = debug_trace_column_json(
            "SELECT id FROM orders; SELECT id FROM customers",
            DlinDialect::Generic,
            None,
            "id",
            false,
        )
        .expect_err("debug trace accepts exactly one statement");
        assert_eq!(error, "expected exactly one statement, found 2");
    }
}
