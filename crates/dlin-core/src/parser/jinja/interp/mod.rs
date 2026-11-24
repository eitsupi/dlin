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
    /// Deduplicated, in the order first called.
    pub(crate) refs: Vec<RefCall>,
    /// Where each of `refs` was first called, by index.
    pub(crate) ref_sites: Vec<ExtractionSite>,
    /// Deduplicated, in the order first called.
    pub(crate) sources: Vec<SourceCall>,
    /// Where each of `sources` was first called, by index.
    pub(crate) source_sites: Vec<ExtractionSite>,
    pub(crate) config: SqlConfig,
    /// Sorted and deduplicated.
    pub(crate) reasons: Vec<ExtractedDependencyReason>,
    /// How each ref/source call was treated, checked by the tests.
    #[cfg_attr(not(test), expect(dead_code, reason = "only the tests read it"))]
    pub(crate) decisions: Vec<Decision>,
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
            let at = interp.budget_site.take().unwrap_or_else(|| {
                interp
                    .current
                    .map(|span| interp.site(span))
                    .unwrap_or_default()
            });
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
        ref_sites: rec.ref_sites,
        sources: rec.sources,
        source_sites: rec.source_sites,
        config: rec.config,
        reasons: rec.reasons.into_iter().collect(),
        decisions: rec.decisions,
    }
}

/// A template that cannot be parsed: keep only calls whose arguments are
/// all literals.
fn recover(sql: &str, line: Option<u32>) -> DependencyExtraction {
    let (mut refs, mut sources) = recovery::literal_calls(sql);
    dedup_in_order(&mut refs);
    dedup_in_order(&mut sources);
    DependencyExtraction {
        ref_sites: vec![ExtractionSite::default(); refs.len()],
        source_sites: vec![ExtractionSite::default(); sources.len()],
        refs,
        sources,
        config: recovery::extract_config_regex(sql),
        reasons: vec![DependencyReason::JinjaRenderFailed {
            at: ExtractionSite {
                line,
                ..ExtractionSite::default()
            },
        }],
        decisions: Vec::new(),
    }
}

/// Remove repeated items, keeping the first of each.
fn dedup_in_order<T: PartialEq>(items: &mut Vec<T>) {
    let mut kept: Vec<T> = Vec::with_capacity(items.len());
    for item in items.drain(..) {
        if !kept.contains(&item) {
            kept.push(item);
        }
    }
    *items = kept;
}
