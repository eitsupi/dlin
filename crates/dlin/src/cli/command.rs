use std::path::PathBuf;

use super::*;
use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Visualize dbt model lineage graph
    Graph(GraphArgs),

    /// List nodes in the dbt project (lightweight, no edges)
    List(ListArgs),

    /// Compute downstream impact analysis for a model (with severity scoring)
    #[command(
        long_about = "\
Compute downstream impact analysis for one or more models.

Finds all downstream dependents and assigns severity levels:
  Critical  impacts exposures (dashboards, reports)
  High      impacts table/incremental materializations or mart models
  Medium    impacts staging or intermediate models
  Low       impacts tests only

Text output: human-readable report with severity and distance (degree).
JSON output: structured array of impact reports for CI/programmatic use.

Stdin/pipe support:
  Accepts model names or file paths on stdin (one per line). \
  File paths (detected by extension or path separator) are resolved to model names \
  using the dbt project configuration.

Exit codes:
  0   Success (impact computed, even if no downstream dependents)
  1   Error (no models specified, or all specified models not found)",
        after_long_help = "\
Examples:
  # Text report for a single model
  dlin impact orders

  # JSON output for CI integration
  dlin impact orders -o json

  # Multiple models at once
  dlin impact orders stg_orders -o json

  # From git diff (pipe changed files)
  git diff --name-only main | dlin impact -o json

  # Use manifest instead of SQL parsing
  dlin impact orders --source manifest --manifest-path target/manifest.json"
    )]
    Impact {
        /// Model names to analyze impact for (also accepts stdin)
        model: Vec<String>,

        /// Path to dbt project directory
        #[arg(short = 'p', long = "project-dir", default_value = ".")]
        project_dir: PathBuf,

        /// Directory for caching extraction results (default: <project-dir>/.dlin_cache)
        #[arg(long, env = "DLIN_CACHE_DIR")]
        cache_dir: Option<PathBuf>,

        /// Disable extraction cache (always re-parse all files, results are not saved)
        #[arg(long, env = "DLIN_NO_CACHE")]
        no_cache: bool,

        /// Discard existing cache and rebuild from scratch
        #[arg(long, env = "DLIN_REFRESH_CACHE", conflicts_with = "no_cache")]
        refresh_cache: bool,

        /// Output format: text (default) or json
        #[arg(short = 'o', long, default_value = "text")]
        output: ImpactOutputFormat,

        /// Data source: sql (default) or manifest
        #[arg(
            long,
            default_value = "sql",
            long_help = "\
Data source for building the lineage graph.

  sql (default)   Parse SQL files directly — no dbt or Python required.
                  Exposures and generic tests are detected from YAML with
                  dlin-specific IDs; use manifest mode for exact test
                  dependency resolution.
  manifest        Use dbt manifest.json for full accuracy. Requires
                  prior `dbt compile` (or `dbt run`/`dbt build`)."
        )]
        source: SourceType,

        /// Path to manifest.json file or directory containing target/manifest.json
        #[arg(
            long,
            long_help = "\
Path to manifest.json file or directory containing target/manifest.json.

Default: <project-dir>/target/manifest.json"
        )]
        manifest_path: Option<PathBuf>,

        /// Suppress warning messages
        #[arg(short = 'q', long)]
        quiet: bool,
    },

    /// Show project summary (node counts, manifest status, etc.)
    #[command(
        long_about = "\
Show a summary of the dbt project: node counts by type, edge count, \
variable definitions, and manifest.json freshness.

Useful for onboarding, CI logs, and feeding project context to AI agents.

Output formats:
  text (default)  Human-readable summary
  json            Structured JSON for programmatic use

Manifest freshness is checked automatically when a manifest.json is found \
at the default location (<project-dir>/target/manifest.json) or at the \
path given by --manifest-path. The check detects both files newer than the \
manifest (stale) and files referenced in the manifest but missing from disk \
(deleted), using the same logic as `dlin check-manifest`.",
        after_long_help = "\
Examples:
  # Quick project overview
  dlin summary

  # JSON output for AI agents
  dlin summary -o json

  # Use manifest as data source
  dlin summary --source manifest"
    )]
    Summary(SummaryArgs),

    /// Column-level lineage and impact analysis
    #[command(
        long_about = "\
Column-level lineage and impact analysis.

