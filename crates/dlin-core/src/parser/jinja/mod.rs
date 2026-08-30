use std::collections::HashMap;

use minijinja::Environment;

use super::sql::{RefCall, SourceCall, SqlConfig};

mod render;
pub(crate) mod source;
#[cfg(test)]
mod tests;

/// All extracted information from rendering a dbt Jinja SQL template
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct JinjaExtraction {
    pub refs: Vec<RefCall>,
    pub sources: Vec<SourceCall>,
    pub config: SqlConfig,
}

/// Result of a Jinja-based extraction attempt.
///
/// minijinja evaluates function arguments before resolving the callee, so even
/// when rendering fails (e.g. on an unknown macro) the refs/sources recorded up
/// to the failure point are valid. `complete` tells the caller whether rendering
/// finished, while `semantic_certain` tells it whether placeholder runtime
/// values could have selected the wrong branch. A complete but uncertain
/// render is recovered from the model-level template and only the local macro
/// scopes recorded below when no model-level uncertainty was observed; an
/// incomplete render uses whole-model recovery because execution provenance
/// may be truncated at the failure.
#[derive(Debug, Clone, Default)]
pub struct JinjaOutcome {
    pub extraction: JinjaExtraction,
    pub complete: bool,
    /// Whether the extraction is semantically certain rather than merely
    /// successfully rendered with the placeholder dbt environment.
    pub(crate) semantic_certain: bool,
    /// Whether uncertainty was observed in model-level execution (outside a
    /// local macro). This requires whole-model recovery even when a macro
    /// scope was also marked uncertain.
    pub(crate) model_uncertain: bool,
    /// Local model macro scopes in which an uncertainty callback executed.
    /// Used to limit regex recovery for complete renders.
    pub(crate) uncertain_macro_scopes: Vec<String>,
}

/// Try to extract refs, sources, and config from SQL content using minijinja.
/// Renders twice (with `is_incremental()` returning both false and true) and
/// merges results to capture refs/sources from all conditional branches.
///
/// `macro_prefix` is the pre-built concatenation of valid macro SQL files.
/// It is prepended to the template so that custom macros containing
/// ref()/source() calls are expanded and tracked.
pub fn extract_via_jinja(sql: &str, macro_prefix: &str) -> JinjaOutcome {
    extract_via_jinja_with_vars(sql, macro_prefix, &HashMap::new())
}

/// Like [`extract_via_jinja`] but resolves `var()` calls using the given
/// project-level variables (parsed from `dbt_project.yml`).
pub fn extract_via_jinja_with_vars(
    sql: &str,
    macro_prefix: &str,
    vars: &HashMap<String, serde_json::Value>,
) -> JinjaOutcome {
    render::render_with_incremental(sql, macro_prefix, vars)
}

/// Build a macro prefix string from individual macro sources, skipping
/// any that fail to parse as valid minijinja templates. This ensures one
/// bad macro file doesn't disable jinja-based extraction for all models.
pub fn build_macro_prefix(macro_sources: &[String]) -> String {
    if macro_sources.is_empty() {
        return String::new();
    }
    let env = Environment::new();
    let mut prefix = String::new();
    for source in macro_sources {
        // Only include macros that minijinja can parse individually
        if env.template_from_str(source).is_err() {
            continue;
        }
        // Verify the accumulated prefix still parses after adding this macro
        let len = prefix.len();
        prefix.push_str(source);
        prefix.push('\n');
        if env.template_from_str(&prefix).is_err() {
            prefix.truncate(len);
        }
    }
    prefix
}

/// Merge `other` into `base`, adding only deduplicated refs and sources
pub(super) fn merge_extraction(base: &mut JinjaExtraction, other: JinjaExtraction) {
    for r in other.refs {
        if !base.refs.contains(&r) {
            base.refs.push(r);
        }
    }
    for s in other.sources {
        if !base.sources.contains(&s) {
            base.sources.push(s);
        }
    }
    // config from the first render takes precedence; fill in fields the
    // first render did not produce (e.g. it failed before reaching config())
    if base.config.materialized.is_none() {
        base.config.materialized = other.config.materialized;
    }
    if base.config.tags.is_empty() {
        base.config.tags = other.config.tags;
    }
}
