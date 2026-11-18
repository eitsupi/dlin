//! Dependency extraction by abstract interpretation of the Jinja AST.
//!
//! The template is interpreted, not rendered: values that only dbt knows at
//! run time are unknown, with a reason. A `ref()` or `source()` whose
//! arguments are all known is a dependency. When an unknown value selects a
//! dependency (a `ref()` argument, a condition, a loop, a callee), its
//! reasons are recorded; an unknown value that is only rendered is not a
//! reason. Calls whose callee is unknown, unsupported constructs and
//! analysis limits are always recorded, since they may hide dependencies.

use std::collections::HashMap;

#[doc(hidden)]
pub mod compare;
mod convert;
mod eval;
mod ir;
mod known;
pub(crate) mod library;
#[cfg(test)]
mod tests;
mod value;

#[allow(unused_imports)]
pub(crate) use eval::{Decision, DecisionOutcome};
pub(crate) use library::MacroLibrary;

use crate::graph::dependency::{
    AnalysisLimit, DependencyReason, ExtractedDependencyReason, ExtractionSite,
};
use crate::parser::sql::{RefCall, SourceCall, SqlConfig, recovery};
use eval::{Interp, Signal};
use ir::SourceId;

/// The dependencies of one SQL file and why they may be incomplete.
#[derive(Debug, Default)]
pub(crate) struct DependencyExtraction {
    pub(crate) refs: Vec<RefCall>,
    pub(crate) sources: Vec<SourceCall>,
    pub(crate) config: SqlConfig,
    /// Sorted and deduplicated.
    pub(crate) reasons: Vec<ExtractedDependencyReason>,
    /// How each ref/source call was treated, for comparing extractors.
    pub(crate) decisions: Vec<Decision>,
    /// Names of the macros that were called.
    pub(crate) entered: std::collections::BTreeSet<std::sync::Arc<str>>,
}

/// Extract the dependencies of a SQL file.
pub(crate) fn extract_dependencies(
    sql: &str,
    library: &MacroLibrary,
    vars: &HashMap<String, serde_json::Value>,
) -> DependencyExtraction {
    let template = match convert::parse_template(sql, SourceId::Template) {
        Ok(template) => template,
        Err(failure) => return recover(sql, failure.line),
    };
    let mut interp = Interp::new(library, sql, vars);
    // dbt renders `is_incremental()` as false while parsing; both branches
    // are followed so that incremental-only dependencies are kept.
    for incremental in [false, true] {
        interp.incremental = incremental;
        interp.record_config = !incremental;
        if let Err(Signal::Budget) = interp.run_template(&template.body) {
            let at = interp
                .current
                .map(|span| interp.site(span))
                .unwrap_or_default();
            interp
                .rec
                .reasons
                .insert(DependencyReason::AnalysisLimitReached {
                    limit: AnalysisLimit::WorkBudget,
                    at,
                });
            break;
        }
    }
    let mut rec = interp.rec;
    if rec.materialized_unknown {
        rec.config.materialized = None;
    }
    // The two passes record the same calls again.
    let mut seen = Vec::new();
    rec.decisions.retain(|decision| {
        let new = !seen.contains(decision);
        if new {
            seen.push(decision.clone());
        }
        new
    });
    DependencyExtraction {
        refs: rec.refs,
        sources: rec.sources,
        config: rec.config,
        reasons: rec.reasons.into_iter().collect(),
        decisions: rec.decisions,
        entered: rec.entered,
    }
}

/// A template that cannot be parsed: keep only calls whose arguments are
/// all literals.
fn recover(sql: &str, line: Option<u32>) -> DependencyExtraction {
    let (refs, sources) = recovery::extract_refs_and_sources_regex_scoped(sql, None);
    DependencyExtraction {
        refs,
        sources,
        config: recovery::extract_config_regex(sql),
        reasons: vec![DependencyReason::JinjaRenderFailed {
            at: ExtractionSite { line, expr: None },
        }],
        decisions: Vec::new(),
        entered: Default::default(),
    }
}
