use sqllineage::{self, AnalyzeOptions, ColumnMapping, ColumnOrigin, StatementType, TransformKind};

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

        Ok(BackendAnalysis {
            statements: results
                .into_iter()
                .enumerate()
                .map(|(statement_ordinal, result)| {
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

fn analyze_output(
    output: &super::types::OutputColumnRequest,
    mappings: &[ColumnMapping],
    dialect: super::dialect::DlinDialect,
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
