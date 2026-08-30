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
    let outcome = super::jinja::extract_via_jinja_with_vars(sql, macro_prefix, vars);
    if outcome.complete && outcome.semantic_certain {
        return outcome.extraction;
    }
    let mut ext = outcome.extraction;
    let scoped_macro_names: Option<HashSet<String>> = if outcome.complete
        && !outcome.model_uncertain
        && !outcome.uncertain_macro_scopes.is_empty()
    {
        Some(outcome.uncertain_macro_scopes.iter().cloned().collect())
    } else {
        None
    };
    super::jinja::merge_extraction(
        &mut ext,
        super::jinja::JinjaExtraction {
            refs: recovery::extract_refs_regex_scoped(sql, scoped_macro_names.as_ref()),
            sources: recovery::extract_sources_regex_scoped(sql, scoped_macro_names.as_ref()),
            config: recovery::extract_config_regex(sql),
        },
    );
    ext
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
