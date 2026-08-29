use clap::Parser;
use dlin_core::graph::column_lineage::DlinDialect;

use super::command::Command;

/// Parse a `--dialect` value via `DlinDialect`'s `FromStr` implementation.
///
/// `DlinDialect` derives `clap::ValueEnum` (for its per-variant aliases), but
/// clap's automatic parser selection would then list every dialect spelling
/// as a "Possible values" block in `--help`, which is not how this flag has
/// ever been documented (its accepted spellings are described in prose in
/// each command's own help text instead). Pinning the parser to `FromStr`
/// keeps the flag's parsing behavior — and `--help` output — unchanged.
pub(super) fn parse_dialect(s: &str) -> Result<DlinDialect, String> {
    s.parse()
}

/// A dialect supplied by a user, retaining the original spelling for
/// compatibility diagnostics while exposing the parsed enum to callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialectArg {
    pub dialect: DlinDialect,
    pub requested: String,
}

impl PartialEq<DlinDialect> for DialectArg {
    fn eq(&self, other: &DlinDialect) -> bool {
        self.dialect == *other
    }
}

pub(super) fn parse_dialect_arg(s: &str) -> Result<DialectArg, String> {
    Ok(DialectArg {
        dialect: parse_dialect(s)?,
        requested: s.to_string(),
    })
}

#[derive(Parser, Debug)]
#[command(
    name = "dlin",
    about = "A fast CLI tool for dbt model lineage analysis",
    long_about = "\
A fast CLI tool for dbt model lineage analysis.

Parses SQL files directly to extract ref() and source() dependencies (no dbt compile needed), \
or reads manifest.json for full-fidelity graphs. Outputs as ASCII, DOT, JSON, Mermaid, SVG, \
or HTML.

Data sources:
  sql (default)   Parse SQL files via regex + minijinja. No Python or dbt required.
                  Detects ref() and source() calls in SQL, plus exposures from YAML.
                  Generic tests (not_null, unique, etc.) are inferred from YAML declarations
                  with dlin-specific IDs — use manifest mode for exact dependency resolution.
  manifest        Read a pre-compiled manifest.json for full accuracy. Requires
                  `dbt compile` (or `dbt run`/`dbt build`) to have been run first.
                  Use `dlin check-manifest` to verify freshness before querying.

  Use sql mode for quick, local exploration without dbt setup. Switch to manifest
  mode when you need complete test coverage or exact materialization info.

Stdin support:
  Accepts model names or file paths from stdin. File paths (detected by extension \
or path separators) are resolved to model names automatically.

Exit codes:
  0   Success
  1   Error (project not found, all specified models not found, etc.)

Error format (--error-format):
  text (default)  Human-readable with What/Why/Hint structure:
                  Error: <what>
                    Why: <why>     (when available)
                    Hint: <hint>   (when available)
  json            Structured JSON on stderr (fixed schema):
                  {\"level\":\"error\",\"what\":\"...\",\"why\":...,\"hint\":...}
                  why and hint are strings or null.
                  Also settable via DLIN_ERROR_FORMAT=json",
    after_long_help = "\
Examples:
  dlin graph                                       # Full lineage (ASCII art)
  dlin graph -o json                               # Full lineage as JSON
  dlin graph orders -u 2 -d 1                      # orders with 2 upstream, 1 downstream
  dlin graph -o json --json-full                   # JSON with all fields
  dlin list -o json                                # List all node names as JSON
  dlin list orders -o json --json-full             # Model details: path, description, columns, materialization
  dlin list --search shipping                      # Search models by name or description (regex)
  dlin list orders stg_orders -o json              # List specific models as JSON
  dlin impact orders -o json                       # Downstream impact analysis
  dlin summary                                     # Project overview (node counts, etc.)
  dlin summary -o json                             # Project overview as JSON
  dlin check-manifest || dbt compile               # Recompile if stale or files deleted
  git diff --name-only main | dlin graph           # Lineage of changed files
  dlin column upstream orders                      # Upstream: where do columns come from? (requires dbt compile)
  dlin column downstream stg_orders --column order_id  # Downstream: what depends on this column? (requires dbt compile)",
    version
)]
pub struct Cli {
    /// Error/warning output format on stderr
    #[arg(
        long,
        global = true,
        default_value = "text",
        env = "DLIN_ERROR_FORMAT",
        long_help = "\
Error/warning output format on stderr: text (default) or json.

When json, diagnostics are emitted as structured JSON objects with a
fixed schema: {\"level\":\"error\"|\"warning\",\"what\":\"...\",\"why\":...,\"hint\":...}
where why and hint are either strings or null."
    )]
    pub error_format: ErrorFormat,

    #[command(subcommand)]
    pub command: Command,
}

pub use dlin_core::{CollapseMode, Direction, GroupBy, ListOutputFormat};

#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum ErrorFormat {
    /// Human-readable error messages (default)
    Text,
    /// Structured JSON on stderr: {"level":"...","what":"...","why":...,"hint":...}
    Json,
}
