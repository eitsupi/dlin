use regex::Regex;
use std::collections::HashSet;
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

// Matches {% raw %}...{% endraw %} sections, whose content jinja treats as literal text
static JINJA_RAW_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{%[+-]?\s*raw\s*[+-]?%\}[\s\S]*?\{%[+-]?\s*endraw\s*[+-]?%\}").unwrap()
});

// Matches a jinja expression block {{ ... }} or statement block {% ... %}.
// ref()/source() calls are only meaningful inside these blocks; scanning
// block contents (rather than the whole file) avoids false positives from
// e.g. SQL comments that mention ref('...') outside any jinja block.
static JINJA_BLOCK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{[\s\S]*?\}\}|\{%[\s\S]*?%\}").unwrap());

// Matches ref('name'), ref("name"), ref('pkg', 'name'), ref("pkg", "name"),
// ref('name', version=N), ref('name', v=N), and the pkg variants.
// Both `version=` and `v=` are accepted per dbt-core v2.
// Version values may be bare integers (version=2) or quoted strings (version='alpha').
// Applied to the contents of jinja blocks, so the call may appear anywhere a
// jinja expression can: as the whole block, as a macro argument, inside
// {% set %}, etc.
// Capture groups:
//   1, 2 → pkg, name      (two-positional-arg form)
//   3    → version        (optional version=/v= kwarg in two-arg form)
//   4, 5 → name, version  (single-arg + version=/v= kwarg form)
//   6    → name           (single-arg form)
static REF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \bref\s*\(\s*
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
        \s*\)
    "#,
    )
    .unwrap()
});

// Matches source('src_name', 'table_name') anywhere inside a jinja block
static SOURCE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?x)
        \bsource\s*\(\s*
        ['"]([^'"]+)['"]\s*,\s*['"]([^'"]+)['"]
        \s*\)
    "#,
    )
    .unwrap()
});

/// Strip Jinja comments from SQL content
fn strip_jinja_comments(sql: &str) -> String {
    JINJA_COMMENT.replace_all(sql, "").to_string()
}

/// Strip jinja constructs whose content is never evaluated ({# #} comments
/// and {% raw %} sections), leaving only renderable template text.
fn strip_inert_jinja(sql: &str) -> String {
    let no_comments = strip_jinja_comments(sql);
    JINJA_RAW_BLOCK.replace_all(&no_comments, "").to_string()
}

/// Remove the supplied model-local macro spans, then remove inert Jinja
/// constructs. Keeping span discovery separate lets callers reuse one scan
/// for both direct model and macro-local runtime analysis.
pub(super) fn strip_macro_definitions_for_runtime_analysis(
    sql: &str,
    definitions: &[ModelMacroSpan],
) -> String {
    if definitions.is_empty() {
        return strip_inert_jinja(sql);
    }

    let mut result = String::with_capacity(sql.len());
    let mut cursor = 0;
    for definition in definitions {
        result.push_str(&sql[cursor..definition.start]);
        cursor = definition.end;
    }
    result.push_str(&sql[cursor..]);
    strip_inert_jinja(&result)
}

/// Return source spans for model-local macro definitions. This scanner skips
/// Jinja comments, raw blocks, quoted terminators, and nested statement blocks
/// so a source transformation cannot modify inert template text.
/// The offsets are byte positions in the original source.
#[derive(Debug, Clone)]
pub(super) struct ModelMacroSpan {
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) opening_end: usize,
    pub(super) closing_start: usize,
    pub(super) name: String,
}

pub(super) fn model_macro_definition_spans(sql: &str) -> Vec<ModelMacroSpan> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some((start, end, kind, content)) = next_jinja_tag(sql, cursor) {
        cursor = end;
        if kind != JinjaTagKind::Statement {
            continue;
        }
        let Some(name) = jinja_tag_name(content) else {
            continue;
        };
        if name == "raw" {
            cursor = find_raw_end(sql, end).unwrap_or(sql.len());
            continue;
        }
        if name != "macro" {
            continue;
        }
        let Some((closing_start, definition_end)) = find_macro_end(sql, end) else {
            // Do not guess at the extent of an incomplete definition. A
            // caller can still render the original source and retain the
            // ordinary completion/fallback behavior.
            return Vec::new();
        };
        let Some(macro_name) = macro_tag_name(content) else {
            continue;
        };
        result.push(ModelMacroSpan {
            start,
            end: definition_end,
            opening_end: end,
            closing_start,
            name: macro_name.to_owned(),
        });
        cursor = definition_end;
    }
    result
}

