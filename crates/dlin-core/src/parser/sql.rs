use regex::Regex;
use std::sync::LazyLock;

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

static JINJA_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{#[\s\S]*?#\}").unwrap());

// Matches ref('name'), ref("name"), ref('pkg', 'name'), ref("pkg", "name"),
// ref('name', version=N), ref('name', v=N), and the pkg variants.
// Both `version=` and `v=` are accepted per dbt-core v2.
// Version values may be bare integers (version=2) or quoted strings (version='alpha').
// Handles {{ ref(...) }} and {{- ref(...) -}} whitespace control.
// Capture groups:
//   1, 2 → pkg, name      (two-positional-arg form)
//   3    → version        (optional version=/v= kwarg in two-arg form)
//   4, 5 → name, version  (single-arg + version=/v= kwarg form)
//   6    → name           (single-arg form)
static REF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \{\{-?\s*
        ref\s*\(\s*
        (?:
            # Two-argument form: ref('pkg', 'name') or ref('pkg', 'name', version=N) or ref('pkg', 'name', v=N)
            (?:['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]\s*(?:,\s*(?:version|v)\s*=\s*(-?\d+|'[^']*'|"[^"]*"))?)
            |
            # Single-arg + version kwarg: ref('name', version=N) or ref('name', v=N)
            (?:['"]([^'"]+)['"]\s*,\s*(?:version|v)\s*=\s*(-?\d+|'[^']*'|"[^"]*"))
            |
            # Single-argument form: ref('name') or ref("name")
            ['"]([^'"]+)['"]
        )
        \s*\)\s*
        -?\}\}
    "#,
    )
    .unwrap()
});

// Matches source('src_name', 'table_name')
static SOURCE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \{\{-?\s*
        source\s*\(\s*
        ['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]
        \s*\)\s*
        -?\}\}
    "#,
    )
    .unwrap()
});

/// Strip Jinja comments from SQL content
fn strip_jinja_comments(sql: &str) -> String {
    JINJA_COMMENT.replace_all(sql, "").to_string()
}

/// Extract all refs, sources, and config from SQL content in a single pass.
/// Tries minijinja rendering first; falls back to regex on failure.
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
    if let Some(ext) = super::jinja::extract_via_jinja_with_vars(sql, macro_prefix, vars) {
        return ext;
    }
    super::jinja::JinjaExtraction {
        refs: extract_refs_regex(sql),
        sources: extract_sources_regex(sql),
        config: extract_config_regex(sql),
    }
}

/// Extract all ref() and source() calls from SQL content in a single pass.
/// Tries minijinja rendering first; falls back to regex on failure.
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
    if let Some(ext) = super::jinja::extract_via_jinja_with_vars(sql, macro_prefix, vars) {
        return (ext.refs, ext.sources);
    }
    (extract_refs_regex(sql), extract_sources_regex(sql))
}

/// Extract all ref() calls from SQL content.
pub fn extract_refs(sql: &str) -> Vec<RefCall> {
    extract_refs_and_sources(sql, "").0
}

/// Extract all source() calls from SQL content.
pub fn extract_sources(sql: &str) -> Vec<SourceCall> {
    extract_refs_and_sources(sql, "").1
}

/// Strip surrounding single or double quotes from a version kwarg capture.
/// Bare integers are returned unchanged; quoted strings have their delimiters removed.
fn strip_version_quotes(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('\'') && s.ends_with('\'')) || (s.starts_with('"') && s.ends_with('"')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
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

/// Regex fallback for extracting ref() calls
fn extract_refs_regex(sql: &str) -> Vec<RefCall> {
    let cleaned = strip_jinja_comments(sql);
    let mut refs = Vec::new();

    for cap in REF_PATTERN.captures_iter(&cleaned) {
        if let (Some(pkg), Some(name)) = (cap.get(1), cap.get(2)) {
            // Two-positional-arg form: ref('pkg', 'name') or ref('pkg', 'name', version=N)
            refs.push(RefCall {
                package: Some(pkg.as_str().to_string()),
                name: name.as_str().to_string(),
                version: cap
                    .get(3)
                    .map(|v| normalize_version_str(&strip_version_quotes(v.as_str()))),
            });
        } else if let (Some(name), Some(ver)) = (cap.get(4), cap.get(5)) {
            // Single-arg + version kwarg: ref('name', version=N) or ref('name', version='str')
            refs.push(RefCall {
                package: None,
                name: name.as_str().to_string(),
                version: Some(normalize_version_str(&strip_version_quotes(ver.as_str()))),
            });
        } else if let Some(name) = cap.get(6) {
            // Single-arg form: ref('name')
            refs.push(RefCall {
                package: None,
                name: name.as_str().to_string(),
                version: None,
            });
        }
    }

    refs
}

/// Regex fallback for extracting source() calls
fn extract_sources_regex(sql: &str) -> Vec<SourceCall> {
    let cleaned = strip_jinja_comments(sql);
    let mut sources = Vec::new();

    for cap in SOURCE_PATTERN.captures_iter(&cleaned) {
        sources.push(SourceCall {
            source_name: cap[1].to_string(),
            table_name: cap[2].to_string(),
        });
    }

    sources
}

/// Parsed config block from SQL
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SqlConfig {
    pub materialized: Option<String>,
    pub tags: Vec<String>,
}

// Matches {{ config(...) }} blocks — captures the inner arguments
static CONFIG_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \{\{-?\s*
        config\s*\(
        ([\s\S]*?)
        \)\s*
        -?\}\}
    "#,
    )
    .unwrap()
});

