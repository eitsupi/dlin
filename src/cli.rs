use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
                  YAML-defined generic tests (not_null, unique, etc.) are NOT detected
                  — use manifest mode for full test coverage.
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
  dlin graph                              # Full lineage (ASCII art)
  dlin graph -o json                      # Full lineage as JSON
  dlin graph orders -u 2 -d 1             # orders with 2 upstream, 1 downstream
  dlin graph -o json --json-full           # JSON with all fields
  dlin list -o json                       # List all node names as JSON
  dlin list orders stg_orders -o json     # List specific models as JSON
  dlin impact orders -o json              # Downstream impact analysis
  dlin summary                            # Project overview (node counts, etc.)
  dlin summary -o json                    # Project overview as JSON
  dlin check-manifest || dbt compile      # Recompile if stale or files deleted
  git diff --name-only main | dlin graph  # Lineage of changed files",
    version
)]
pub struct Cli {
    /// Error/warning output format on stderr: text (default) or json.
    /// When json, diagnostics are emitted as structured JSON objects with a
    /// fixed schema: {"level":"error"|"warning","what":"...","why":...,"hint":...}
    /// where why and hint are either strings or null
    #[arg(long, global = true, default_value = "text", env = "DLIN_ERROR_FORMAT")]
    pub error_format: ErrorFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GroupBy {
    /// Group nodes by node type (source, model, test, etc.)
    NodeType,
    /// Group nodes by their file directory (e.g. models/staging, models/marts)
    Directory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Direction {
    /// Left to right (default)
    LR,
    /// Top to bottom
    TB,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::LR => write!(f, "LR"),
            Direction::TB => write!(f, "TB"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum ErrorFormat {
    /// Human-readable error messages (default)
    Text,
    /// Structured JSON on stderr: {"level":"...","what":"...","why":...,"hint":...}
    Json,
}

#[derive(Debug, clap::Args)]
#[command(
    long_about = "\
Visualize dbt model lineage graph.

Shows the dependency graph of dbt models, sources, seeds, snapshots, tests, and exposures. \
By default all node types are included. Use --node-type to restrict output to specific types. \
Use positional arguments to focus on specific models.

Focusing vs filtering:
  Positional arguments (MODEL) set the focus: BFS traversal from those models \
discovers upstream/downstream neighbors. -u/-d control how many levels to traverse. \
If no positional arguments are provided, the initial focus is the full graph \
before any additional filtering is applied.
  --select (-s) filters the focused set by condition (label glob, tag, path). \
With positional arguments, the BFS result is intersected with the selector \
matches; without positional arguments, the focus is narrowed to only the \
selector matches.

Output formats:
  ascii (default)  Text-based DAG for terminal display
  json             Machine-readable {\"nodes\":[...], \"edges\":[...]} structure
  dot              Graphviz DOT format (pipe to `dot -Tsvg > out.svg`)
  mermaid          Mermaid diagram syntax
  plain            One node name per line (useful for scripting)
  svg              Self-contained SVG file
  html             Interactive HTML with pan/zoom/search

Depth control (-u/-d):
  Without -u/-d, all reachable nodes are included.
  -u 0 -d 0 shows only the focus model itself.
  BFS traversal follows edges regardless of node type.

Node type filter (--node-type):
  By default all node types are shown. Use --node-type to restrict output.
  Applied as a post-filter AFTER depth traversal. Only matching node types \
appear in the output. By default, edges through excluded nodes are preserved \
as transitive edges (see --no-transitive).
    --node-type model,source    # only models and sources

Transitive edge completion:
  When filters (--node-type, --select, or focus models) remove intermediate nodes, \
edges through those nodes are preserved as transitive edges. Many outputs annotate \
these with how many intermediate nodes were collapsed (for example, DOT/Mermaid \
label edges as \"type (via N)\", and JSON/HTML expose a collapsed_through field). \
Use --no-transitive to disable this and drop such edges instead.

Stdin/pipe support:
  Accepts model names or file paths on stdin (one per line). \
File paths are resolved to model names using dbt project configuration.",
    after_long_help = "\
Examples:
  # === Full project lineage ===
  dlin graph
  dlin graph -o json

  # === Focus on specific models (positional args = BFS from those nodes) ===
  dlin graph orders -u 2 -d 1           # 2 upstream, 1 downstream of orders
  dlin graph stg_orders -d 0            # just the node, no downstream
  dlin graph stg_orders orders customers # multiple focus models

  # === Filter by selector (--select/-s = keep only matching nodes) ===
  dlin graph -s path:models/marts -o json
  dlin graph -s 'path:**/staging/**' -o json
  dlin graph -s 'tag:finance,path:**/staging/**' -o json   # OR logic
  dlin graph -s 'stg_*' -o json                            # label glob
  dlin graph -s 'tag:night*' -o json                       # tag glob

  # === Combine focus + selector (intersect: BFS result AND selector match) ===
  dlin graph orders -d 3 -s 'path:**/staging/**'  # downstream of orders, in staging/

  # === Node type filter (post-filter, transitive edges preserved by default) ===
  dlin graph orders -u 3 --node-type source,model -o json
  dlin graph raw.orders -d 2 --node-type source,model -o json
  dlin graph --node-type source,exposure -o mermaid  # sources feeding exposures
  dlin graph --node-type source -o json              # list sources only

  # Disable transitive edge completion
  dlin graph --node-type model --no-transitive -o json

  # === Stdin / git integration ===
  git diff --name-only main | dlin graph -o json

  # === Data source ===
  dlin graph --source manifest --manifest-path target/manifest.json

  # === JSON field control ===
  dlin graph -o json --json-fields unique_id,label,description
  dlin graph -o json --json-full

  # === Graphviz / visual output ===
  dlin graph -o dot | dot -Tsvg > lineage.svg
  dlin graph -o mermaid --direction tb   # top-to-bottom layout"
)]
pub struct GraphArgs {
    /// Model names to focus on (shows full lineage if omitted)
    pub model: Vec<String>,

    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Directory for caching extraction results (default: <project-dir>/.dlin_cache)
    #[arg(long, env = "DLIN_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Disable extraction cache (always re-parse all files, results are not saved)
    #[arg(long, env = "DLIN_NO_CACHE")]
    pub no_cache: bool,

    /// Discard existing cache and rebuild from scratch
    #[arg(long, env = "DLIN_REFRESH_CACHE", conflicts_with = "no_cache")]
    pub refresh_cache: bool,

    /// Upstream levels to show (default: all)
    #[arg(short = 'u', long)]
    pub upstream: Option<usize>,

    /// Downstream levels to show (default: all)
    #[arg(short = 'd', long)]
    pub downstream: Option<usize>,

    /// Output format: ascii (default), dot, json, mermaid, plain, svg, html
    #[arg(short = 'o', long, default_value = "ascii")]
    pub output: OutputFormat,

    /// Selector expression (comma-separated, OR logic).
    /// All selectors support glob patterns (*, **, ?, []):
    ///   tag:<pattern>     match nodes by tag
    ///   path:<pattern>    match by file path (prefix or glob)
    ///   <pattern>         match by model label
    #[arg(short = 's', long)]
    pub select: Option<String>,

    /// Filter output by node type (comma-separated). Default: all types.
    /// Available types: model, source, seed, snapshot, test, exposure.
    ///
    /// WARNING: In sql mode, only singular tests (SQL files in tests/) are detected.
    /// YAML-defined generic tests (not_null, unique, relationships, etc.) are NOT detected.
    /// Use --source manifest for full test coverage.
    #[arg(long = "node-type", value_delimiter = ',')]
    pub node_types: Option<Vec<String>>,

    /// Disable transitive edge completion when filters remove intermediate nodes.
    /// By default, when --node-type, --select, or focus models exclude nodes,
    /// edges through removed nodes are preserved as transitive edges with "(via N)" labels.
    #[arg(long)]
    pub no_transitive: bool,

    /// Collapse intermediate nodes, keeping only endpoints (nodes with no
    /// predecessors or no successors). Removed nodes become transitive edges
    /// with "(via N)" labels. When combined with --group-by, endpoints are
    /// determined per group: a node is kept if it connects to a node outside
    /// its group, or has no predecessors/successors within its group.
    /// Without --group-by, global graph endpoints are kept.
    /// Tip: with --source manifest, tests may appear as downstream endpoints;
    /// combine with --node-type to exclude them if needed.
    #[arg(long)]
    pub collapse: bool,

    /// Group nodes using subgraph/cluster blocks in supported formats (dot, mermaid).
    /// Supported values: node-type (group by source, model, test, etc.),
    /// directory (group by file directory path)
    #[arg(long = "group-by")]
    pub group_by: Option<GroupBy>,

    /// Graph direction for dot and mermaid output.
    /// LR = left to right (default), TB = top to bottom
    #[arg(long, default_value = "lr")]
    pub direction: Direction,

    /// Data source: sql (parse SQL files directly, default) or manifest (use manifest.json from dbt compile)
    #[arg(long, default_value = "sql")]
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// Select which fields to include in JSON node output (comma-separated). Only the specified fields are emitted; unspecified fields are omitted. Available: unique_id, label, node_type, file_path, description, materialization, tags, columns, sql_content, exposure. Default (when neither --json-fields nor --json-full is given): unique_id, label, node_type, file_path. The exposure field is an object containing label, type, url, maturity, and owner; it is non-null only for exposure nodes and must be explicitly requested via --json-fields or --json-full. Note: sql_content reads raw SQL files on disk in sql mode, or compiled_code from manifest.json in manifest mode (requires prior `dbt compile`)
    #[arg(long, value_delimiter = ',', conflicts_with = "json_full")]
    pub json_fields: Option<Vec<String>>,

    /// Shorthand for specifying all available fields in --json-fields. Cannot be combined with --json-fields
    #[arg(long, conflicts_with = "json_fields")]
    pub json_full: bool,

    /// Suppress warning messages
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

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

        /// Data source: sql (parse SQL files directly, default) or manifest (use manifest.json from dbt compile)
        #[arg(long, default_value = "sql")]
        source: SourceType,

        /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
        #[arg(long)]
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
}

#[derive(Debug, clap::Args)]
pub struct CheckManifestArgs {
    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// Output format: text (default) or json
    #[arg(short = 'o', long, default_value = "text")]
    pub output: CheckManifestOutputFormat,

    /// Suppress warning messages (exit code only)
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, clap::Args)]
pub struct SummaryArgs {
    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Directory for caching extraction results (default: <project-dir>/.dlin_cache)
    #[arg(long, env = "DLIN_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Disable extraction cache (always re-parse all files, results are not saved)
    #[arg(long, env = "DLIN_NO_CACHE")]
    pub no_cache: bool,

    /// Discard existing cache and rebuild from scratch
    #[arg(long, env = "DLIN_REFRESH_CACHE", conflicts_with = "no_cache")]
    pub refresh_cache: bool,

    /// Output format: text (default) or json
    #[arg(short = 'o', long, default_value = "text")]
    pub output: SummaryOutputFormat,

    /// Data source: sql (parse SQL files directly, default) or manifest (use manifest.json from dbt compile)
    #[arg(long, default_value = "sql")]
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// Suppress warning messages
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum SummaryOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum CheckManifestOutputFormat {
    Text,
    Json,
}

#[derive(Debug, clap::Args)]
#[command(
    long_about = "\
List nodes in the dbt project (no edges, no depth traversal).

A lightweight alternative to `graph` for enumerating nodes. \
Outputs one node per line (plain) or a JSON array (json). \
Useful for getting a quick inventory of models, sources, etc.

When model names are given as positional arguments or via stdin, only those nodes \
are listed. Without arguments, all nodes are listed.

Plain output: one node name per line, sorted alphabetically.
JSON output: array of objects with unique_id, label, node_type, and metadata.

Stdin/pipe support:
  Accepts model names or file paths on stdin (one per line). \
File paths are resolved to model names using dbt project configuration.",
    after_long_help = "\
Examples:
  # List all models and sources
  dlin list

  # List specific models
  dlin list orders stg_orders

  # List as JSON for programmatic use
  dlin list -o json

  # List only source nodes
  dlin list --node-type source

  # List models tagged 'finance'
  dlin list -s tag:finance

  # Count models (combine with standard tools)
  dlin list --node-type model | wc -l

  # Pipeline: get impacted models, then fetch their SQL
  dlin impact orders -o json | jq -r '.[].impacted_nodes[].unique_id' | dlin list -o json --json-fields unique_id,sql_content

  # List models from changed files
  git diff --name-only main | dlin list -o json --json-fields unique_id,label"
)]
pub struct ListArgs {
    /// Model names to list (lists all nodes if omitted)
    pub model: Vec<String>,

    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Directory for caching extraction results (default: <project-dir>/.dlin_cache)
    #[arg(long, env = "DLIN_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Disable extraction cache (always re-parse all files, results are not saved)
    #[arg(long, env = "DLIN_NO_CACHE")]
    pub no_cache: bool,

    /// Discard existing cache and rebuild from scratch
    #[arg(long, env = "DLIN_REFRESH_CACHE", conflicts_with = "no_cache")]
    pub refresh_cache: bool,

    /// Output format: plain (default) or json
    #[arg(short = 'o', long, default_value = "plain")]
    pub output: ListOutputFormat,

    /// Selector expression (comma-separated, OR logic).
    /// All selectors support glob patterns (*, **, ?, []):
    ///   tag:<pattern>     match nodes by tag
    ///   path:<pattern>    match by file path (prefix or glob)
    ///   <pattern>         match by model label
    #[arg(short = 's', long)]
    pub select: Option<String>,

    /// Filter output by node type (comma-separated). Default: all types.
    /// Available types: model, source, seed, snapshot, test, exposure.
    ///
    /// WARNING: In sql mode, only singular tests (SQL files in tests/) are detected.
    /// YAML-defined generic tests (not_null, unique, relationships, etc.) are NOT detected.
    /// Use --source manifest for full test coverage.
    #[arg(long = "node-type", value_delimiter = ',')]
    pub node_types: Option<Vec<String>>,

    /// Data source: sql (parse SQL files directly, default) or manifest (use manifest.json from dbt compile)
    #[arg(long, default_value = "sql")]
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// Select which fields to include in JSON node output (comma-separated). Only the specified fields are emitted; unspecified fields are omitted. Available: unique_id, label, node_type, file_path, description, materialization, tags, columns, sql_content, exposure. Default (when neither --json-fields nor --json-full is given): unique_id, label, node_type, file_path. The exposure field is an object containing label, type, url, maturity, and owner; it is non-null only for exposure nodes and must be explicitly requested via --json-fields or --json-full. Note: sql_content reads raw SQL files on disk in sql mode, or compiled_code from manifest.json in manifest mode (requires prior `dbt compile`)
    #[arg(long, value_delimiter = ',', conflicts_with = "json_full")]
    pub json_fields: Option<Vec<String>>,

    /// Shorthand for specifying all available fields in --json-fields. Cannot be combined with --json-fields
    #[arg(long, conflicts_with = "json_fields")]
    pub json_full: bool,

    /// Suppress warning messages
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum OutputFormat {
    Ascii,
    Dot,
    Json,
    Mermaid,
    Plain,
    Svg,
    Html,
}

impl OutputFormat {
    /// Return the lowercase label for this output format.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ascii => "ascii",
            Self::Dot => "dot",
            Self::Json => "json",
            Self::Mermaid => "mermaid",
            Self::Plain => "plain",
            Self::Svg => "svg",
            Self::Html => "html",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum SourceType {
    /// Parse SQL files directly — no dbt/Python required. Exposures are detected from YAML, but YAML-defined generic tests (not_null, unique, relationships, etc.) are NOT detected — only singular test SQL files are found
    Sql,
    /// Use dbt manifest.json — full accuracy, requires prior `dbt compile`
    Manifest,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ImpactOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ListOutputFormat {
    Plain,
    Json,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_no_subcommand_shows_help() {
        // With no subcommand, clap should error (which triggers help display)
        let result = Cli::try_parse_from(["dlin"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_version_flag() {
        let result = Cli::try_parse_from(["dlin", "--version"]);
        // clap exits with an error (DisplayVersion) when --version is passed
        let err = result.unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    fn unwrap_graph(cli: Cli) -> GraphArgs {
        match cli.command {
            Command::Graph(args) => args,
            _ => panic!("Expected Graph subcommand"),
        }
    }

    #[test]
    fn test_graph_default_args() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert!(args.model.is_empty());
        assert!(args.upstream.is_none());
        assert!(args.downstream.is_none());
        assert!(args.select.is_none());
        assert!(args.node_types.is_none());
        assert_eq!(args.source, SourceType::Sql);
        assert!(args.manifest_path.is_none());
        assert!(matches!(args.output, OutputFormat::Ascii));
        assert!(!args.quiet);
    }

    #[test]
    fn test_graph_quiet_flag() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph", "-q"]).unwrap());
        assert!(args.quiet);
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--quiet"]).unwrap());
        assert!(args.quiet);
    }

    #[test]
    fn test_impact_quiet_flag() {
        let cli = Cli::try_parse_from(["dlin", "impact", "orders", "-q"]).unwrap();
        match cli.command {
            Command::Impact { quiet, .. } => assert!(quiet),
            _ => panic!("Expected Impact subcommand"),
        }
    }

    #[test]
    fn test_graph_all_flags() {
        let args = unwrap_graph(
            Cli::try_parse_from([
                "dlin",
                "graph",
                "my_model",
                "-p",
                "/path/to/project",
                "-u",
                "2",
                "-d",
                "3",
                "-o",
                "dot",
                "--node-type",
                "model,source,test,seed,snapshot,exposure",
                "--select",
                "tag:nightly,path:models/staging",
            ])
            .unwrap(),
        );
        assert_eq!(args.model, vec!["my_model"]);
        assert_eq!(args.project_dir, PathBuf::from("/path/to/project"));
        assert_eq!(args.upstream, Some(2));
        assert_eq!(args.downstream, Some(3));
        assert!(matches!(args.output, OutputFormat::Dot));
        assert_eq!(
            args.node_types,
            Some(vec![
                "model".to_string(),
                "source".to_string(),
                "test".to_string(),
                "seed".to_string(),
                "snapshot".to_string(),
                "exposure".to_string(),
            ])
        );
        assert_eq!(
            args.select.as_deref(),
            Some("tag:nightly,path:models/staging")
        );
    }

    #[test]
    fn test_graph_multiple_models() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "stg_orders", "raw.orders", "-u", "0"]).unwrap(),
        );
        assert_eq!(args.model, vec!["stg_orders", "raw.orders"]);
        assert_eq!(args.upstream, Some(0));
    }

    #[test]
    fn test_graph_select_short_flag() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "-s", "orders,tag:nightly"]).unwrap(),
        );
        assert_eq!(args.select.as_deref(), Some("orders,tag:nightly"));
    }

    #[test]
    fn test_graph_select_long_flag() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "--select", "path:models/staging"]).unwrap(),
        );
        assert_eq!(args.select.as_deref(), Some("path:models/staging"));
    }

    #[test]
    fn test_graph_json_fields() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "--json-fields", "unique_id,label"]).unwrap(),
        );
        assert_eq!(
            args.json_fields,
            Some(vec!["unique_id".to_string(), "label".to_string()])
        );
        assert!(!args.json_full);
    }

    #[test]
    fn test_graph_json_full() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--json-full"]).unwrap());
        assert!(args.json_full);
        assert!(args.json_fields.is_none());
    }

    #[test]
    fn test_graph_json_fields_and_full_conflict() {
        let result =
            Cli::try_parse_from(["dlin", "graph", "--json-fields", "unique_id", "--json-full"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_graph_json_fields_default_none() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert!(args.json_fields.is_none());
        assert!(!args.json_full);
    }

    #[test]
    fn test_graph_source_default_is_sql() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert_eq!(args.source, SourceType::Sql);
        assert!(args.manifest_path.is_none());
    }

    #[test]
    fn test_graph_source_manifest_with_path() {
        let args = unwrap_graph(
            Cli::try_parse_from([
                "dlin",
                "graph",
                "--source",
                "manifest",
                "--manifest-path",
                "/path/to/manifest.json",
            ])
            .unwrap(),
        );
        assert_eq!(args.source, SourceType::Manifest);
        assert_eq!(
            args.manifest_path,
            Some(PathBuf::from("/path/to/manifest.json"))
        );
    }

    #[test]
    fn test_graph_source_manifest_directory() {
        let args = unwrap_graph(
            Cli::try_parse_from([
                "dlin",
                "graph",
                "--source",
                "manifest",
                "--manifest-path",
                "/path/to/project",
            ])
            .unwrap(),
        );
        assert_eq!(args.source, SourceType::Manifest);
        assert_eq!(args.manifest_path, Some(PathBuf::from("/path/to/project")));
    }

    #[test]
    fn test_graph_output_formats() {
        for (fmt, expected) in [
            ("ascii", "Ascii"),
            ("dot", "Dot"),
            ("json", "Json"),
            ("mermaid", "Mermaid"),
            ("plain", "Plain"),
            ("svg", "Svg"),
            ("html", "Html"),
        ] {
            let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph", "-o", fmt]).unwrap());
            assert_eq!(format!("{:?}", args.output), expected);
        }

        // Invalid format
        let result = Cli::try_parse_from(["dlin", "graph", "-o", "yaml"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_impact_subcommand() {
        let cli =
            Cli::try_parse_from(["dlin", "impact", "orders", "-p", "/path/to/project"]).unwrap();
        match cli.command {
            Command::Impact {
                ref model,
                ref project_dir,
                ..
            } => {
                assert_eq!(model, &["orders"]);
                assert_eq!(project_dir, &PathBuf::from("/path/to/project"));
            }
            _ => panic!("Expected Impact subcommand"),
        }
    }

    #[test]
    fn test_impact_subcommand_json() {
        let cli = Cli::try_parse_from(["dlin", "impact", "orders", "-o", "json"]).unwrap();
        match cli.command {
            Command::Impact { ref output, .. } => {
                assert!(matches!(output, ImpactOutputFormat::Json));
            }
            _ => panic!("Expected Impact subcommand"),
        }
    }

    #[test]
    fn test_impact_multiple_models() {
        let cli = Cli::try_parse_from([
            "dlin",
            "impact",
            "orders",
            "stg_orders",
            "-p",
            "/path/to/project",
        ])
        .unwrap();
        match cli.command {
            Command::Impact {
                ref model,
                ref project_dir,
                ..
            } => {
                assert_eq!(model, &["orders", "stg_orders"]);
                assert_eq!(project_dir, &PathBuf::from("/path/to/project"));
            }
            _ => panic!("Expected Impact subcommand"),
        }
    }

    #[test]
    fn test_impact_show_sql_removed() {
        // --show-sql was removed; verify it's no longer accepted
        let result = Cli::try_parse_from(["dlin", "impact", "orders", "--show-sql"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_impact_no_model_parses_ok() {
        // No positional args is allowed at parse time (stdin may provide models at runtime)
        let cli = Cli::try_parse_from(["dlin", "impact"]).unwrap();
        match cli.command {
            Command::Impact { model, .. } => assert!(model.is_empty()),
            _ => panic!("expected Impact command"),
        }
    }

    #[test]
    fn test_graph_node_type_single() {
        let args =
            unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--node-type", "model"]).unwrap());
        assert_eq!(args.node_types, Some(vec!["model".to_string()]));
    }

    #[test]
    fn test_graph_node_type_multiple() {
        let args = unwrap_graph(
            Cli::try_parse_from(["dlin", "graph", "--node-type", "model,source"]).unwrap(),
        );
        assert_eq!(
            args.node_types,
            Some(vec!["model".to_string(), "source".to_string()])
        );
    }

    #[test]
    fn test_graph_node_type_default_none() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert!(args.node_types.is_none());
    }

    // -- List subcommand tests ------------------------------------------------

    fn unwrap_list(cli: Cli) -> ListArgs {
        match cli.command {
            Command::List(args) => args,
            _ => panic!("Expected List subcommand"),
        }
    }

    #[test]
    fn test_list_default_args() {
        let args = unwrap_list(Cli::try_parse_from(["dlin", "list"]).unwrap());
        assert!(args.model.is_empty());
        assert!(matches!(args.output, ListOutputFormat::Plain));
        assert!(args.select.is_none());
        assert!(args.node_types.is_none());
        assert_eq!(args.source, SourceType::Sql);
        assert!(args.manifest_path.is_none());
        assert!(!args.quiet);
    }

    #[test]
    fn test_list_with_models() {
        let args =
            unwrap_list(Cli::try_parse_from(["dlin", "list", "orders", "stg_orders"]).unwrap());
        assert_eq!(args.model, vec!["orders", "stg_orders"]);
    }

    #[test]
    fn test_list_with_models_and_flags() {
        let args = unwrap_list(
            Cli::try_parse_from([
                "dlin",
                "list",
                "orders",
                "-o",
                "json",
                "--json-fields",
                "unique_id,sql_content",
            ])
            .unwrap(),
        );
        assert_eq!(args.model, vec!["orders"]);
        assert!(matches!(args.output, ListOutputFormat::Json));
        assert_eq!(
            args.json_fields,
            Some(vec!["unique_id".to_string(), "sql_content".to_string()])
        );
    }

    #[test]
    fn test_list_json_output() {
        let args = unwrap_list(Cli::try_parse_from(["dlin", "list", "-o", "json"]).unwrap());
        assert!(matches!(args.output, ListOutputFormat::Json));
    }

    #[test]
    fn test_list_with_filters() {
        let args = unwrap_list(
            Cli::try_parse_from([
                "dlin",
                "list",
                "--node-type",
                "model,source,test",
                "-s",
                "tag:nightly",
                "-q",
            ])
            .unwrap(),
        );
        assert_eq!(
            args.node_types,
            Some(vec![
                "model".to_string(),
                "source".to_string(),
                "test".to_string()
            ])
        );
        assert_eq!(args.select.as_deref(), Some("tag:nightly"));
        assert!(args.quiet);
    }

    #[test]
    fn test_list_invalid_output_format() {
        let result = Cli::try_parse_from(["dlin", "list", "-o", "dot"]);
        assert!(result.is_err());
    }

    // -- Summary subcommand tests ---------------------------------------------

    fn unwrap_summary(cli: Cli) -> SummaryArgs {
        match cli.command {
            Command::Summary(args) => args,
            _ => panic!("Expected Summary subcommand"),
        }
    }

    #[test]
    fn test_summary_default_args() {
        let args = unwrap_summary(Cli::try_parse_from(["dlin", "summary"]).unwrap());
        assert!(matches!(args.output, SummaryOutputFormat::Text));
        assert_eq!(args.source, SourceType::Sql);
        assert!(args.manifest_path.is_none());
        assert!(!args.quiet);
        assert!(!args.no_cache);
    }

    #[test]
    fn test_summary_json_output() {
        let args = unwrap_summary(Cli::try_parse_from(["dlin", "summary", "-o", "json"]).unwrap());
        assert!(matches!(args.output, SummaryOutputFormat::Json));
    }

    #[test]
    fn test_summary_with_manifest() {
        let args = unwrap_summary(
            Cli::try_parse_from([
                "dlin",
                "summary",
                "--source",
                "manifest",
                "--manifest-path",
                "/path/to/manifest.json",
            ])
            .unwrap(),
        );
        assert_eq!(args.source, SourceType::Manifest);
        assert_eq!(
            args.manifest_path,
            Some(PathBuf::from("/path/to/manifest.json"))
        );
    }

    #[test]
    fn test_summary_quiet_flag() {
        let args = unwrap_summary(Cli::try_parse_from(["dlin", "summary", "-q"]).unwrap());
        assert!(args.quiet);
    }

    #[test]
    fn test_summary_invalid_output_format() {
        let result = Cli::try_parse_from(["dlin", "summary", "-o", "dot"]);
        assert!(result.is_err());
    }

    // -- Error format tests ---------------------------------------------------

    #[test]
    fn test_error_format_default() {
        let cli = Cli::try_parse_from(["dlin", "graph"]).unwrap();
        assert_eq!(cli.error_format, ErrorFormat::Text);
    }

    #[test]
    fn test_error_format_json() {
        let cli = Cli::try_parse_from(["dlin", "--error-format", "json", "graph"]).unwrap();
        assert_eq!(cli.error_format, ErrorFormat::Json);
    }

    #[test]
    fn test_error_format_json_after_subcommand() {
        // global flags work after subcommand too
        let cli = Cli::try_parse_from(["dlin", "graph", "--error-format", "json"]).unwrap();
        assert_eq!(cli.error_format, ErrorFormat::Json);
    }

    #[test]
    fn test_error_format_invalid() {
        let result = Cli::try_parse_from(["dlin", "--error-format", "xml", "graph"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_format_with_impact() {
        let cli =
            Cli::try_parse_from(["dlin", "--error-format", "json", "impact", "orders"]).unwrap();
        assert_eq!(cli.error_format, ErrorFormat::Json);
    }
}
