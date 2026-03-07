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
    QUIET.store(quiet, Ordering::Relaxed);
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
    QUIET.load(Ordering::Relaxed)
}
