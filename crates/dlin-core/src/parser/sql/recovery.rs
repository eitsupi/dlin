//! Literal `ref()`/`source()`/`config()` calls found by pattern matching,
//! for text the Jinja parser cannot handle: templates that do not parse and
//! the dependency expressions written in YAML.

use std::sync::LazyLock;

use regex::Regex;

use super::{RefCall, SourceCall, SqlConfig, normalize_version_str};

static JINJA_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{#[\s\S]*?#\}").unwrap());

// Matches {% raw %}...{% endraw %} sections, whose content jinja treats as literal text
static JINJA_RAW_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{%[+-]?\s*raw\s*[+-]?%\}[\s\S]*?\{%[+-]?\s*endraw\s*[+-]?%\}").unwrap()
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

/// The `ref()` and `source()` calls whose arguments are all literals, inside
/// any Jinja block outside comments and raw sections.
pub(crate) fn literal_calls(sql: &str) -> (Vec<RefCall>, Vec<SourceCall>) {
    let cleaned = strip_inert_jinja(sql);
    let mut refs = Vec::new();
    let mut sources = Vec::new();

    for block in JINJA_BLOCK.find_iter(&cleaned) {
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

    (refs, sources)
}

// Matches {{ config(...) }} blocks — captures the inner arguments.
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

// Matches materialized='value' or materialized="value".
static MATERIALIZED_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"materialized\s*=\s*['"]([^'"]+)['"]"#).unwrap());

// Matches tags=['a', 'b'] or tags=["a", "b"].
static TAGS_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"tags\s*=\s*\[([^\]]*)\]"#).unwrap());

// Matches individual tag values inside the tags list.
static TAG_VALUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"]([^'"]+)['"]"#).unwrap());

/// The literal settings of the first `config()` call.
pub(crate) fn extract_config_regex(sql: &str) -> SqlConfig {
    let cleaned = strip_jinja_comments(sql);
    let mut config = SqlConfig::default();

    if let Some(cap) = CONFIG_PATTERN.captures(&cleaned) {
        let inner = &cap[1];
        if let Some(mat) = MATERIALIZED_PATTERN.captures(inner) {
            config.materialized = Some(mat[1].to_string());
        }
        if let Some(tags_cap) = TAGS_PATTERN.captures(inner) {
            config.tags = TAG_VALUE
                .captures_iter(&tags_cap[1])
                .map(|capture| capture[1].to_string())
                .collect();
        }
    }

    config
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
