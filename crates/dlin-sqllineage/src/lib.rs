#![warn(clippy::pedantic, clippy::nursery)]
#![allow(
    clippy::module_name_repetitions,
    clippy::must_use_candidate,
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::redundant_else,
    clippy::redundant_pub_crate,
    clippy::too_many_lines,
    clippy::option_if_let_else,
    clippy::missing_const_for_fn,
    clippy::use_self,
    clippy::match_same_arms,
    clippy::match_wildcard_for_single_variants
)]

//! # sqllineage
//!
//! Extract table-level and column-level data lineage from SQL statements.
//!
//! Given a SQL string, `sqllineage` produces:
//! - **Table-level lineage** — which tables are read (inputs) and written (output)
//! - **Column-level lineage** — which source columns each output column derives from
//!
//! The crate is **schema-agnostic by default**. When a [`CatalogProvider`] is
//! supplied, it resolves `SELECT *` and ambiguous unqualified columns.
//!
//! # Quick start
//!
//! ```
//! use sqllineage::{analyze, AnalyzeOptions};
//!
//! let results = analyze(
//!     "INSERT INTO summary SELECT user_id, SUM(score) AS total FROM events GROUP BY user_id",
//!     AnalyzeOptions::default(),
//! ).unwrap();
//!
//! let result = &results[0];
//! assert_eq!(result.tables.inputs[0].table, "events");
//! assert_eq!(result.tables.output.as_ref().unwrap().table, "summary");
//! assert_eq!(result.columns.mappings.len(), 2);
//! ```

mod build;
mod dialect;
mod graph;
mod resolve;
pub mod types;

pub use types::*;

use sqlparser::parser::Parser;

/// Analyze a SQL string and extract lineage for each statement.
///
/// Returns one [`AnalyzeResult`] per statement in the input. Statements
/// that carry no lineage (DDL, GRANT, etc.) are included with
/// [`StatementType::Other`] and empty lineage.
///
/// # Errors
///
/// Returns [`ParseError`] if the SQL string cannot be parsed or fails semantic
/// validation during lineage analysis (for example, exact set-operation
/// branches with different column counts).
#[allow(clippy::needless_pass_by_value)]
pub fn analyze(sql: &str, opts: AnalyzeOptions) -> Result<Vec<AnalyzeResult>, ParseError> {
    let dialect = opts.dialect.to_sqlparser_dialect();
    let statements = Parser::parse_sql(&*dialect, sql).map_err(|e| ParseError {
        message: e.to_string(),
    })?;

    let catalog = opts.catalog;
    statements
        .iter()
        .map(|stmt| {
            let builder = build::LineageBuilder::new(opts.normalize_case, opts.dialect);
            let (raw_graph, warnings, statement_type) = builder.build(stmt);
            resolve::resolve(raw_graph, catalog.as_deref(), warnings, statement_type)
        })
        .collect()
}
