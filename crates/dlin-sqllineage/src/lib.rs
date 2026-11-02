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

//! Extract ordered table and column lineage with typed uncertainty.
//!
//! ```
//! use dlin_sqllineage::{Analyzer, Dialect};
//!
//! let statement = Analyzer::new(Dialect::Generic)
//!     .analyze("SELECT id FROM events")?
//!     .into_single()?;
//! assert_eq!(statement.output.columns[0].ordinal, Some(0));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Syntax errors fail the input. Semantic errors are attached to the affected
//! statement. JSON representations are for debugging and are not stable contracts.

mod analyzer;
mod build;
mod catalog;
mod diagnostic;
mod dialect;
mod graph;
mod ident;
mod keys;
mod output;
mod resolve;
mod types;

pub use analyzer::{Analyzer, ParsedSql, parse};
pub use catalog::*;
pub use diagnostic::*;
pub use ident::*;
pub use output::*;
pub use types::Dialect;
