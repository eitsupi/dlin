#![allow(dead_code)]

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisCompleteness {
    Complete,
    Indeterminate { reason: String },
}

/// Index of an output column within an analysis request. The request order is
/// the alphabetically sorted set of output column names rather than the SQL
/// projection order, so this is not a SQL projection position or output
/// ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisSlot(pub usize);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDiscovery {
    pub outputs: Vec<DiscoveredOutput>,
    pub duplicate_names: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputName {
    Named(String),
    UnaliasedExpression,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredOutput {
    pub name: OutputName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionState {
    Resolved,
    NotFound,
    Ambiguous,
    Indeterminate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendSource {
    Concrete {
        table: String,
        column: String,
    },
    Ambiguous {
        column: String,
        candidates: Vec<String>,
    },
    Wildcard {
        table: String,
    },
    Recursive {
        base_sources: Vec<BackendSource>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendColumnOutcome {
    Resolved(BackendColumnResult),
    Failed(BackendColumnFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendAnalysis {
    pub statements: Vec<BackendStatementResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStatementResult {
    pub statement_ordinal: usize,
    pub lineage_bearing: bool,
    pub completeness: AnalysisCompleteness,
    pub has_unresolved_stars: bool,
    /// One outcome is required for every output in the corresponding
    /// [`LineageRequest`]. Each outcome's slot must be in range, distinct from
    /// the other outcomes' slots, and name-matching for that requested slot.
    /// Outcomes may be returned in any order: vector position has no meaning,
    /// and is not a SQL projection position. A backend that cannot analyze an
    /// output must return [`BackendColumnOutcome::Failed`] for it rather than
    /// omit the outcome.
    pub columns: Vec<BackendColumnOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendColumnResult {
    pub target: OutputTarget,
    pub resolution: ResolutionState,
    pub transformation: crate::graph::column_lineage::TransformationType,
    pub sources: Vec<BackendSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendColumnFailure {
    pub target: OutputTarget,
    pub resolution: ResolutionState,
    pub error: BackendError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputTarget {
    pub slot: AnalysisSlot,
    pub name: OutputName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputColumnRequest {
    pub slot: AnalysisSlot,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendErrorKind {
    Parse,
    UnsupportedDialect,
    UnsupportedStatement,
    ColumnResolution { state: ResolutionState },
    IncompleteAnalysis,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError {
    pub kind: BackendErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDiscoveryRequest<'a> {
    pub sql: &'a str,
    pub dialect: crate::graph::column_lineage::backend::dialect::DlinDialect,
    pub catalog: Option<&'a crate::graph::column_lineage::backend::catalog::CatalogSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageRequest<'a> {
    pub sql: &'a str,
    pub dialect: crate::graph::column_lineage::backend::dialect::DlinDialect,
    pub catalog: Option<&'a crate::graph::column_lineage::backend::catalog::CatalogSnapshot>,
    pub outputs: &'a [OutputColumnRequest],
    pub duplicate_output_names: &'a BTreeSet<String>,
}

/// Put backend column outcomes into request-slot order while preserving every
/// unaffected outcome. A backend is required to return one outcome for each
/// requested slot, but malformed outcomes are diagnosed and replaced with a
/// visible internal failure for the affected slot.
pub fn normalize_column_outcomes(
    outputs: &[OutputColumnRequest],
    outcomes: Vec<BackendColumnOutcome>,
) -> (Vec<BackendColumnOutcome>, Vec<BackendError>) {
    let mut indexed: Vec<Option<BackendColumnOutcome>> = (0..outputs.len()).map(|_| None).collect();
    let mut diagnostics = Vec::new();

    for outcome in outcomes {
        let target = match &outcome {
            BackendColumnOutcome::Resolved(result) => &result.target,
            BackendColumnOutcome::Failed(failure) => &failure.target,
        };
        let slot = target.slot.0;

        if slot >= outputs.len() {
            diagnostics.push(BackendError {
                kind: BackendErrorKind::Internal,
                message: format!(
                    "backend returned an outcome for slot {slot}, but {} outputs were requested",
                    outputs.len()
                ),
            });
            continue;
        }

        if indexed[slot].is_some() {
            diagnostics.push(BackendError {
                kind: BackendErrorKind::Internal,
                message: format!(
                    "backend returned a duplicate outcome for slot {slot}; the later outcome was rejected"
                ),
            });
            continue;
        }

        // `UnaliasedExpression` has no textual name to compare with the
        // request. Its slot is still validated; only `Named` carries a name
        // that can be checked here.
        if let OutputName::Named(target_name) = &target.name
            && target_name != &outputs[slot].name
        {
            diagnostics.push(BackendError {
                kind: BackendErrorKind::Internal,
                message: format!(
                    "backend outcome for slot {slot} named '{target_name}', but the request names that slot '{}'",
                    outputs[slot].name
                ),
            });
            continue;
        }

        indexed[slot] = Some(outcome);
    }

    let normalized = indexed
        .into_iter()
        .enumerate()
        .map(|(slot, outcome)| {
            outcome.unwrap_or_else(|| {
                let request = &outputs[slot];
                let message = format!(
                    "backend returned no outcome for column '{}' at slot {slot}",
                    request.name
                );
                diagnostics.push(BackendError {
                    kind: BackendErrorKind::Internal,
                    message: message.clone(),
                });
                BackendColumnOutcome::Failed(BackendColumnFailure {
                    target: OutputTarget {
                        slot: AnalysisSlot(slot),
                        name: OutputName::Named(request.name.clone()),
                    },
                    resolution: ResolutionState::Indeterminate,
                    error: BackendError {
                        kind: BackendErrorKind::Internal,
                        message,
                    },
                })
            })
        })
        .collect();

    (normalized, diagnostics)
}

pub fn require_single_lineage_statement(
    analysis: BackendAnalysis,
) -> Result<BackendStatementResult, BackendError> {
    if analysis.statements.len() != 1 {
        let reason = if analysis.statements.is_empty() {
            "no statements in analysis result".to_string()
        } else {
            format!(
                "expected exactly one statement, found {}",
                analysis.statements.len()
            )
        };
        return Err(BackendError {
            kind: BackendErrorKind::IncompleteAnalysis,
            message: reason,
        });
    }

    let statement = analysis.statements.into_iter().next().ok_or(BackendError {
        kind: BackendErrorKind::IncompleteAnalysis,
        message: "no statements in analysis result".to_string(),
    })?;

    if !statement.lineage_bearing {
        return Err(BackendError {
            kind: BackendErrorKind::UnsupportedStatement,
            message: "statement is not lineage-bearing".to_string(),
        });
    }

    if let AnalysisCompleteness::Indeterminate { reason } = &statement.completeness {
        return Err(BackendError {
            kind: BackendErrorKind::IncompleteAnalysis,
            message: reason.clone(),
        });
    }

    Ok(statement)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(slot: usize, name: &str) -> OutputColumnRequest {
        OutputColumnRequest {
            slot: AnalysisSlot(slot),
            name: name.to_string(),
        }
    }

    fn resolved(
        slot: usize,
        name: &str,
        transformation: crate::graph::column_lineage::TransformationType,
    ) -> BackendColumnOutcome {
        BackendColumnOutcome::Resolved(BackendColumnResult {
            target: OutputTarget {
                slot: AnalysisSlot(slot),
                name: OutputName::Named(name.to_string()),
            },
            resolution: ResolutionState::Resolved,
            transformation,
            sources: vec![],
        })
    }

    fn failed(slot: usize, name: &str) -> BackendColumnOutcome {
        BackendColumnOutcome::Failed(BackendColumnFailure {
            target: OutputTarget {
                slot: AnalysisSlot(slot),
                name: OutputName::Named(name.to_string()),
            },
            resolution: ResolutionState::NotFound,
            error: BackendError {
                kind: BackendErrorKind::ColumnResolution {
                    state: ResolutionState::NotFound,
                },
                message: "not found".to_string(),
            },
        })
    }

    #[test]
    fn normalize_column_outcomes_restores_request_order() {
        let outputs = [request(0, "alpha"), request(1, "beta")];
        let (normalized, diagnostics) = normalize_column_outcomes(
            &outputs,
            vec![
                resolved(
                    1,
                    "beta",
                    crate::graph::column_lineage::TransformationType::Expression,
                ),
                resolved(
                    0,
                    "alpha",
                    crate::graph::column_lineage::TransformationType::Direct,
                ),
            ],
        );

        assert!(diagnostics.is_empty());
        assert_eq!(normalized.len(), 2);
        assert!(matches!(
            &normalized[0],
            BackendColumnOutcome::Resolved(result)
                if result.target.slot == AnalysisSlot(0)
                    && result.target.name == OutputName::Named("alpha".to_string())
                    && result.transformation == crate::graph::column_lineage::TransformationType::Direct
        ));
        assert!(matches!(
            &normalized[1],
            BackendColumnOutcome::Resolved(result)
                if result.target.slot == AnalysisSlot(1)
                    && result.target.name == OutputName::Named("beta".to_string())
                    && result.transformation == crate::graph::column_lineage::TransformationType::Expression
        ));
    }

    #[test]
    fn normalize_column_outcomes_synthesizes_missing_failure() {
        let outputs = [request(0, "alpha"), request(1, "beta")];
        let (normalized, diagnostics) = normalize_column_outcomes(
            &outputs,
            vec![resolved(
                1,
                "beta",
                crate::graph::column_lineage::TransformationType::Direct,
            )],
        );

        assert_eq!(normalized.len(), 2);
        assert!(matches!(
            &normalized[0],
            BackendColumnOutcome::Failed(failure)
                if failure.target.slot == AnalysisSlot(0)
                    && failure.target.name == OutputName::Named("alpha".to_string())
                    && failure.resolution == ResolutionState::Indeterminate
                    && failure.error.kind == BackendErrorKind::Internal
                    && failure.error.message == "backend returned no outcome for column 'alpha' at slot 0"
        ));
        assert!(matches!(
            &normalized[1],
            BackendColumnOutcome::Resolved(result)
                if result.target.slot == AnalysisSlot(1)
                    && result.target.name == OutputName::Named("beta".to_string())
        ));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == BackendErrorKind::Internal
                && diagnostic.message == "backend returned no outcome for column 'alpha' at slot 0"
        }));
    }

    #[test]
    fn normalize_column_outcomes_rejects_duplicate_without_displacing_first() {
        let outputs = [request(0, "alpha")];
        let (normalized, diagnostics) = normalize_column_outcomes(
            &outputs,
            vec![
                resolved(
                    0,
                    "alpha",
                    crate::graph::column_lineage::TransformationType::Direct,
                ),
                failed(0, "alpha"),
            ],
        );

        assert!(matches!(
            &normalized[0],
            BackendColumnOutcome::Resolved(result)
                if result.target.slot == AnalysisSlot(0)
                    && result.transformation == crate::graph::column_lineage::TransformationType::Direct
        ));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == BackendErrorKind::Internal
                && diagnostic.message
                    == "backend returned a duplicate outcome for slot 0; the later outcome was rejected"
        }));
    }

    #[test]
    fn normalize_column_outcomes_rejects_out_of_range_slot() {
        let outputs = [request(0, "alpha")];
        let (normalized, diagnostics) = normalize_column_outcomes(
            &outputs,
            vec![resolved(
                1,
                "beta",
                crate::graph::column_lineage::TransformationType::Direct,
            )],
        );

        assert!(matches!(
            &normalized[0],
            BackendColumnOutcome::Failed(failure)
                if failure.target.slot == AnalysisSlot(0)
                    && failure.target.name == OutputName::Named("alpha".to_string())
        ));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == BackendErrorKind::Internal
                && diagnostic.message
                    == "backend returned an outcome for slot 1, but 1 outputs were requested"
        }));
    }

    #[test]
    fn normalize_column_outcomes_rejects_name_mismatch() {
        let outputs = [request(0, "alpha")];
        let (normalized, diagnostics) = normalize_column_outcomes(
            &outputs,
            vec![resolved(
                0,
                "beta",
                crate::graph::column_lineage::TransformationType::Direct,
            )],
        );

        assert!(matches!(
            &normalized[0],
            BackendColumnOutcome::Failed(failure)
                if failure.target.slot == AnalysisSlot(0)
                    && failure.target.name == OutputName::Named("alpha".to_string())
                    && failure.error.kind == BackendErrorKind::Internal
        ));
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic.kind == BackendErrorKind::Internal
                && diagnostic.message
                    == "backend outcome for slot 0 named 'beta', but the request names that slot 'alpha'"
        }));
    }

    #[test]
    fn require_single_lineage_statement_rejects_empty_or_multi() {
        assert!(matches!(
            require_single_lineage_statement(BackendAnalysis {
                statements: Vec::new(),
            }),
            Err(err) if err.kind == BackendErrorKind::IncompleteAnalysis
        ));

        assert!(matches!(
            require_single_lineage_statement(BackendAnalysis {
                statements: vec![
                    BackendStatementResult {
                        statement_ordinal: 0,
                        lineage_bearing: true,
                        completeness: AnalysisCompleteness::Complete,
                        has_unresolved_stars: false,
                        columns: vec![],
                    },
                    BackendStatementResult {
                        statement_ordinal: 1,
                        lineage_bearing: true,
                        completeness: AnalysisCompleteness::Complete,
                        has_unresolved_stars: false,
                        columns: vec![],
                    },
                ],
            }),
            Err(err) if err.kind == BackendErrorKind::IncompleteAnalysis
        ));
    }

    #[test]
    fn require_single_lineage_statement_rejects_non_lineage_or_indeterminate() {
        let non_lineage = require_single_lineage_statement(BackendAnalysis {
            statements: vec![BackendStatementResult {
                statement_ordinal: 0,
                lineage_bearing: false,
                completeness: AnalysisCompleteness::Complete,
                has_unresolved_stars: false,
                columns: vec![],
            }],
        });
        assert!(
            matches!(non_lineage, Err(err) if err.kind == BackendErrorKind::UnsupportedStatement)
        );

        let indeterminate = require_single_lineage_statement(BackendAnalysis {
            statements: vec![BackendStatementResult {
                statement_ordinal: 0,
                lineage_bearing: true,
                completeness: AnalysisCompleteness::Indeterminate {
                    reason: "analysis ambiguous".to_string(),
                },
                has_unresolved_stars: false,
                columns: vec![],
            }],
        });
        assert!(
            matches!(indeterminate, Err(err) if err.kind == BackendErrorKind::IncompleteAnalysis)
        );
    }
}
