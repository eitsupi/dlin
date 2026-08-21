#![allow(dead_code)]

use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisCompleteness {
    Complete,
    Indeterminate { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputOrdinal(pub usize);

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
    pub ordinal: OutputOrdinal,
    pub name: OutputName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputColumnRequest {
    pub ordinal: OutputOrdinal,
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
