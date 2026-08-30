use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use super::super::jinja::source::{
    ModelMacroSpan, inside_string_literal, model_macro_definition_spans, string_literal_spans,
    strip_inert_jinja,
};
use super::{RefCall, SourceCall, SqlConfig, normalize_version_str};

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

/// Regex fallback for extracting ref() calls.
/// Scans inside every jinja block so calls nested in macro arguments or
/// {% set %} statements are found too, mirroring dbt which registers a
/// ref() wherever it is evaluated.
#[cfg(test)]
pub(super) fn extract_refs_regex(sql: &str) -> Vec<RefCall> {
    extract_refs_regex_scoped(sql, None)
}

#[cfg(test)]
pub(super) fn extract_refs_regex_scoped(
    sql: &str,
    macro_scopes: Option<&HashSet<String>>,
) -> Vec<RefCall> {
    extract_refs_and_sources_regex_scoped(sql, macro_scopes).0
}

/// Extract refs and sources in one cleaned-source/Jinja-block traversal.
/// Keeping the two result vectors separate preserves the public extraction
/// shape while avoiding duplicate parsing and string-literal scans in the
/// fallback path.
pub(super) fn extract_refs_and_sources_regex_scoped(
    sql: &str,
    macro_scopes: Option<&HashSet<String>>,
) -> (Vec<RefCall>, Vec<SourceCall>) {
    let cleaned = strip_inert_jinja(sql);
    let definitions = macro_scopes.map(|_| model_macro_definition_spans(&cleaned));
    let mut refs = Vec::new();
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

/// Extract refs from already-selected macro definition slices. Reachability
/// supplies spans from the original prefix, so successful scoped recovery can
/// avoid rescanning every unrelated prefix definition and its owner lookup.
#[cfg(test)]
pub(super) fn extract_refs_regex_spans(source: &str, spans: &[ModelMacroSpan]) -> Vec<RefCall> {
    extract_refs_and_sources_regex_spans(source, spans).0
}

/// Extract refs and sources from selected macro definition slices, cleaning
/// and scanning each slice only once.
pub(super) fn extract_refs_and_sources_regex_spans(
    source: &str,
    spans: &[ModelMacroSpan],
) -> (Vec<RefCall>, Vec<SourceCall>) {
    let mut refs = Vec::new();
    let mut sources = Vec::new();
    for span in spans {
        let Some(definition) = source.get(span.start..span.end) else {
            continue;
        };
        let (definition_refs, definition_sources) =
            extract_refs_and_sources_regex_scoped(definition, None);
        refs.extend(definition_refs);
        sources.extend(definition_sources);
    }
    (refs, sources)
}

/// Regex fallback for extracting source() calls.
/// Scans inside every jinja block, like [`extract_refs_regex`].
#[cfg(test)]
pub(super) fn extract_sources_regex(sql: &str) -> Vec<SourceCall> {
    extract_sources_regex_scoped(sql, None)
}

#[cfg(test)]
pub(super) fn extract_sources_regex_scoped(
    sql: &str,
    macro_scopes: Option<&HashSet<String>>,
) -> Vec<SourceCall> {
    extract_refs_and_sources_regex_scoped(sql, macro_scopes).1
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

/// Regex fallback for extracting config() settings.
pub(super) fn extract_config_regex(sql: &str) -> SqlConfig {
    let cleaned = super::super::jinja::source::strip_jinja_comments(sql);
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