// Matches materialized='value' or materialized="value"
static MATERIALIZED_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"materialized\s*=\s*['"]([^'"]+)['"]"#).unwrap());

// Matches tags=['a', 'b'] or tags=["a", "b"]
static TAGS_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"tags\s*=\s*\[([^\]]*)\]"#).unwrap());

// Matches individual tag values inside the tags list
static TAG_VALUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"]([^'"]+)['"]"#).unwrap());

/// Extract config() block settings from SQL content.
/// Tries minijinja rendering first; falls back to regex on failure.
pub fn extract_config(sql: &str, macro_prefix: &str) -> SqlConfig {
    if let Some(ext) = super::jinja::extract_via_jinja(sql, macro_prefix) {
        return ext.config;
    }
    extract_config_regex(sql)
}

/// Regex fallback for extracting config() settings
fn extract_config_regex(sql: &str) -> SqlConfig {
    let cleaned = strip_jinja_comments(sql);
    let mut config = SqlConfig::default();

    if let Some(cap) = CONFIG_PATTERN.captures(&cleaned) {
        let inner = &cap[1];

        if let Some(mat) = MATERIALIZED_PATTERN.captures(inner) {
            config.materialized = Some(mat[1].to_string());
        }

        if let Some(tags_cap) = TAGS_PATTERN.captures(inner) {
            let tags_inner = &tags_cap[1];
            config.tags = TAG_VALUE
                .captures_iter(tags_inner)
                .map(|c| c[1].to_string())
                .collect();
        }
    }

    config
}

#[cfg(test)]
mod tests {
    use super::*;
    // Access private fallback directly so the regex path is covered independently
    // of the Jinja extractor (which normally runs first in extract_refs).
    use super::extract_refs_regex;

    #[test]
    fn test_single_ref() {
        let sql = "SELECT * FROM {{ ref('stg_orders') }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "stg_orders");
        assert!(refs[0].package.is_none());
    }

    #[test]
    fn test_double_quoted_ref() {
        let sql = r#"SELECT * FROM {{ ref("stg_orders") }}"#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "stg_orders");
    }

    #[test]
    fn test_two_arg_ref() {
        let sql = "SELECT * FROM {{ ref('other_project', 'stg_orders') }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package.as_deref(), Some("other_project"));
        assert_eq!(refs[0].name, "stg_orders");
    }

    #[test]
    fn test_whitespace_control() {
        let sql = "SELECT * FROM {{- ref('stg_orders') -}}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "stg_orders");
    }

    #[test]
    fn test_multiple_refs() {
        let sql = r#"
            SELECT
                o.*,
                c.name
            FROM {{ ref('stg_orders') }} o
            JOIN {{ ref('stg_customers') }} c ON o.customer_id = c.id
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].name, "stg_orders");
        assert_eq!(refs[1].name, "stg_customers");
    }

    #[test]
    fn test_source() {
        let sql = "SELECT * FROM {{ source('raw', 'orders') }}";
        let sources = extract_sources(sql);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_name, "raw");
        assert_eq!(sources[0].table_name, "orders");
    }

    #[test]
    fn test_source_whitespace_control() {
        let sql = "SELECT * FROM {{- source('raw', 'orders') -}}";
        let sources = extract_sources(sql);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_name, "raw");
    }

    #[test]
    fn test_strip_jinja_comments() {
        let sql = r#"
            {# This is a comment with {{ ref('should_be_ignored') }} #}
            SELECT * FROM {{ ref('actual_model') }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "actual_model");
    }

    #[test]
    fn test_mixed_refs_and_sources() {
        let sql = r#"
            SELECT *
            FROM {{ source('raw', 'orders') }}
            JOIN {{ ref('stg_customers') }} ON 1=1
        "#;
        let refs = extract_refs(sql);
        let sources = extract_sources(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(sources.len(), 1);
    }

    #[test]
    fn test_no_refs() {
        let sql = "SELECT 1 as id";
        let refs = extract_refs(sql);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extra_spaces() {
        let sql = "SELECT * FROM {{  ref(  'stg_orders'  )  }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "stg_orders");
    }

    #[test]
    fn test_ref_with_version_kwarg() {
        let sql = "SELECT * FROM {{ ref('my_model', version=2) }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("2"));
        assert!(refs[0].package.is_none());
    }

    #[test]
    fn test_ref_with_version_kwarg_spaced() {
        let sql = "SELECT * FROM {{ ref('my_model', version = 3) }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("3"));
    }

    #[test]
    fn test_ref_without_version_has_none() {
        let sql = "SELECT * FROM {{ ref('my_model') }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].version, None);
    }

    #[test]
    fn test_ref_two_arg_has_no_version() {
        let sql = "SELECT * FROM {{ ref('pkg', 'my_model') }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package.as_deref(), Some("pkg"));
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version, None);
    }

    #[test]
    fn test_version_does_not_conflict_with_two_arg_form() {
        // ref('pkg', 'name') must NOT match the version=N branch
        let sql = "SELECT * FROM {{ ref('mypkg', 'model_a') }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
        assert_eq!(refs[0].name, "model_a");
        assert_eq!(refs[0].version, None);
    }

    #[test]
    fn test_two_arg_ref_with_version_kwarg() {
        let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', version=3) }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("3"));
    }

    #[test]
    fn test_ref_with_v_shorthand_kwarg() {
        let sql = "SELECT * FROM {{ ref('my_model', v=2) }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("2"));
        assert!(refs[0].package.is_none());
    }

    #[test]
    fn test_two_arg_ref_with_v_shorthand_kwarg() {
        let sql = "SELECT * FROM {{ ref('mypkg', 'my_model', v=3) }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("3"));
    }

    #[test]
    fn test_ref_with_string_version_kwarg() {
        // dbt-core accepts version='alpha' for non-integer version strings
        let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='alpha') }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("alpha"));
    }

    #[test]
    fn test_ref_with_quoted_integer_version_kwarg() {
        // version='2' (quoted) must resolve identically to version=2 (bare integer)
        let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='2') }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].version.as_deref(), Some("2"));
    }

    #[test]
    fn test_ref_with_padded_integer_version_kwarg() {
        // version='02' must normalize to "2" to match YAML v: 2 → version_value_to_str → "2"
        let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='02') }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].version.as_deref(), Some("2"));
    }

    #[test]
    fn test_ref_with_decimal_version_kwarg() {
        // version='2.0' stays as "2.0" — matching YAML `v: "2.0"` which also keeps "2.0".
        // Both use i64-only normalization so non-integer numeric strings are not rewritten.
        let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', version='2.0') }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].version.as_deref(), Some("2.0"));
    }

    // These tests call the regex fallback directly to confirm `v=` support
    // in that path, independent of the Jinja extractor.

    #[test]
    fn test_regex_fallback_v_shorthand_kwarg() {
        let refs = extract_refs_regex("SELECT * FROM {{ ref('my_model', v=2) }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("2"));
        assert!(refs[0].package.is_none());
    }

    #[test]
    fn test_regex_fallback_two_arg_v_shorthand_kwarg() {
        let refs = extract_refs_regex("SELECT * FROM {{ ref('mypkg', 'my_model', v=3) }}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].package.as_deref(), Some("mypkg"));
        assert_eq!(refs[0].name, "my_model");
        assert_eq!(refs[0].version.as_deref(), Some("3"));
    }

    // ─── Config extraction tests ───

    #[test]
    fn test_config_materialized() {
        let sql = "{{ config(materialized='incremental') }}\nSELECT 1";
        let config = extract_config(sql, "");
        assert_eq!(config.materialized.as_deref(), Some("incremental"));
        assert!(config.tags.is_empty());
    }

    #[test]
    fn test_config_materialized_double_quotes() {
        let sql = r#"{{ config(materialized="table") }}"#;
        let config = extract_config(sql, "");
        assert_eq!(config.materialized.as_deref(), Some("table"));
    }

    #[test]
    fn test_config_tags() {
        let sql = "{{ config(tags=['nightly', 'finance']) }}\nSELECT 1";
        let config = extract_config(sql, "");
        assert_eq!(config.tags, vec!["nightly", "finance"]);
    }

    #[test]
    fn test_config_both() {
        let sql = "{{ config(materialized='view', tags=['daily']) }}\nSELECT 1";
        let config = extract_config(sql, "");
        assert_eq!(config.materialized.as_deref(), Some("view"));
        assert_eq!(config.tags, vec!["daily"]);
    }

    #[test]
    fn test_config_whitespace_control() {
        let sql = "{{- config(materialized='ephemeral') -}}\nSELECT 1";
        let config = extract_config(sql, "");
        assert_eq!(config.materialized.as_deref(), Some("ephemeral"));
    }

    #[test]
    fn test_config_multiline() {
        let sql = r#"{{
            config(
                materialized='incremental',
                tags=['nightly', 'warehouse']
            )
        }}
        SELECT 1"#;
        let config = extract_config(sql, "");
        assert_eq!(config.materialized.as_deref(), Some("incremental"));
        assert_eq!(config.tags, vec!["nightly", "warehouse"]);
    }

    #[test]
    fn test_no_config() {
        let sql = "SELECT * FROM {{ ref('orders') }}";
        let config = extract_config(sql, "");
        assert!(config.materialized.is_none());
        assert!(config.tags.is_empty());
    }

    #[test]
    fn test_config_in_comment_ignored() {
        let sql = r#"
            {# {{ config(materialized='table') }} #}
            SELECT 1
        "#;
        let config = extract_config(sql, "");
        assert!(config.materialized.is_none());
    }
}
