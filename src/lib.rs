pub mod cli;
pub mod error;
pub mod graph;
pub mod input;
pub mod parser;
pub mod render;
#[cfg(feature = "tui")]
pub mod tui;

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
                eprintln!("{}", $crate::format_json_diagnostic("warning", &msg));
            } else {
                eprintln!("Warning: {}", msg);
            }
        }
    };
}

/// Format a diagnostic message as a JSON object for stderr.
pub fn format_json_diagnostic(level: &str, message: &str) -> String {
    // Manual JSON construction to avoid serde dependency in lib root.
    let escaped = message
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("{{\"level\":\"{level}\",\"message\":\"{escaped}\"}}")
}

/// Format an error for stderr output, respecting the current error format.
pub fn format_error(err: &dyn std::fmt::Display) -> String {
    if is_error_format_json() {
        format_json_diagnostic("error", &err.to_string())
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
    fn test_format_json_diagnostic() {
        let json = format_json_diagnostic("error", "something broke");
        assert_eq!(json, r#"{"level":"error","message":"something broke"}"#);
    }

    #[test]
    fn test_format_json_diagnostic_escaping() {
        let json = format_json_diagnostic("warning", "bad \"quotes\" and\nnewline");
        assert_eq!(
            json,
            r#"{"level":"warning","message":"bad \"quotes\" and\nnewline"}"#
        );
    }

    #[test]
    fn test_format_json_diagnostic_backslash() {
        let json = format_json_diagnostic("error", r"path\to\file");
        assert_eq!(
            json,
            r#"{"level":"error","message":"path\\to\\file"}"#
        );
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
        assert_eq!(
            msg,
            r#"{"level":"error","message":"something went wrong"}"#
        );
        set_error_format_json(false);
    }
}
