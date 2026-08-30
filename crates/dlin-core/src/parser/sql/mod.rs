use std::collections::HashSet;

mod recovery;

/// A reference to another dbt model via ref()
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RefCall {
    /// Optional package name (for cross-project refs)
    pub package: Option<String>,
    /// Model name
    pub name: String,
    /// Version from ref('name', version=N) or ref('name', version='alpha').
    /// Stored as a string to support both integer and non-integer versions.
    #[serde(default)]
    pub version: Option<String>,
}

/// A reference to a dbt source via source()
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SourceCall {
    /// Source name
    pub source_name: String,
    /// Table name within the source
    pub table_name: String,
}

/// Extract all refs, sources, and config from SQL content in a single pass.
/// Tries minijinja rendering first; if rendering fails partway (e.g. on an
/// unknown macro), or if placeholder values make the result semantically
/// uncertain, keeps whatever it recorded and merges in the regex scan.
///
/// `macro_prefix` is the pre-built concatenation of valid macro SQL files
/// so that custom macros containing ref()/source() are expanded and tracked.
pub fn extract_all(sql: &str, macro_prefix: &str) -> super::jinja::JinjaExtraction {
    extract_all_with_vars(sql, macro_prefix, &std::collections::HashMap::new())
}

/// Like [`extract_all`] but resolves `var()` calls using project-level variables.
pub fn extract_all_with_vars(
    sql: &str,
    macro_prefix: &str,
    vars: &std::collections::HashMap<String, serde_json::Value>,
) -> super::jinja::JinjaExtraction {
    let prepared = super::jinja::reachability::PreparedMacroPrefix::new(macro_prefix);
    extract_all_with_prepared_prefix(sql, &prepared, vars)
}

pub(crate) fn extract_all_with_prepared_prefix(
    sql: &str,
    macro_prefix: &super::jinja::reachability::PreparedMacroPrefix,
    vars: &std::collections::HashMap<String, serde_json::Value>,
) -> super::jinja::JinjaExtraction {
    let outcome = super::jinja::extract_via_jinja_with_prepared_prefix(sql, macro_prefix, vars);
    if outcome.complete && outcome.semantic_certain {
        return outcome.extraction;
    }
    let fallback_scopes = regex_fallback_scopes(sql, macro_prefix, &outcome);
    let mut ext = outcome.extraction;
    let (model_scopes, model_spans, prefix_scopes, prefix_spans) = fallback_scopes
        .map(|scopes| {
            (
                scopes.model,
                scopes.model_spans,
                scopes.prefix,
                scopes.prefix_spans,
            )
        })
        .unwrap_or((None, None, None, None));
    let (refs, sources) = model_spans.map_or_else(
        || recovery::extract_refs_and_sources_regex_scoped(sql, model_scopes.as_ref()),
        |spans| {
            let all_spans = outcome.local_macro_spans.as_slice();
            let model_source =
                super::jinja::source::strip_macro_definitions_for_runtime_analysis(sql, all_spans);
            let (mut refs, mut sources) =
                recovery::extract_refs_and_sources_regex_scoped(&model_source, None);
            let (local_refs, local_sources) =
                recovery::extract_refs_and_sources_regex_spans(sql, &spans);
            refs.extend(local_refs);
            sources.extend(local_sources);
            (refs, sources)
        },
    );
    super::jinja::merge_extraction(
        &mut ext,
        super::jinja::JinjaExtraction {
            refs,
            sources,
            config: recovery::extract_config_regex(sql),
        },
    );
    let (prefix_refs, prefix_sources) = prefix_spans.as_ref().map_or_else(
        || {
            recovery::extract_refs_and_sources_regex_scoped(
                macro_prefix.source(),
                prefix_scopes.as_ref(),
            )
        },
        |spans| recovery::extract_refs_and_sources_regex_spans(macro_prefix.source(), spans),
    );
    super::jinja::merge_extraction(
        &mut ext,
        super::jinja::JinjaExtraction {
            refs: prefix_refs,
            sources: prefix_sources,
            config: SqlConfig::default(),
        },
    );
    ext
}