Unlike `dlin graph` (model-level, bidirectional, with -u/-d depth control),
column analysis is split by direction — each subcommand covers one direction:

  upstream    — traces where each output column's data came from
  downstream  — finds which models/columns are affected by a column change

There are no -u/-d depth flags; the full chain is always traversed in both cases.

Unlike `dlin graph`/`dlin impact`, there is no --source flag — manifest.json is the only
data source for column-level analysis.

Both subcommands require manifest.json with compiled SQL (run `dbt compile` first).
Use `dlin check-manifest` to verify freshness before querying.",
        after_long_help = "\
Examples:
  dlin column upstream orders                             # upstream: where do columns come from?
  dlin column downstream stg_orders --column order_id     # downstream: what depends on this column?"
    )]
    Column(ColumnArgs),

    /// Low-level debugging tools for SQL parsing and lineage tracing
    #[command(
        long_about = "\
Low-level debugging tools for SQL parsing and column lineage.

These subcommands operate on raw SQL strings without requiring a dbt project \
or manifest.json, making them useful for isolating parsing or lineage issues.

Subcommands:
  parse-sql       Parse SQL and display the AST (Debug or JSON)
  trace-column    Trace a single column's lineage through a SQL statement",
        after_long_help = "\
Examples:
  # Parse SQL and show AST debug output
  dlin debug parse-sql 'SELECT a, b FROM t' --dialect bigquery

  # Parse SQL from a file via stdin
  dlin debug parse-sql --dialect snowflake < query.sql

  # Trace a column's lineage
  dlin debug trace-column 'SELECT t.id AS order_id FROM t' --column order_id

  # Trace with schema information
  dlin debug trace-column 'SELECT * FROM t' --column id --schema 't:id,name'"
    )]
    Debug(DebugArgs),

    /// Check if manifest.json is up-to-date (detects stale and deleted files)
    #[command(
        name = "check-manifest",
        long_about = "\
Helper for working with manifest.json — checks whether it needs to be regenerated.

Since `dbt compile` can be slow (seconds to tens of seconds depending on project \
size and warehouse connection), this command lets you skip unnecessary recompilation \
by detecting whether any project files have changed since the manifest was last built.

Performs two checks:
  1. Compares the modification time of manifest.json against all project SQL and \
YAML files (models, macros, tests, snapshots, seeds, and analyses). Files newer \
than the manifest are reported as 'stale'.
  2. Reads nodes and sources from manifest.json and checks that their referenced \
source files still exist on disk. Missing files are reported as 'deleted'.

If either stale or deleted files are found, exits with code 1.

This command does not use dlin's extraction cache; it only compares file timestamps.

Exit codes:
  0   Manifest is up-to-date (no stale or deleted files)
  1   Manifest is stale (files newer or deleted) or manifest not found",
        after_long_help = "\
Examples:
  # Check and conditionally recompile
  dlin check-manifest || dbt compile

  # Quiet mode (exit code only)
  dlin check-manifest -q

  # JSON output for programmatic use
  dlin check-manifest -o json

  # Check with explicit manifest path
  dlin check-manifest --manifest-path path/to/manifest.json"
    )]
    CheckManifest(CheckManifestArgs),

    /// Run an MCP server over stdio for AI agents
    #[command(
        long_about = "\
Run a Model Context Protocol (MCP) server over stdio.

This exposes manifest-backed dlin analysis as typed JSON-RPC tools for MCP
clients such as Claude Desktop. The server reads one JSON-RPC message per line
from stdin and writes one JSON-RPC response per line to stdout.

MCP uses manifest mode only. Run `dbt compile` first so target/manifest.json
exists and contains compiled SQL for column lineage tools.",
        after_long_help = "\
Examples:
  dlin mcp --project-dir /path/to/dbt/project --dialect bigquery
  dlin mcp --project-dir /path/to/dbt/project --manifest-path target/manifest.json --dialect snowflake"
    )]
    Mcp(McpArgs),
}
