pub mod cli;
pub mod error;
pub mod graph;
pub mod input;
pub mod parser;
pub mod render;
#[cfg(feature = "tui")]
pub mod tui;

use std::sync::atomic::{AtomicBool, Ordering};

static QUIET: AtomicBool = AtomicBool::new(false);

/// Enable quiet mode (suppress warnings on stderr).
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Release);
}

/// Print a warning message to stderr unless quiet mode is enabled.
#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => {
        if !$crate::is_quiet() {
            eprintln!("Warning: {}", format!($($arg)*));
        }
    };
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
}