struct RegexFallbackScopes {
    model: Option<HashSet<String>>,
    /// Selected local definition spans for partial model recovery. `None`
    /// means whole-model scan; `Some(empty)` means only top-level model SQL.
    model_spans: Option<Vec<super::jinja::source::ModelMacroSpan>>,
    prefix: Option<HashSet<String>>,
    /// Selected prefix definition spans for scoped recovery. `None` means a
    /// conservative whole-prefix scan; `Some(empty)` means scan no prefix.
    prefix_spans: Option<Vec<super::jinja::source::ModelMacroSpan>>,
}

fn regex_fallback_scopes(
    sql: &str,
    macro_prefix: &super::jinja::reachability::PreparedMacroPrefix,
    outcome: &super::jinja::JinjaOutcome,
) -> Option<RegexFallbackScopes> {
    if !outcome.complete {
        // A failed render has incomplete execution provenance: an unvisited
        // prefix macro may still have been reachable before the failure.
        // Whole-prefix recovery is therefore intentionally conservative.
        return None;
    }
    if outcome.model_uncertain {
        // Model-level uncertainty can select a local macro that did not
        // execute in the placeholder render. Build the symbol graph only
        // on this path; complete certain and macro-local-only renders do
        // not pay for another MiniJinja compilation pass.
        let plan = if outcome.macro_reachability_unknown {
            None
        } else {
            outcome.macro_reachability.clone().or_else(|| {
                super::jinja::reachability::macro_reachability_with_prepared_prefix(
                    sql,
                    macro_prefix,
                    outcome
                        .local_macro_spans_scanned
                        .then_some(outcome.local_macro_spans.as_slice()),
                    outcome.model_macro_roots.as_ref(),
                )
            })
        };
        return match plan {
            Some(plan) => {
                let mut scopes = plan.local_scopes;
                scopes.extend(outcome.uncertain_macro_scopes.iter().cloned());
                // A scoped scan of every local macro has the same result as
                // the conservative whole-model scan, but avoids an owner
                // lookup for every Jinja block. Keep zero macros scoped-empty
                // so model-level recovery remains isolated from definitions.
                let model = if plan.local_macro_count > 0 && scopes.len() == plan.local_macro_count
                {
                    None
                } else {
                    Some(scopes)
                };
                let model_spans = if model.is_none() {
                    None
                } else {
                    Some(plan.local_definition_spans)
                };
                let prefix = if plan.prefix_macro_count > 0
                    && plan.prefix_scopes.len() == plan.prefix_macro_count
                {
                    None
                } else {
                    Some(plan.prefix_scopes)
                };
                let prefix_spans = if prefix.is_none() {
                    None
                } else {
                    Some(plan.prefix_definition_spans)
                };
                Some(RegexFallbackScopes {
                    model,
                    model_spans,
                    prefix,
                    prefix_spans,
                })
            }
            None => None,
        };
    }
    if !outcome.uncertain_macro_scopes.is_empty() {
        // A runtime callback inside a local macro can call a project macro
        // that was not entered by the placeholder render. Treat each
        // uncertain local scope as a graph root so recovery follows those
        // prefix dependencies without scanning unrelated project macros.
        let uncertain_roots = outcome.uncertain_macro_scopes.iter().cloned().collect();
        let plan = super::jinja::reachability::macro_reachability_with_prepared_prefix(
            sql,
            macro_prefix,
            outcome
                .local_macro_spans_scanned
                .then_some(outcome.local_macro_spans.as_slice()),
            Some(&uncertain_roots),
        );
        match plan {
            Some(plan) => {
                let prefix = if plan.prefix_macro_count > 0
                    && plan.prefix_scopes.len() == plan.prefix_macro_count
                {
                    None
                } else {
                    Some(plan.prefix_scopes)
                };
                let prefix_spans = if prefix.is_none() {
                    None
                } else {
                    Some(plan.prefix_definition_spans)
                };
                Some(RegexFallbackScopes {
                    model: Some(plan.local_scopes),
                    model_spans: Some(plan.local_definition_spans),
                    prefix,
                    prefix_spans,
                })
            }
            None => Some(RegexFallbackScopes {
                model: Some(uncertain_roots),
                model_spans: None,
                // A failed reachability analysis cannot establish which
                // project macro definitions were reachable. Recover the
                // whole prefix rather than risking a false negative.
                prefix: None,
                prefix_spans: None,
            }),
        }
    } else {
        None
    }
}

