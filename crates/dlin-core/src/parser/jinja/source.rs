use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

static JINJA_COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{#[\s\S]*?#\}").unwrap());

// Matches {% raw %}...{% endraw %} sections, whose content jinja treats as literal text
static JINJA_RAW_BLOCK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{%[+-]?\s*raw\s*[+-]?%\}[\s\S]*?\{%[+-]?\s*endraw\s*[+-]?%\}").unwrap()
});

/// Strip Jinja comments from SQL content
pub(crate) fn strip_jinja_comments(sql: &str) -> String {
    JINJA_COMMENT.replace_all(sql, "").to_string()
}

/// Strip jinja constructs whose content is never evaluated ({# #} comments
/// and {% raw %} sections), leaving only renderable template text.
pub(crate) fn strip_inert_jinja(sql: &str) -> String {
    let no_comments = strip_jinja_comments(sql);
    JINJA_RAW_BLOCK.replace_all(&no_comments, "").to_string()
}

/// Remove the supplied model-local macro spans, then remove inert Jinja
/// constructs. Keeping span discovery separate lets callers reuse one scan
/// for both direct model and macro-local runtime analysis.
pub(crate) fn strip_macro_definitions_for_runtime_analysis(
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
pub(crate) struct ModelMacroSpan {
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) opening_end: usize,
    pub(crate) closing_start: usize,
    pub(crate) name: String,
}

pub(crate) fn model_macro_definition_spans(sql: &str) -> Vec<ModelMacroSpan> {
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
pub(crate) fn inject_macro_runtime_markers(
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
pub(crate) fn string_literal_spans(block: &str) -> Vec<(usize, usize)> {
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
pub(crate) fn inside_string_literal(spans: &[(usize, usize)], pos: usize) -> bool {
    spans.iter().any(|&(start, end)| pos > start && pos < end)
}
