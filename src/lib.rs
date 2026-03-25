pub mod cli;
pub mod error;
pub mod graph;
pub mod input;
pub mod parser;
pub mod render;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

/// 0 = text (default), 1 = json
static ERROR_FORMAT: AtomicU8 = AtomicU8::new(0);

/// Enable quiet mode (suppress warnings on stderr).
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Release);
}

/// Set the error output format: `true` for JSON, `false` for text (default).
pub fn set_error_format_json(json: bool) {
    ERROR_FORMAT.store(if json { 1 } else { 0 }, Ordering::Release);
}

/// Returns true if error output should be JSON.
pub fn is_error_format_json() -> bool {
    ERROR_FORMAT.load(Ordering::Acquire) == 1
}

/// Print a warning message to stderr unless quiet mode is enabled.
/// Respects error format: emits JSON when `--error-format json` is set.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        if !$crate::is_quiet() {
            let msg = format!($($arg)*);
            if $crate::is_error_format_json() {
                eprintln!("{}", $crate::format_json_diagnostic_structured("warning", &msg, None, None));
            } else {
                eprintln!("Warning: {}", msg);
            }
        }
    };
}

/// Format a structured diagnostic as a JSON object for stderr.
///
/// Always emits all four fields `{"level","what","why","hint"}` so that
/// consumers can rely on a fixed schema. Missing values are `null`.
pub fn format_json_diagnostic_structured(
    level: &str,
    what: &str,
    why: Option<&str>,
    hint: Option<&str>,
) -> String {
    fn escape_json(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => out.push_str(r"\\"),
                '"' => out.push_str(r#"\""#),
                '\n' => out.push_str(r"\n"),
                '\r' => out.push_str(r"\r"),
                '\t' => out.push_str(r"\t"),
                c if c < '\x20' => {
                    out.push_str(&format!(r"\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out
    }
    fn json_str_or_null(val: Option<&str>, escape: &dyn Fn(&str) -> String) -> String {
        match val {
            Some(s) => format!(r#""{}""#, escape(s)),
            None => "null".to_string(),
        }
    }
    let level = escape_json(level);
    let what = escape_json(what);
    let why = json_str_or_null(why, &escape_json);
    let hint = json_str_or_null(hint, &escape_json);
    format!(r#"{{"level":"{level}","what":"{what}","why":{why},"hint":{hint}}}"#)
}

/// Structured error with optional Why and Hint fields.
///
/// Agents and humans can both recover faster when an error says *what* went
/// wrong, *why* it happened, and *what to do next*.
pub struct Diagnostic {
    pub what: String,
    pub why: Option<String>,
    pub hint: Option<String>,
}

impl Diagnostic {
    /// Create a diagnostic from an `anyhow::Error`, attaching context-specific
    /// Why / Hint when the message matches a known pattern.
    pub fn from_error(err: &anyhow::Error) -> Self {
        let what = format!("{err}");
        diagnose(what)
    }
}

/// Pattern-match an error message and attach Why / Hint when applicable.
fn diagnose(what: String) -> Diagnostic {
    // manifest not found (default path)
    if what.contains("No manifest.json found at") && what.contains("Use --manifest-path") {
        return Diagnostic {
            what,
            why: Some(
                "manifest source requires a compiled manifest.json produced by dbt".to_string(),
            ),
            hint: Some(
                "Run `dbt compile` to generate manifest.json, or use `--source sql` to parse SQL files directly".to_string(),
            ),
        };
    }

    // manifest not found (explicit --manifest-path pointing to directory)
    if what.contains("No manifest.json found at") && what.contains("Expected target/manifest.json")
    {
        return Diagnostic {
            what,
            why: Some("the specified directory does not contain target/manifest.json".to_string()),
            hint: Some(
                "Run `dbt compile` inside that project, or pass the exact file path with --manifest-path <path>/target/manifest.json".to_string(),
            ),
        };
    }

    // manifest path does not exist
    if what.starts_with("Manifest path does not exist:") {
        return Diagnostic {
            what,
            why: None,
            hint: Some(
                "Check the path for typos, or run `dbt compile` to generate manifest.json"
                    .to_string(),
            ),
        };
    }

    // --source sql + --manifest-path conflict
    if what.contains("--manifest-path cannot be used with --source sql") {
        return Diagnostic {
            what,
            why: Some(
                "--source sql parses .sql files directly and does not use manifest.json"
                    .to_string(),
            ),
            hint: Some("Use `--source manifest` to read from manifest.json, or remove --manifest-path to parse SQL files".to_string()),
        };
    }

    // model not found
    if what.starts_with("model not found:") {
        return Diagnostic {
            what,
            why: None,
            hint: Some("Check the spelling. Run `dlin list` to see available models".to_string()),
        };
    }

    // unknown JSON field(s)
    if what.starts_with("unknown JSON field(s):") {
        return Diagnostic {
            what,
            why: None,
            hint: Some("Use `--json-full` to emit all fields".to_string()),
        };
    }

    // no models found (impact command)
    if what.starts_with("no models found matching:") {
        return Diagnostic {
            what,
            why: None,
            hint: Some("Check the spelling. Run `dlin list` to see available models".to_string()),
        };
    }

    // no model names provided (impact command)
    if what.contains("no model names provided") {
        return Diagnostic {
            what,
            why: None,
            hint: Some(
                "Provide model names as arguments, e.g. `dlin impact stg_orders`".to_string(),
            ),
        };
    }

    // dbt project not found
    if what.contains("dbt project not found:") {
        return Diagnostic {
            what,
            why: None,
            hint: Some(
                "Ensure you are in a dbt project directory, or use --project-dir to specify one"
                    .to_string(),
            ),
        };
    }

    // cannot resolve project directory
    if what.starts_with("cannot resolve project directory") {
        return Diagnostic {
            what,
            why: None,
            hint: Some("Check that the directory exists and is accessible".to_string()),
        };
    }

    // Fallback: no Why/Hint
    Diagnostic {
        what,
        why: None,
        hint: None,
    }
}

/// Format a [`Diagnostic`] for stderr output, respecting the current error format.
pub fn format_diagnostic(diag: &Diagnostic) -> String {
    if is_error_format_json() {
        format_json_diagnostic_structured(
            "error",
            &diag.what,
            diag.why.as_deref(),
            diag.hint.as_deref(),
        )
    } else {
        let mut out = format!("Error: {}", diag.what);
        if let Some(ref why) = diag.why {
            out.push_str(&format!("\n  Why: {why}"));
        }
        if let Some(ref hint) = diag.hint {
            out.push_str(&format!("\n  Hint: {hint}"));
        }
        out
    }
}

/// Format an error for stderr output, respecting the current error format.
pub fn format_error(err: &dyn std::fmt::Display) -> String {
    if is_error_format_json() {
        format_json_diagnostic_structured("error", &err.to_string(), None, None)
    } else {
        format!("Error: {err}")
    }
}

/// Returns true if quiet mode is enabled.
pub fn is_quiet() -> bool {
    QUIET.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quiet_flag() {
        // Default is not quiet
        set_quiet(false);
        assert!(!is_quiet());

        set_quiet(true);
        assert!(is_quiet());

        // warn! should not panic in quiet mode
        warn!("this should be suppressed");

        set_quiet(false);
        assert!(!is_quiet());
    }

    #[test]
    fn test_error_format_flag() {
        set_error_format_json(false);
        assert!(!is_error_format_json());

        set_error_format_json(true);
        assert!(is_error_format_json());

        set_error_format_json(false);
        assert!(!is_error_format_json());
    }

    #[test]
    fn test_format_json_diagnostic_structured_basic() {
        let json = format_json_diagnostic_structured("error", "something broke", None, None);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["what"], "something broke");
        assert!(parsed["why"].is_null());
        assert!(parsed["hint"].is_null());
    }

    #[test]
    fn test_format_json_diagnostic_structured_escaping_quotes() {
        let json = format_json_diagnostic_structured(
            "warning",
            concat!(r#"bad "quotes" and"#, "\nnewline"),
            None,
            None,
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["level"], "warning");
        assert_eq!(parsed["what"], concat!(r#"bad "quotes" and"#, "\nnewline"));
    }

    #[test]
    fn test_format_json_diagnostic_structured_backslash() {
        let json = format_json_diagnostic_structured("error", r"path\to\file", None, None);
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["what"], r"path\to\file");
    }

    #[test]
    fn test_format_json_diagnostic_structured_control_chars() {
        // RFC 8259: control characters U+0000–U+001F must be \uXXXX escaped
        let json = format_json_diagnostic_structured(
            "error",
            "null:\x00 bell:\x07 backspace:\x08",
            None,
            None,
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["what"], "null:\x00 bell:\x07 backspace:\x08");
    }

    #[test]
    fn test_format_error_text() {
        set_error_format_json(false);
        let msg = format_error(&"something went wrong");
        assert_eq!(msg, "Error: something went wrong");
    }

    #[test]
    fn test_format_error_json() {
        set_error_format_json(true);
        let msg = format_error(&"something went wrong");
        let parsed: serde_json::Value = serde_json::from_str(&msg).unwrap();
        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["what"], "something went wrong");
        set_error_format_json(false);
    }

    #[test]
    fn test_diagnose_manifest_not_found_default() {
        let diag = diagnose(
            "No manifest.json found at /foo/target/manifest.json. Use --manifest-path or run `dbt compile` first.".to_string(),
        );
        assert!(diag.why.is_some());
        assert!(diag.hint.as_ref().unwrap().contains("dbt compile"));
        assert!(diag.hint.as_ref().unwrap().contains("--source sql"));
    }

    #[test]
    fn test_diagnose_manifest_not_found_directory() {
        let diag = diagnose(
            "No manifest.json found at /foo/target/manifest.json. Expected target/manifest.json in the directory.".to_string(),
        );
        assert!(diag.why.is_some());
        assert!(diag.hint.is_some());
    }

    #[test]
    fn test_diagnose_manifest_path_missing() {
        let diag = diagnose("Manifest path does not exist: /nonexistent".to_string());
        assert!(diag.why.is_none());
        assert!(diag.hint.as_ref().unwrap().contains("typos"));
    }

    #[test]
    fn test_diagnose_source_flag_conflict() {
        let diag = diagnose(
            "--manifest-path cannot be used with --source sql; did you mean --source manifest?"
                .to_string(),
        );
        assert!(diag.why.is_some());
        assert!(diag.hint.as_ref().unwrap().contains("--source manifest"));
    }

    #[test]
    fn test_diagnose_model_not_found() {
        let diag = diagnose("model not found: stg_orders".to_string());
        assert!(diag.hint.as_ref().unwrap().contains("dlin list"));
    }

    #[test]
    fn test_diagnose_unknown_json_fields() {
        let diag = diagnose("unknown JSON field(s): foo, bar. Available fields: a, b".to_string());
        assert!(diag.hint.as_ref().unwrap().contains("--json-full"));
    }

    #[test]
    fn test_diagnose_project_not_found() {
        let diag = diagnose("dbt project not found: no dbt_project.yml in /foo".to_string());
        assert!(diag.hint.as_ref().unwrap().contains("--project-dir"));
    }

    #[test]
    fn test_diagnose_fallback_no_hint() {
        let diag = diagnose("some unknown error".to_string());
        assert_eq!(diag.what, "some unknown error");
        assert!(diag.why.is_none());
        assert!(diag.hint.is_none());
    }

    #[test]
    fn test_format_diagnostic_text() {
        set_error_format_json(false);
        let diag = Diagnostic {
            what: "model not found: foo".to_string(),
            why: None,
            hint: Some("Run `dlin list`".to_string()),
        };
        let out = format_diagnostic(&diag);
        assert_eq!(out, "Error: model not found: foo\n  Hint: Run `dlin list`");
    }

    #[test]
    fn test_format_diagnostic_text_with_why() {
        set_error_format_json(false);
        let diag = Diagnostic {
            what: "something failed".to_string(),
            why: Some("the reason".to_string()),
            hint: Some("do this".to_string()),
        };
        let out = format_diagnostic(&diag);
        assert_eq!(
            out,
            "Error: something failed\n  Why: the reason\n  Hint: do this"
        );
    }

    #[test]
    fn test_format_diagnostic_json_with_hint() {
        set_error_format_json(true);
        let diag = Diagnostic {
            what: "model not found: foo".to_string(),
            why: None,
            hint: Some("Run `dlin list`".to_string()),
        };
        let out = format_diagnostic(&diag);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["level"], "error");
        assert_eq!(parsed["what"], "model not found: foo");
        assert!(parsed["why"].is_null());
        assert_eq!(parsed["hint"], "Run `dlin list`");
        set_error_format_json(false);
    }

    #[test]
    fn test_format_diagnostic_json_full() {
        set_error_format_json(true);
        let diag = Diagnostic {
            what: "it broke".to_string(),
            why: Some("bad input".to_string()),
            hint: Some("fix it".to_string()),
        };
        let out = format_diagnostic(&diag);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["what"], "it broke");
        assert_eq!(parsed["why"], "bad input");
        assert_eq!(parsed["hint"], "fix it");
        set_error_format_json(false);
    }

    #[test]
    fn test_format_diagnostic_json_no_hint() {
        set_error_format_json(true);
        let diag = Diagnostic {
            what: "unknown error".to_string(),
            why: None,
            hint: None,
        };
        let out = format_diagnostic(&diag);
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["what"], "unknown error");
        assert!(parsed["why"].is_null());
        assert!(parsed["hint"].is_null());
        set_error_format_json(false);
    }

    #[test]
    fn test_format_json_diagnostic_structured_escaping() {
        let json = format_json_diagnostic_structured(
            "error",
            r#"bad "quotes""#,
            Some("line\nnewline"),
            Some(r"path\to\fix"),
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["what"], r#"bad "quotes""#);
        assert_eq!(parsed["why"], "line\nnewline");
        assert_eq!(parsed["hint"], r"path\to\fix");
    }
}