#[cfg(test)]
fn regex_fallback_macro_scopes(
    sql: &str,
    macro_prefix: &str,
    outcome: &super::jinja::JinjaOutcome,
) -> Option<HashSet<String>> {
    let prepared = super::jinja::reachability::PreparedMacroPrefix::new(macro_prefix);
    regex_fallback_scopes(sql, &prepared, outcome).and_then(|scopes| scopes.model)
}

/// Extract all ref() and source() calls from SQL content in a single pass.
/// Tries minijinja rendering first; if rendering fails partway or relies on a
/// placeholder runtime value, merges the partial result with the regex scan.
/// Complete uncertain renders restrict that merge to model-level text and
/// macro scopes whose execution actually observed an uncertain value; failed
/// renders retain whole-model recovery because their execution provenance is
/// incomplete.
///
/// `macro_prefix` is the pre-built concatenation of valid macro SQL files
/// so that custom macros containing ref()/source() are expanded and tracked.
pub fn extract_refs_and_sources(sql: &str, macro_prefix: &str) -> (Vec<RefCall>, Vec<SourceCall>) {
    extract_refs_and_sources_with_vars(sql, macro_prefix, &std::collections::HashMap::new())
}

/// Like [`extract_refs_and_sources`] but resolves `var()` calls using project-level variables.
pub fn extract_refs_and_sources_with_vars(
    sql: &str,
    macro_prefix: &str,
    vars: &std::collections::HashMap<String, serde_json::Value>,
) -> (Vec<RefCall>, Vec<SourceCall>) {
    let ext = extract_all_with_vars(sql, macro_prefix, vars);
    (ext.refs, ext.sources)
}

pub(crate) fn extract_refs_and_sources_with_prepared_prefix(
    sql: &str,
    macro_prefix: &super::jinja::reachability::PreparedMacroPrefix,
    vars: &std::collections::HashMap<String, serde_json::Value>,
) -> (Vec<RefCall>, Vec<SourceCall>) {
    let ext = extract_all_with_prepared_prefix(sql, macro_prefix, vars);
    (ext.refs, ext.sources)
}

/// Extract all ref() calls from SQL content.
pub fn extract_refs(sql: &str) -> Vec<RefCall> {
    extract_refs_and_sources(sql, "").0
}

/// Extract all source() calls from SQL content.
pub fn extract_sources(sql: &str) -> Vec<SourceCall> {
    extract_refs_and_sources(sql, "").1
}

/// Normalize a version string to a canonical form, matching the normalization
/// applied to YAML string version values in version_value_to_str().
/// Integer strings (including zero-padded) are normalized: "02" → "2".
/// Non-integer strings (including "2.0") are returned as-is.
/// Using i64 only (no f64 fallback) keeps this consistent with the YAML string
/// path so that ref(version='2.0') resolves to the same ID as `v: "2.0"`.
pub(super) fn normalize_version_str(s: &str) -> String {
    if let Ok(n) = s.parse::<i64>() {
        return n.to_string();
    }
    s.to_string()
}

#[cfg(test)]
fn extract_refs_regex(sql: &str) -> Vec<RefCall> {
    recovery::extract_refs_regex(sql)
}

#[cfg(test)]
fn extract_sources_regex(sql: &str) -> Vec<SourceCall> {
    recovery::extract_sources_regex(sql)
}

/// Parsed config block from SQL
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SqlConfig {
    pub materialized: Option<String>,
    pub tags: Vec<String>,
}

/// Extract config() block settings from SQL content.
/// Tries minijinja rendering first; falls back to regex on failure.
pub fn extract_config(sql: &str, macro_prefix: &str) -> SqlConfig {
    extract_all(sql, macro_prefix).config
}

#[cfg(test)]
mod tests;
