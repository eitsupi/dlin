//! Structured parse errors and statement diagnostics.

use serde::Serialize;
use std::fmt;

/// Error encountered while parsing SQL.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ParseError {
    /// Human-readable parser message.
    pub message: String,
    /// Broad parser failure category.
    pub kind: ParseErrorKind,
    /// Position in the original SQL, if available.
    pub location: Option<Location>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SQL parse error: {}", self.message)
    }
}

impl std::error::Error for ParseError {}

/// Broad category of a SQL parsing error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ParseErrorKind {
    /// SQL syntax was invalid.
    Syntax,
    /// Tokenization failed.
    Tokenizer,
    /// Parser recursion exceeded its configured limit.
    RecursionLimit,
}

/// Line and column in the source SQL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Location {
    /// One-based line number.
    pub line: usize,
    /// One-based column number.
    pub column: usize,
}

/// A non-fatal or statement-local analysis finding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    /// Impact of this diagnostic on the statement result.
    pub severity: Severity,
    /// Kind of finding.
    pub kind: DiagnosticKind,
    /// Source location, if available.
    pub location: Option<Location>,
}

/// Severity of a diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Severity {
    /// Analysis completed, possibly with a warning.
    Warning,
    /// The statement's lineage could not be trusted or produced.
    Error,
}

/// Structured kind of an analysis diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum DiagnosticKind {
    /// Set-operation branches have different output arities.
    SetOperationArityMismatch {
        /// Number of columns in the left operand.
        left: usize,
        /// Number of columns in the right operand.
        right: usize,
    },
    /// A cycle was detected in the lineage graph.
    UnexpectedCycle,
    /// A query has an empty projection list.
    ///
    /// A bare `FROM t` in dialects that allow it is read as `SELECT *` and only
    /// warns; the diagnostic still tells callers that no select list was written.
    EmptyProjection,
    /// Multiple output columns share a name, compared by
    /// [`Ident::column_key`](super::Ident::column_key) for the analyzer's
    /// dialect.
    DuplicateOutputName {
        /// Duplicated output name.
        name: super::Ident,
        /// Known output positions. Positions after an unexpanded star are omitted.
        ordinals: Vec<usize>,
    },
    /// The statement contains syntax that is not modeled.
    UnsupportedSyntax {
        /// Stable description of the unsupported construct.
        what: &'static str,
    },
}

impl DiagnosticKind {
    /// Return the stable uppercase code for this diagnostic variant.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SetOperationArityMismatch { .. } => "SET_OPERATION_ARITY_MISMATCH",
            Self::UnexpectedCycle => "UNEXPECTED_CYCLE",
            Self::EmptyProjection => "EMPTY_PROJECTION",
            Self::DuplicateOutputName { .. } => "DUPLICATE_OUTPUT_NAME",
            Self::UnsupportedSyntax { .. } => "UNSUPPORTED_SYNTAX",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_codes_are_stable_variant_names() {
        let cases = [
            (
                DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 },
                "SET_OPERATION_ARITY_MISMATCH",
            ),
            (DiagnosticKind::UnexpectedCycle, "UNEXPECTED_CYCLE"),
            (DiagnosticKind::EmptyProjection, "EMPTY_PROJECTION"),
            (
                DiagnosticKind::DuplicateOutputName {
                    name: super::super::Ident::unquoted("x"),
                    ordinals: vec![0, 1],
                },
                "DUPLICATE_OUTPUT_NAME",
            ),
            (
                DiagnosticKind::UnsupportedSyntax { what: "pipe" },
                "UNSUPPORTED_SYNTAX",
            ),
        ];
        for (kind, code) in cases {
            assert_eq!(kind.code(), code);
        }
    }
}