/// Inject an empty-output marker immediately inside selected macro definitions.
/// The caller determines which definitions have runtime-dependent free
/// variables; this helper only applies the source transformation at validated
/// Jinja tag boundaries.
pub(super) fn inject_macro_runtime_markers(
    sql: &str,
    definitions: &[ModelMacroSpan],
    scalar_macro_names: &HashSet<String>,
    enter_marker: &str,
    exit_marker: &str,
) -> String {
    if definitions.is_empty() {
        return sql.to_owned();
    }
    let mut result = String::with_capacity(sql.len() + definitions.len() * enter_marker.len() * 2);
    let mut cursor = 0;
    for definition in definitions {
        if definition.end > sql.len() || definition.start < cursor {
            return sql.to_owned();
        }
        // A `-%}` opening tag trims all whitespace immediately following the
        // tag. Put the marker after that original whitespace so introducing
        // the marker cannot make the trim stop at the marker expression.
        let opening_insertion =
            if definition.opening_end >= 3 && sql.as_bytes()[definition.opening_end - 3] == b'-' {
                let mut end = definition.opening_end;
                while end < sql.len() && sql.as_bytes()[end].is_ascii_whitespace() {
                    end += 1;
                }
                end
            } else {
                definition.opening_end
            };
        // A `{%- endmacro` tag trims whitespace immediately before itself.
        // Put the exit marker before that whitespace so the original trim
        // still sees the same whitespace run.
        let closing_insertion = if sql.as_bytes().get(definition.closing_start + 2) == Some(&b'-') {
            let mut start = definition.closing_start;
            while start > opening_insertion && sql.as_bytes()[start - 1].is_ascii_whitespace() {
                start -= 1;
            }
            start
        } else {
            definition.closing_start
        };
        if opening_insertion > closing_insertion || opening_insertion < cursor {
            return sql.to_owned();
        }
        result.push_str(&sql[cursor..opening_insertion]);
        result.push_str("{{ ");
        result.push_str(enter_marker);
        result.push_str("(\"");
        result.push_str(&definition.name);
        result.push_str("\", ");
        result.push_str(if scalar_macro_names.contains(&definition.name) {
            "true"
        } else {
            "false"
        });
        result.push_str(") }}");
        result.push_str(&sql[opening_insertion..closing_insertion]);
        result.push_str("{{ ");
        result.push_str(exit_marker);
        result.push_str("(\"");
        result.push_str(&definition.name);
        result.push_str("\") }}");
        cursor = closing_insertion;
    }
    result.push_str(&sql[cursor..]);
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JinjaTagKind {
    Comment,
    Expression,
    Statement,
}

fn next_jinja_tag(source: &str, from: usize) -> Option<(usize, usize, JinjaTagKind, &str)> {
    let mut cursor = from;
    while let Some(relative) = source[cursor..].find("{") {
        let start = cursor + relative;
        let remainder = &source[start..];
        let (kind, close) = if remainder.starts_with("{#") {
            (JinjaTagKind::Comment, "#}")
        } else if remainder.starts_with("{{") {
            (JinjaTagKind::Expression, "}}")
        } else if remainder.starts_with("{%") {
            (JinjaTagKind::Statement, "%}")
        } else {
            cursor = start + 1;
            continue;
        };
        let end = if kind == JinjaTagKind::Comment {
            source[start + 2..]
                .find(close)
                .map(|offset| start + 2 + offset + 2)?
        } else {
            find_jinja_terminator(source, start + 2, close.as_bytes()[0], close.as_bytes()[1])?
        };
        let content_end = end - 2;
        return Some((start, end, kind, &source[start + 2..content_end]));
    }
    None
}

fn find_jinja_terminator(source: &str, from: usize, first: u8, second: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut cursor = from;
    let mut quote = None;
    while cursor + 1 < bytes.len() {
        let byte = bytes[cursor];
        if let Some(quote_byte) = quote {
            if byte == b'\\' {
                cursor += 2;
                continue;
            }
            if byte == quote_byte {
                quote = None;
            }
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if byte == first && bytes[cursor + 1] == second {
            return Some(cursor + 2);
        }
        cursor += 1;
    }
    None
}

fn jinja_tag_name(content: &str) -> Option<&str> {
    let content = content.trim().trim_start_matches(['-', '+']).trim();
    content.split_whitespace().next()
}

fn macro_tag_name(content: &str) -> Option<&str> {
    let content = content.trim().trim_start_matches(['-', '+']).trim();
    let rest = content.strip_prefix("macro")?.trim_start();
    let end = rest.find(|ch: char| ch == '(' || ch.is_whitespace())?;
    let name = &rest[..end];
    (!name.is_empty()).then_some(name)
}

fn find_raw_end(source: &str, from: usize) -> Option<usize> {
    let mut cursor = from;
    while let Some((_, end, kind, content)) = next_jinja_tag(source, cursor) {
        cursor = end;
        if kind == JinjaTagKind::Statement && jinja_tag_name(content) == Some("endraw") {
            return Some(end);
        }
    }
    None
}

fn find_macro_end(source: &str, from: usize) -> Option<(usize, usize)> {
    let mut cursor = from;
    let mut depth = 1;
    while let Some((start, end, kind, content)) = next_jinja_tag(source, cursor) {
        cursor = end;
        if kind != JinjaTagKind::Statement {
            continue;
        }
        match jinja_tag_name(content) {
            Some("raw") => cursor = find_raw_end(source, end).unwrap_or(source.len()),
            Some("macro") => depth += 1,
            Some("endmacro") => {
                depth -= 1;
                if depth == 0 {
                    return Some((start, end));
                }
            }
            _ => {}
        }
    }
    None
}

/// Byte spans of quoted string literals inside a jinja block, honoring
/// backslash escapes. Used to reject ref()/source() text that appears
/// inside a string literal (e.g. a log message) rather than as a call.
fn string_literal_spans(block: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut chars = block.char_indices();
    while let Some((start, c)) = chars.next() {
        if c != '\'' && c != '"' {
            continue;
        }
        let mut end = block.len();
        while let Some((i, d)) = chars.next() {
            if d == '\\' {
                chars.next();
            } else if d == c {
                end = i + d.len_utf8();
                break;
            }
        }
        spans.push((start, end));
    }
    spans
}

/// Whether `pos` falls strictly inside any of the given string literal spans
/// (a call whose own arguments are quoted starts at the identifier, which is
/// outside every span).
fn inside_string_literal(spans: &[(usize, usize)], pos: usize) -> bool {
    spans.iter().any(|&(start, end)| pos > start && pos < end)
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
            refs: extract_refs_regex_scoped(sql, scoped_macro_names.as_ref()),
            sources: extract_sources_regex_scoped(sql, scoped_macro_names.as_ref()),
            config: extract_config_regex(sql),
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

/// Regex fallback for extracting ref() calls.
/// Scans inside every jinja block so calls nested in macro arguments or
/// {% set %} statements are found too, mirroring dbt which registers a
/// ref() wherever it is evaluated.
#[cfg(test)]
fn extract_refs_regex(sql: &str) -> Vec<RefCall> {
    extract_refs_regex_scoped(sql, None)
}

fn extract_refs_regex_scoped(sql: &str, macro_scopes: Option<&HashSet<String>>) -> Vec<RefCall> {
    let cleaned = strip_inert_jinja(sql);
    let definitions = macro_scopes.map(|_| model_macro_definition_spans(&cleaned));
    let mut refs = Vec::new();

    for block in JINJA_BLOCK.find_iter(&cleaned) {
        if let Some(scopes) = macro_scopes {
            let owner = definitions.as_ref().and_then(|definitions| {
                definitions.iter().find(|definition| {
                    definition.start <= block.start() && block.start() < definition.end
                })
            });
            if owner.is_some_and(|definition| !scopes.contains(&definition.name)) {
                continue;
            }
        }
        let literal_spans = string_literal_spans(block.as_str());
        for cap in REF_PATTERN.captures_iter(block.as_str()) {
            if inside_string_literal(&literal_spans, cap.get(0).unwrap().start()) {
                continue;
            }
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
    }

    refs
}

/// Regex fallback for extracting source() calls.
/// Scans inside every jinja block, like [`extract_refs_regex`].
#[cfg(test)]
fn extract_sources_regex(sql: &str) -> Vec<SourceCall> {
    extract_sources_regex_scoped(sql, None)
}

fn extract_sources_regex_scoped(
    sql: &str,
    macro_scopes: Option<&HashSet<String>>,
) -> Vec<SourceCall> {
    let cleaned = strip_inert_jinja(sql);
    let definitions = macro_scopes.map(|_| model_macro_definition_spans(&cleaned));
    let mut sources = Vec::new();

    for block in JINJA_BLOCK.find_iter(&cleaned) {
        if let Some(scopes) = macro_scopes {
            let owner = definitions.as_ref().and_then(|definitions| {
                definitions.iter().find(|definition| {
                    definition.start <= block.start() && block.start() < definition.end
                })
            });
            if owner.is_some_and(|definition| !scopes.contains(&definition.name)) {
                continue;
            }
        }
        let literal_spans = string_literal_spans(block.as_str());
        for cap in SOURCE_PATTERN.captures_iter(block.as_str()) {
            if inside_string_literal(&literal_spans, cap.get(0).unwrap().start()) {
                continue;
            }
            sources.push(SourceCall {
                source_name: cap[1].to_string(),
                table_name: cap[2].to_string(),
            });
        }
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
    extract_all(sql, macro_prefix).config
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

    // ─── Refs nested inside macro arguments (GitHub issue: refs in macro args
    //     were dropped when jinja rendering failed on an unknown macro) ───

    #[test]
    fn test_refs_in_namespaced_macro_args() {
        // Package macros (pkg.macro_name) can't be resolved by minijinja, so
        // rendering fails; refs in the arguments must still be extracted.
        let sql = r#"
            {{
                shared_macros.import_cte([
                    ('orders', ref('int_orders_aggregated')),
                    ('returns', ref('int_returns_by_region')),
                    ('customers', ref('dim_customers'))
                ])
            }}
            SELECT * FROM orders
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 3);
        assert!(refs.iter().any(|r| r.name == "int_orders_aggregated"));
        assert!(refs.iter().any(|r| r.name == "int_returns_by_region"));
        assert!(refs.iter().any(|r| r.name == "dim_customers"));
    }

    #[test]
    fn test_refs_in_unknown_macro_args() {
        let sql = "{{ import_cte(ref('upstream_model')) }}";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "upstream_model");
    }

    #[test]
    fn test_ref_kwarg_in_package_macro() {
        // dbt_utils is typically not part of the scanned project macros
        let sql = "SELECT {{ dbt_utils.star(from=ref('stg_orders')) }} FROM x";
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "stg_orders");
    }

    #[test]
    fn test_ref_after_failed_macro_call_still_extracted() {
        // Rendering aborts at the unknown macro; refs after the failure point
        // must be recovered by the regex merge.
        let sql = r#"
            SELECT * FROM {{ ref('before_model') }}
            JOIN ({{ unknown_macro(ref('arg_model')) }}) USING (id)
            JOIN {{ ref('after_model') }} USING (id)
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 3);
        assert!(refs.iter().any(|r| r.name == "before_model"));
        assert!(refs.iter().any(|r| r.name == "arg_model"));
        assert!(refs.iter().any(|r| r.name == "after_model"));
    }

    #[test]
    fn test_dynamic_ref_salvaged_from_partial_render() {
        // ref(var(...)) cannot be found by regex; it must be salvaged from the
        // partial jinja render even though the template fails later.
        let sql = r#"
            SELECT * FROM {{ ref('model_' ~ var('env', 'dev')) }}
            JOIN ({{ unknown_macro() }}) USING (id)
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "model_dev");
    }

    #[test]
    fn test_runtime_env_var_branches_are_merged() {
        let sql = r#"
            {% if env_var('REGION') == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "orders_us"));
        assert!(refs.iter().any(|r| r.name == "orders_eu"));
    }

    #[test]
    fn test_runtime_fallback_ignores_plus_controlled_raw_blocks() {
        let sql = r#"
            {%+ raw +%}{{ ref('not_a_dependency') }}{%+ endraw +%}
            {% if execute %}
                {{ ref('execute_true_raw_guard') }}
            {% else %}
                {{ ref('execute_false_raw_guard') }}
            {% endif %}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "execute_true_raw_guard"));
        assert!(refs.iter().any(|r| r.name == "execute_false_raw_guard"));
        assert!(!refs.iter().any(|r| r.name == "not_a_dependency"));
    }

    #[test]
    fn test_execute_branches_are_merged() {
        let sql = r#"
            {% if execute %}
                SELECT * FROM {{ ref('execute_true') }}
            {% else %}
                SELECT * FROM {{ ref('execute_false') }}
            {% endif %}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "execute_true"));
        assert!(refs.iter().any(|r| r.name == "execute_false"));
    }

    #[test]
    fn test_top_level_uncertainty_recovers_alternate_macro_refs() {
        let sql = r#"
            {% macro selected() %}
                {% if env_var('REGION') == 'us' %}
                    {{ ref('selected_us_ref') }}
                {% else %}
                    {{ ref('selected_eu_ref') }}
                {% endif %}
            {% endmacro %}
            {% macro alternate() %}{{ ref('alternate_ref') }}{% endmacro %}
            {% if execute %}{{ selected() }}{% else %}{{ alternate() }}{% endif %}
        "#;
        let refs = extract_refs(sql);
        assert!(refs.iter().any(|r| r.name == "selected_us_ref"));
        assert!(refs.iter().any(|r| r.name == "selected_eu_ref"));
        assert!(refs.iter().any(|r| r.name == "alternate_ref"));
    }

    #[test]
    fn test_top_level_env_var_uncertainty_recovers_alternate_macro_refs() {
        let sql = r#"
            {% macro selected() %}{{ ref('selected_env_ref') }}{% endmacro %}
            {% macro alternate() %}{{ ref('alternate_env_ref') }}{% endmacro %}
            {% if env_var('REGION') == 'us' %}{{ selected() }}{% else %}{{ alternate() }}{% endif %}
        "#;
        let refs = extract_refs(sql);
        assert!(refs.iter().any(|r| r.name == "selected_env_ref"));
        assert!(refs.iter().any(|r| r.name == "alternate_env_ref"));
    }

    #[test]
    fn test_called_model_macro_runtime_branches_are_merged() {
        let sql = r#"
            {% macro runtime_branch() %}
                {% if execute %}{{ ref('called_true') }}{% else %}{{ ref('called_false') }}{% endif %}
            {% endmacro %}
            {% set invoke = runtime_branch %}
            {{ invoke() }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "called_true"));
        assert!(refs.iter().any(|r| r.name == "called_false"));
    }

    #[test]
    fn test_called_macro_scope_recovery_excludes_uncalled_macro_refs() {
        let sql = r#"
            {% macro called_branch() %}
                {% if env_var('REGION') == 'us' %}
                    {{ ref('called_us') }}
                {% else %}
                    {{ ref('called_eu') }}
                {% endif %}
            {% endmacro %}
            {% macro unused_branch() %}
                {{ ref('unused_ref') }}
            {% endmacro %}
            {{ called_branch() }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "called_us"));
        assert!(refs.iter().any(|r| r.name == "called_eu"));
        assert!(!refs.iter().any(|r| r.name == "unused_ref"));
    }

    #[test]
    fn test_called_target_macro_scope_recovery_excludes_uncalled_refs() {
        let sql = r#"
            {% macro called_branch() %}
                {% if target.name == 'us' %}{{ ref('target_us') }}{% else %}{{ ref('target_eu') }}{% endif %}
            {% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_target') }}{% endmacro %}
            {{ called_branch() }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "target_us"));
        assert!(refs.iter().any(|r| r.name == "target_eu"));
        assert!(!refs.iter().any(|r| r.name == "unused_target"));
    }

    #[test]
    fn test_called_missing_var_macro_scope_recovery_excludes_uncalled_refs() {
        let sql = r#"
            {% macro called_branch() %}
                {% if var('REGION') == 'us' %}{{ ref('var_us') }}{% else %}{{ ref('var_eu') }}{% endif %}
            {% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_var') }}{% endmacro %}
            {{ called_branch() }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "var_us"));
        assert!(refs.iter().any(|r| r.name == "var_eu"));
        assert!(!refs.iter().any(|r| r.name == "unused_var"));
    }

    #[test]
    fn test_transitive_macro_scope_recovery_excludes_uncalled_refs() {
        let sql = r#"
            {% macro inner_branch() %}
                {% if env_var('REGION') == 'us' %}{{ ref('inner_us') }}{% else %}{{ ref('inner_eu') }}{% endif %}
            {% endmacro %}
            {% macro outer_branch() %}{{ inner_branch() }}{% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_transitive') }}{% endmacro %}
            {{ outer_branch() }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "inner_us"));
        assert!(refs.iter().any(|r| r.name == "inner_eu"));
        assert!(!refs.iter().any(|r| r.name == "unused_transitive"));
    }

    #[test]
    fn test_higher_order_macro_scope_recovery_excludes_uncalled_refs() {
        let sql = r#"
            {% macro callback_branch() %}
                {% if env_var('REGION') == 'us' %}{{ ref('callback_us') }}{% else %}{{ ref('callback_eu') }}{% endif %}
            {% endmacro %}
            {% macro invoke(callback) %}{{ callback() }}{% endmacro %}
            {% macro unused_branch() %}{{ ref('unused_higher_order') }}{% endmacro %}
            {{ invoke(callback_branch) }}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "callback_us"));
        assert!(refs.iter().any(|r| r.name == "callback_eu"));
        assert!(!refs.iter().any(|r| r.name == "unused_higher_order"));
    }

    #[test]
    fn test_scalar_runtime_branches_are_merged() {
        for scalar in ["dbt_version", "invocation_id", "run_started_at"] {
            let sql = format!(
                "{{% if {scalar} %}}SELECT * FROM {{{{ ref('{scalar}_true') }}}}{{% else %}}SELECT * FROM {{{{ ref('{scalar}_false') }}}}{{% endif %}}"
            );
            let refs = extract_refs(&sql);
            assert_eq!(refs.len(), 2, "expected both refs for {scalar}");
            assert!(refs.iter().any(|r| r.name == format!("{scalar}_true")));
            assert!(refs.iter().any(|r| r.name == format!("{scalar}_false")));
        }
    }

    #[test]
    fn test_model_macro_spans_ignore_comment_and_raw_text() {
        let sql = r#"
            {# {% macro comment_fake() %}{% endmacro %} #}
            {% raw %}{% macro raw_fake() %}{% endmacro %}{% endraw %}
        "#;
        assert!(model_macro_definition_spans(sql).is_empty());
    }

    #[test]
    fn test_model_macro_spans_ignore_quoted_terminators() {
        let sql = r#"
            {% macro quoted(value="%} endmacro") %}
                {% set text = "%}" %}
            {% endmacro %}
        "#;
        let spans = model_macro_definition_spans(sql);
        assert_eq!(spans.len(), 1);
        let definition = &spans[0];
        let transformed =
            inject_macro_runtime_markers(sql, &spans, &HashSet::new(), "enter", "exit");
        assert_eq!(
            transformed
                .matches("{{ enter(\"quoted\", false) }}")
                .count(),
            1
        );
        assert!(definition.start < definition.end);
        assert_eq!(
            sql[definition.start..definition.end]
                .matches("endmacro")
                .count(),
            2
        );
    }

    #[test]
    fn test_model_macro_spans_support_plus_and_minus_whitespace_control() {
        let sql = r#"
            {%- macro minus() -%}minus{%- endmacro -%}
            {%+ macro plus() +%}plus{%+ endmacro +%}
        "#;
        assert_eq!(model_macro_definition_spans(sql).len(), 2);
    }

    #[test]
    fn test_model_macro_spans_find_multiple_macros() {
        let sql = "{% macro first() %}one{% endmacro %}{% macro second() %}two{% endmacro %}";
        let spans = model_macro_definition_spans(sql);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].end <= spans[1].start);
    }

    #[test]
    fn test_model_macro_spans_ignore_unclosed_macro() {
        let sql = "{% macro unclosed() %}{% if execute %}value";
        assert!(model_macro_definition_spans(sql).is_empty());
        assert_eq!(
            inject_macro_runtime_markers(sql, &[], &HashSet::new(), "enter", "exit"),
            sql
        );
    }

    #[test]
    fn test_missing_var_branches_are_merged() {
        let sql = r#"
            {% if var('REGION') == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "orders_us"));
        assert!(refs.iter().any(|r| r.name == "orders_eu"));
    }

    #[test]
    fn test_runtime_target_branches_are_merged() {
        let sql = r#"
            {% if target.name == 'us' %}
                SELECT * FROM {{ ref('orders_us') }}
            {% else %}
                SELECT * FROM {{ ref('orders_eu') }}
            {% endif %}
        "#;
        let refs = extract_refs(sql);
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().any(|r| r.name == "orders_us"));
        assert!(refs.iter().any(|r| r.name == "orders_eu"));
    }

    #[test]
    fn test_source_in_unknown_macro_args() {
        let sql = "{{ import_cte(source('raw', 'orders')) }}";
        let sources = extract_sources(sql);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_name, "raw");
        assert_eq!(sources[0].table_name, "orders");
    }

    #[test]
    fn test_defined_macro_called_with_namespace() {
        // Even when the macro body is known, dbt allows calling it with the
        // package namespace, which minijinja can't resolve.
        let macro_src =
            "{% macro import_cte(pairs) %}{% for p in pairs %}{{ p[1] }}{% endfor %}{% endmacro %}";
        let sql = "{{ my_package.import_cte([('orders', ref('int_orders'))]) }}";
        let (refs, _) = extract_refs_and_sources(sql, macro_src);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "int_orders");
    }

    #[test]
    fn test_regex_fallback_ref_in_set_statement() {
        let refs = extract_refs_regex("{% set orders = ref('stg_orders') %}");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "stg_orders");
    }

    #[test]
    fn test_regex_fallback_ignores_ref_outside_jinja_blocks() {
        // A bare ref('...') in a SQL comment is not a jinja call and must not
        // create a dependency.
        let sql = r#"
            -- replaced ref('old_model') with a CTE
            {{ unknown_macro() }}
            SELECT 1
        "#;
        let refs = extract_refs_regex(sql);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_regex_fallback_ignores_raw_blocks() {
        let sql = r#"
            {% raw %}{{ ref('not_a_dep') }}{% endraw %}
            {{ unknown_macro(ref('real_dep')) }}
        "#;
        let refs = extract_refs_regex(sql);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "real_dep");
    }

    #[test]
    fn test_regex_fallback_no_partial_identifier_match() {
        // myref(...) must not be mistaken for ref(...)
        let refs = extract_refs_regex("{{ myref('not_a_model') }}");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_regex_fallback_ignores_ref_text_in_string_literal() {
        // ref(...) appearing inside a string literal is text, not a call
        let refs = extract_refs_regex(r#"{{ unknown_macro("ref('not_a_dep')") }}"#);
        assert!(refs.is_empty());
    }

    #[test]
    fn test_regex_fallback_ignores_source_text_in_string_literal() {
        let sources = extract_sources_regex(r#"{% set msg = "source('raw', 'orders')" %}"#);
        assert!(sources.is_empty());
    }

    #[test]
    fn test_regex_fallback_real_ref_next_to_string_literal_ref_text() {
        let refs =
            extract_refs_regex(r#"{{ unknown_macro('label', "ref('fake')", ref('real_model')) }}"#);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "real_model");
    }

    #[test]
    fn test_regex_fallback_handles_escaped_quotes_in_strings() {
        // The escaped quote must not desynchronize string span tracking
        let refs =
            extract_refs_regex(r#"{{ unknown_macro("a \"quoted\" ref('fake')") + ref('real') }}"#);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].name, "real");
    }

    #[test]
    fn test_config_preserved_when_render_fails_later() {
        let sql = r#"
            {{ config(materialized='incremental', tags=['nightly']) }}
            {{ unknown_macro(ref('a')) }}
        "#;
        let ext = extract_all(sql, "");
        assert_eq!(ext.config.materialized.as_deref(), Some("incremental"));
        assert_eq!(ext.config.tags, vec!["nightly"]);
        assert_eq!(ext.refs.len(), 1);
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
