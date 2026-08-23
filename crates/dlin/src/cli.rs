use clap::{Parser, Subcommand};
use dlin_core::graph::column_lineage::DlinDialect;
use std::path::PathBuf;

/// Parse a `--dialect` value via `DlinDialect`'s `FromStr` implementation.
///
/// `DlinDialect` derives `clap::ValueEnum` (for its per-variant aliases), but
/// clap's automatic parser selection would then list every dialect spelling
/// as a "Possible values" block in `--help`, which is not how this flag has
/// ever been documented (its accepted spellings are described in prose in
/// each command's own help text instead). Pinning the parser to `FromStr`
/// keeps the flag's parsing behavior — and `--help` output — unchanged.
fn parse_dialect(s: &str) -> Result<DlinDialect, String> {
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

fn parse_dialect_arg(s: &str) -> Result<DialectArg, String> {
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

Column display (--show-columns):
  Include column names inside node labels (currently mermaid only). \
In sql mode, columns are taken from YAML properties files when available, \
falling back to parsing SELECT clauses. In manifest mode, columns come \
from manifest metadata. Combines well with --collapse to show rich \
detail on fewer endpoint nodes.

Stdin/pipe support:
  Accepts model names or file paths on stdin (one per line). \
File paths are resolved to model names using dbt project configuration.

Column-level analysis:
  This command works at the model level (bidirectional, with -u/-d depth control).
  For column-level lineage tracing, see:
    dlin column upstream    — traces where each output column's data came from
    dlin column downstream  — finds which outputs depend on a given column
  Both require manifest.json (run `dbt compile` first).",
    after_long_help = "\
Examples:
  # === Full project lineage ===
  dlin graph
  dlin graph -o json

  # === Focus on specific models (positional args = BFS from those nodes) ===
  dlin graph orders -u 2 -d 1            # 2 upstream, 1 downstream of orders
  dlin graph stg_orders -d 0             # just the node, no downstream
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
  dlin graph -o mermaid --direction tb   # top-to-bottom layout

  # === Collapse (simplify by removing intermediate nodes) ===
  dlin graph --collapse                              # keep only graph endpoints
  dlin graph orders --collapse                       # endpoints + orders preserved
  dlin graph orders --collapse=focal -u 3            # sources + exposures + orders only

  # === Column display (mermaid only) ===
  dlin graph -o mermaid --show-columns               # show columns in node labels
  dlin graph -o mermaid --collapse --show-columns    # rich detail on fewer nodes

  # === Column-level analysis (requires manifest) ===
  dlin column upstream orders                             # upstream: where do columns come from?
  dlin column downstream stg_orders --column order_id     # downstream: what depends on this column?"
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

    /// Selector expression (comma-separated, OR logic)
    #[arg(
        short = 's',
        long,
        long_help = "\
Selector expression (comma-separated, OR logic).
All selectors support glob patterns (*, **, ?, []):

  tag:<pattern>     match nodes by tag
  path:<pattern>    match by file path (prefix or glob)
  <pattern>         match by model label"
    )]
    pub select: Option<String>,

    /// Filter output by node type (comma-separated)
    #[arg(
        long = "node-type",
        value_delimiter = ',',
        long_help = "\
Filter output by node type (comma-separated). Default: all types.
Available types: model, source, seed, snapshot, test, exposure, semantic_model, metric, saved_query.

NOTE: In sql mode, generic tests are inferred from YAML declarations
with dlin-specific IDs. Use --source manifest for exact dependency resolution."
    )]
    pub node_types: Option<Vec<String>>,

    /// Disable transitive edge completion when filters remove intermediate nodes
    #[arg(
        long,
        long_help = "\
Disable transitive edge completion when filters remove intermediate nodes.
By default, when --node-type, --select, or focus models exclude nodes,
edges through removed nodes are preserved as transitive edges with \"(via N)\" labels."
    )]
    pub no_transitive: bool,

    /// Collapse intermediate nodes, replacing them with transitive edges
    #[arg(long, value_name = "MODE", default_missing_value = "endpoints", num_args = 0..=1, require_equals = true, long_help = "\
Collapse intermediate nodes, replacing them with transitive edges
shown as \"(via N)\" in DOT/Mermaid output.

Without a value (--collapse), defaults to \"endpoints\" mode: keeps
nodes with no predecessors or no successors, plus focus models.

With --collapse=focal: keeps only source/exposure nodes and focus
models as endpoints. Endpoint selection ignores BFS window boundaries
(-u/-d), so window-edge nodes are not treated as pseudo-endpoints,
but traversal still respects the -u/-d limits.

Focus models are preserved even if they would otherwise be intermediate,
as long as they are not removed earlier by filters like --select or
--node-type.

Ignored when --no-transitive is set.")]
    pub collapse: Option<CollapseMode>,

    /// Group nodes in supported formats (dot, mermaid)
    #[arg(
        long = "group-by",
        long_help = "\
Group nodes using subgraph/cluster blocks in supported formats (dot, mermaid).

Supported values:
  node-type    group by source, model, test, etc.
  directory    group by file directory path"
    )]
    pub group_by: Option<GroupBy>,

    /// Show column names inside node labels (currently mermaid only)
    #[arg(
        long,
        long_help = "\
Show column names inside node labels (currently mermaid only).
In sql mode, columns are taken from YAML properties files when
available, falling back to parsing SELECT clauses.
In manifest mode, columns come from manifest metadata.
Combines well with --collapse to show rich detail on fewer nodes."
    )]
    pub show_columns: bool,

    /// Graph direction for dot and mermaid output.
    /// LR = left to right (default), TB = top to bottom
    #[arg(long, default_value = "lr")]
    pub direction: Direction,

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
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(
        long,
        long_help = "\
Path to manifest.json file or directory containing target/manifest.json.

Default: <project-dir>/target/manifest.json"
    )]
    pub manifest_path: Option<PathBuf>,

    /// Select which fields to include in JSON node output (comma-separated)
    #[arg(
        long,
        value_delimiter = ',',
        conflicts_with = "json_full",
        long_help = "\
Select which fields to include in JSON node output (comma-separated).
Only the specified fields are emitted; unspecified fields are omitted.

Available fields:
  unique_id, label, node_type, file_path, description,
  materialization, tags, columns, sql_content, exposure

Default (when neither --json-fields nor --json-full is given):
  unique_id, label, node_type, file_path

The exposure field is an object containing label, type, url, maturity,
and owner; it is non-null only for exposure nodes and must be explicitly
requested via --json-fields or --json-full.

Note: sql_content reads raw SQL files on disk in sql mode, or
compiled_code from manifest.json in manifest mode (requires prior
`dbt compile`)."
    )]
    pub json_fields: Option<Vec<String>>,

    /// Include all available fields in JSON output
    #[arg(
        long,
        conflicts_with = "json_fields",
        long_help = "\
Shorthand for specifying all available fields in --json-fields.
Cannot be combined with --json-fields."
    )]
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

#[derive(Debug, clap::Args)]
pub struct McpArgs {
    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// SQL dialect for parsing compiled SQL.
    /// Auto-detected from manifest.metadata.adapter_type when omitted. Recognized dialects
    /// removed from the active backend fall back to Generic with a warning.
    #[arg(
        long,
        value_parser = parse_dialect_arg,
        long_help = "\
SQL dialect for parsing compiled SQL in get_column_lineage.

When omitted, the dialect is auto-detected from manifest.metadata.adapter_type.
Recognized dialects removed from the active backend fall back to Generic with a warning;
a missing, empty, or unknown adapter_type is an error."
    )]
    pub dialect: Option<DialectArg>,
}

#[derive(Debug, clap::Args)]
pub struct CheckManifestArgs {
    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(
        long,
        long_help = "\
Path to manifest.json file or directory containing target/manifest.json.

Default: <project-dir>/target/manifest.json"
    )]
    pub manifest_path: Option<PathBuf>,

    /// Output format: text (default) or json
    #[arg(short = 'o', long, default_value = "text")]
    pub output: CheckManifestOutputFormat,

    /// Suppress warning messages (exit code only)
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, clap::Args)]
pub struct DebugArgs {
    #[command(subcommand)]
    pub command: DebugCommand,
}

#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// Parse SQL and display the AST (debug or JSON)
    #[command(
        name = "parse-sql",
        long_about = "\
Parse a SQL statement using the production SQL parser and display the result.

By default, shows the Rust Debug representation of the AST. \
Use --format to choose between AST debug output or JSON AST.

This does not require a dbt project — it operates on raw SQL strings.",
        after_long_help = "\
Examples:
  # Show AST debug representation (default)
  dlin debug parse-sql 'SELECT a, b FROM t'

  # Show AST as JSON
  dlin debug parse-sql 'SELECT a FROM t' --format json

  # Parse with BigQuery dialect
  dlin debug parse-sql 'SELECT CAST(x AS ARRAY<STRING>) FROM t' --dialect bigquery

  # Parse from file via stdin
  dlin debug parse-sql --dialect snowflake < compiled_query.sql"
    )]
    ParseSql(DebugParseSqlArgs),

    /// Trace a single column's lineage through a SQL statement
    #[command(
        name = "trace-column",
        long_about = "\
Trace a single column's upstream lineage through a SQL statement.

Uses the production sqllineage engine to find where a column comes from. \
Optionally provide table schema information for more accurate resolution \
(especially needed for SELECT * expansion).

This does not require a dbt project — it operates on raw SQL strings.",
        after_long_help = "\
Examples:
  # Basic column trace
  dlin debug trace-column 'SELECT t.id AS order_id FROM t' --column order_id

  # With schema (table:col1,col2 format, semicolon-separated tables)
  dlin debug trace-column \\
    'SELECT * FROM orders JOIN customers ON orders.cid = customers.id' \\
    --column cid \\
    --schema 'orders:id,cid,amount;customers:id,name'

  # With explicit dialect
  dlin debug trace-column 'SELECT a FROM t' --column a --dialect bigquery

  # From file via stdin
  dlin debug trace-column --column order_id --dialect snowflake < query.sql"
    )]
    TraceColumn(DebugTraceColumnArgs),
}

#[derive(Debug, clap::Args)]
pub struct DebugParseSqlArgs {
    /// SQL string to parse (reads from stdin if omitted)
    pub sql: Option<String>,

    /// SQL dialect for parsing (default: generic)
    /// Recognized dialects removed from the active backend fall back to Generic with a warning.
    #[arg(long, default_value = "generic", value_parser = parse_dialect_arg)]
    pub dialect: DialectArg,

    /// Output format: ast (Debug representation), json (JSON AST)
    #[arg(long, default_value = "ast")]
    pub format: DebugOutputFormat,
}

#[derive(Debug, clap::Args)]
pub struct DebugTraceColumnArgs {
    /// SQL string to parse (reads from stdin if omitted)
    pub sql: Option<String>,

    /// Column name to trace
    #[arg(long)]
    pub column: String,

    /// SQL dialect for parsing (default: generic)
    /// Recognized dialects removed from the active backend fall back to Generic with a warning.
    #[arg(long, default_value = "generic", value_parser = parse_dialect_arg)]
    pub dialect: DialectArg,

    /// Table schema definitions for accurate lineage resolution.
    /// Format: table1:col1,col2;table2:col3,col4
    #[arg(long)]
    pub schema: Option<String>,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum DebugOutputFormat {
    /// Rust Debug representation of the AST
    Ast,
    /// JSON serialization of the AST
    Json,
}

/// Arguments for `dlin column` subcommand group
#[derive(Debug, clap::Args)]
pub struct ColumnArgs {
    #[command(subcommand)]
    pub command: ColumnCommand,
}

#[derive(Debug, Subcommand)]
pub enum ColumnCommand {
    /// Compute column-level lineage for a model (traces upstream sources)
    #[command(
        long_about = "\
Compute column-level lineage for one or more models.

Direction: upstream only. Traces backward from a model's output columns to
their raw source columns across the full DAG. There are no -u/-d depth flags
— the entire upstream chain is always traversed automatically.

To find what downstream models or columns would be affected by changing a
specific column, use `dlin column downstream` instead.

There is no --source flag; manifest.json is the only data source (unlike
`dlin graph` which supports both SQL files and manifest).

Requires manifest.json with compiled SQL (run `dbt compile` first).
Use `dlin check-manifest` to verify freshness before querying.

Column resolution order:
  1. YAML column definitions (schema.yml / models.yml)
  2. SQL inference from compiled_code (fallback when YAML is absent)

Stdin/pipe support:
  Accepts model names or file paths on stdin (one per line).
  File paths (detected by extension or path separator) are resolved to
  model names using the dbt project configuration.

Output format (-o/--output):
  json (default)  JSON array per model with the following structure:
    model             model name
    traced_columns    number of columns successfully traced
    total_columns     total number of columns attempted
    columns[]
      column          output column name
      transformation  how the column was derived:
                        direct       passed through unchanged (including renames)
                        aggregation  aggregate function (SUM, COUNT, etc.)
                        expression   arithmetic or other expression
                        cast         type cast (CAST(x AS INT))
                        conditional  CASE WHEN expression
                        unknown      could not classify
      sources[]
        table         source model or raw table name; empty string (\"\") when the
                      value originates from a literal (NULL, constant, UNNEST, etc.)
                      — rendered as \"(literal)\" in plain/mermaid/dot outputs
        column        source column name
        model_path[]  intermediate [model, column, transformation] triples traversed (omitted if empty)
    errors[]    parse or resolution errors (non-empty → exit code 1)
  plain           human-readable text, one model per block
  mermaid         Mermaid flowchart (LR) with subgraphs per model
  dot             Graphviz DOT format; models as clusters, columns as nodes
                  color-coded by transformation type (pipe to `dot -Tsvg`)

Exit codes:
  0   Success
  1   Error (model not found, no manifest, analysis errors, etc.)",
        after_long_help = "\
Examples:
  # Column lineage for a single model (JSON output)
  dlin column upstream orders

  # Human-readable plain output
  dlin column upstream orders -o plain

  # Mermaid flowchart
  dlin column upstream orders -o mermaid

  # Specific columns only
  dlin column upstream orders --column order_id --column status

  # Multiple models
  dlin column upstream orders stg_orders

  # With explicit manifest path
  dlin column upstream orders --manifest-path target/manifest.json

  # BigQuery project
  dlin column upstream orders --dialect bigquery

  # From git diff (pipe changed files)
  git diff --name-only main | dlin column upstream -o json"
    )]
    Upstream(ColumnGraphArgs),

    /// Analyze downstream column-level impact of changing a column
    #[command(
        long_about = "\
Analyze downstream column-level impact of changing a column.

Direction: downstream only. Starting from a specific column, follows forward
edges to find all dependent models and columns. There are no -u/-d depth flags
— all downstream dependents are always included.

This is the reverse direction of `dlin column upstream` (which traces upstream
sources). To trace where a column's data comes from, use `dlin column upstream`.

Takes a single model and one or more --column flags (required).

There is no --source flag; manifest.json is the only data source (unlike
`dlin graph`/`dlin impact` which support both SQL files and manifest).

Requires compiled SQL in manifest.json — run `dbt compile` first.
Use `dlin check-manifest` to verify freshness before querying.

Output format (-o/--output):
  json (default)  JSON array per column with affected downstream columns and models
  plain           human-readable text, one source column per block
  mermaid         Mermaid flowchart (LR) showing impacted columns across models
  dot             Graphviz DOT format; models as clusters, columns as nodes
                  color-coded by transformation type (pipe to `dot -Tsvg`)

Exit codes:
  0   Success
  1   Error (model not found, no manifest, analysis errors, etc.)",
        after_long_help = "\
Examples:
  # Impact of changing a single column (JSON output)
  dlin column downstream stg_orders --column order_id

  # Human-readable plain output
  dlin column downstream stg_orders --column order_id -o plain

  # Mermaid flowchart
  dlin column downstream stg_orders --column order_id -o mermaid

  # Impact of multiple columns
  dlin column downstream stg_orders --column order_id --column status

  # With explicit manifest path
  dlin column downstream stg_orders --column order_id --manifest-path target/manifest.json

  # BigQuery project
  dlin column downstream stg_orders --column order_id --dialect bigquery"
    )]
    Downstream(ColumnImpactArgs),
}

#[derive(Debug, clap::Args)]
pub struct ColumnGraphArgs {
    /// Model names or file paths to analyze column lineage for (also accepts stdin)
    pub model: Vec<String>,

    /// Specific columns to analyze (analyzes all columns if omitted)
    #[arg(long)]
    pub column: Vec<String>,

    /// Output format: json (default), plain, mermaid
    #[arg(short = 'o', long, default_value = "json")]
    pub output: ColumnOutputFormat,

    /// SQL dialect for parsing compiled SQL.
    /// Auto-detected from manifest.metadata.adapter_type when omitted. Recognized dialects
    /// removed from the active backend fall back to Generic with a warning.
    #[arg(long, value_parser = parse_dialect_arg)]
    pub dialect: Option<DialectArg>,

    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// Directory for caching column lineage results (default: <project-dir>/.dlin_cache)
    #[arg(long, env = "DLIN_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Disable column lineage cache
    #[arg(long, env = "DLIN_NO_CACHE")]
    pub no_cache: bool,

    /// Discard existing cache and rebuild from scratch
    #[arg(long, env = "DLIN_REFRESH_CACHE", conflicts_with = "no_cache")]
    pub refresh_cache: bool,

    /// Suppress warning messages
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

#[derive(Debug, clap::Args)]
pub struct ColumnImpactArgs {
    /// Model name to analyze column downstream impact for
    pub model: String,

    /// Columns to analyze impact for (required)
    #[arg(long, required = true)]
    pub column: Vec<String>,

    /// Output format: json (default), plain, mermaid
    #[arg(short = 'o', long, default_value = "json")]
    pub output: ColumnOutputFormat,

    /// SQL dialect for parsing compiled SQL.
    /// Auto-detected from manifest.metadata.adapter_type when omitted. Recognized dialects
    /// removed from the active backend fall back to Generic with a warning.
    #[arg(long, value_parser = parse_dialect_arg)]
    pub dialect: Option<DialectArg>,

    /// Path to dbt project directory
    #[arg(short = 'p', long = "project-dir", default_value = ".")]
    pub project_dir: PathBuf,

    /// Path to manifest.json file or directory containing target/manifest.json (default: <project-dir>/target/manifest.json)
    #[arg(long)]
    pub manifest_path: Option<PathBuf>,

    /// Directory for caching column lineage results (default: <project-dir>/.dlin_cache)
    #[arg(long, env = "DLIN_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Disable column lineage cache
    #[arg(long, env = "DLIN_NO_CACHE")]
    pub no_cache: bool,

    /// Discard existing cache and rebuild from scratch
    #[arg(long, env = "DLIN_REFRESH_CACHE", conflicts_with = "no_cache")]
    pub refresh_cache: bool,

    /// Suppress warning messages
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
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(
        long,
        long_help = "\
Path to manifest.json file or directory containing target/manifest.json.

Default: <project-dir>/target/manifest.json"
    )]
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
  Accepts model names or file paths on stdin (one per line).
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

  # Search models by name or description (regex, case-insensitive)
  dlin list --search shipping
  dlin list --search '^stg_'                     # models whose name starts with stg_
  dlin list --search 'order|payment'             # OR: name/description contains either
  dlin list --search staging --search customer   # AND: must match both patterns
  dlin list --search shipping -o json --json-full

  # Count models (combine with standard tools)
  dlin list --node-type model | wc -l

  # Pipeline: get impacted models, then fetch their SQL
  dlin impact orders -o json | jq -r '.[].impacted_nodes[].unique_id' | dlin list -o json --json-fields unique_id,sql_content

  # List models from changed files
  git diff --name-only main | dlin list -o json --json-fields unique_id,label

  # Find models that expose a specific column name (jq)
  dlin list -o json --json-full | jq '.[] | select(any(.columns[]; . == \"order_id\"))'

  # Find models whose column name partially matches (jq)
  dlin list -o json --json-full | jq '.[] | select(any(.columns[]; contains(\"_amount\")))'  "
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

    /// Selector expression (comma-separated, OR logic)
    #[arg(
        short = 's',
        long,
        long_help = "\
Selector expression (comma-separated, OR logic).
All selectors support glob patterns (*, **, ?, []):

  tag:<pattern>     match nodes by tag
  path:<pattern>    match by file path (prefix or glob)
  <pattern>         match by model label"
    )]
    pub select: Option<String>,

    /// Filter nodes by name or description using a regex pattern (case-insensitive; repeatable for AND)
    #[arg(
        long,
        value_name = "PATTERN",
        action = clap::ArgAction::Append,
        long_help = "\
Filter nodes whose name or description matches the regex pattern (case-insensitive).

Matching rules:
  - Tested against model name and description field (OR within a single pattern)
  - Plain text works as a simple substring match
  - Repeating --search applies AND logic: all patterns must match

Regex syntax (Rust regex, https://docs.rs/regex):
  order|payment   match nodes containing 'order' or 'payment'
  ^stg_           match nodes whose name starts with 'stg_'
  (?-i)Order      opt out of case-insensitivity for this pattern

Examples:
  dlin list --search shipping
  dlin list --search '^stg_'
  dlin list --search 'order|payment'
  dlin list --search staging --search customer   # AND: must match both"
    )]
    pub search: Vec<String>,

    /// Filter output by node type (comma-separated)
    #[arg(
        long = "node-type",
        value_delimiter = ',',
        long_help = "\
Filter output by node type (comma-separated). Default: all types.
Available types: model, source, seed, snapshot, test, exposure, semantic_model, metric, saved_query.

NOTE: In sql mode, generic tests are inferred from YAML declarations
with dlin-specific IDs. Use --source manifest for exact dependency resolution."
    )]
    pub node_types: Option<Vec<String>>,

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
    pub source: SourceType,

    /// Path to manifest.json file or directory containing target/manifest.json
    #[arg(
        long,
        long_help = "\
Path to manifest.json file or directory containing target/manifest.json.

Default: <project-dir>/target/manifest.json"
    )]
    pub manifest_path: Option<PathBuf>,

    /// Select which fields to include in JSON node output (comma-separated)
    #[arg(
        long,
        value_delimiter = ',',
        conflicts_with = "json_full",
        long_help = "\
Select which fields to include in JSON node output (comma-separated).
Only the specified fields are emitted; unspecified fields are omitted.

Available fields:
  unique_id, label, node_type, file_path, description,
  materialization, tags, columns, sql_content, exposure

Default (when neither --json-fields nor --json-full is given):
  unique_id, label, node_type, file_path

The exposure field is an object containing label, type, url, maturity,
and owner; it is non-null only for exposure nodes and must be explicitly
requested via --json-fields or --json-full.

Note: sql_content reads raw SQL files on disk in sql mode, or
compiled_code from manifest.json in manifest mode (requires prior
`dbt compile`)."
    )]
    pub json_fields: Option<Vec<String>>,

    /// Include all available fields in JSON output
    #[arg(
        long,
        conflicts_with = "json_fields",
        long_help = "\
Shorthand for specifying all available fields in --json-fields.
Cannot be combined with --json-fields."
    )]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SourceType {
    /// Parse SQL files directly — no dbt/Python required. Exposures and generic tests are detected from YAML with dlin-specific IDs; use manifest mode for exact test dependency resolution
    Sql,
    /// Use dbt manifest.json — full accuracy, requires prior `dbt compile`
    Manifest,
}

#[derive(Debug, Clone, clap::ValueEnum)]
pub enum ImpactOutputFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, PartialEq, Eq, clap::ValueEnum)]
pub enum ColumnOutputFormat {
    /// Machine-readable JSON array (default)
    Json,
    /// Human-readable plain-text table
    Plain,
    /// Mermaid flowchart diagram
    Mermaid,
    /// Graphviz DOT format (pipe to `dot -Tsvg > out.svg`)
    Dot,
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

    fn unwrap_column_upstream(cli: Cli) -> ColumnGraphArgs {
        match cli.command {
            Command::Column(col) => match col.command {
                ColumnCommand::Upstream(args) => args,
                _ => panic!("Expected Column upstream subcommand"),
            },
            _ => panic!("Expected Column subcommand"),
        }
    }

    fn unwrap_column_downstream(cli: Cli) -> ColumnImpactArgs {
        match cli.command {
            Command::Column(col) => match col.command {
                ColumnCommand::Downstream(args) => args,
                _ => panic!("Expected Column downstream subcommand"),
            },
            _ => panic!("Expected Column subcommand"),
        }
    }

    #[test]
    fn test_column_upstream_subcommand() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from(["dlin", "column", "upstream", "orders"]).unwrap(),
        );
        assert_eq!(args.model, &["orders"]);
        assert!(args.column.is_empty());
    }

    #[test]
    fn test_column_upstream_with_column_filter() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from([
                "dlin", "column", "upstream", "orders", "--column", "order_id", "--column",
                "status",
            ])
            .unwrap(),
        );
        assert_eq!(args.model, &["orders"]);
        assert_eq!(args.column, &["order_id", "status"]);
    }

    #[test]
    fn test_column_upstream_no_model() {
        // No positional args is allowed at parse time (stdin may provide models at runtime)
        let cli = Cli::try_parse_from(["dlin", "column", "upstream"]).unwrap();
        match cli.command {
            Command::Column(col) => match col.command {
                ColumnCommand::Upstream(args) => assert!(args.model.is_empty()),
                _ => panic!("expected Upstream"),
            },
            _ => panic!("expected Column"),
        }
    }

    #[test]
    fn test_column_downstream_subcommand() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
            ])
            .unwrap(),
        );
        assert_eq!(args.model, "stg_orders");
        assert_eq!(args.column, &["order_id"]);
    }

    #[test]
    fn test_column_downstream_requires_column() {
        // --column is required for column downstream
        let result = Cli::try_parse_from(["dlin", "column", "downstream", "stg_orders"]);
        assert!(result.is_err(), "column downstream should require --column");
    }

    #[test]
    fn test_column_downstream_multiple_columns() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
                "--column",
                "status",
            ])
            .unwrap(),
        );
        assert_eq!(args.column, &["order_id", "status"]);
    }

    #[test]
    fn test_column_upstream_with_dialect() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "upstream",
                "orders",
                "--dialect",
                "bigquery",
            ])
            .unwrap(),
        );
        assert_eq!(
            args.dialect,
            Some(DialectArg {
                dialect: DlinDialect::BigQuery,
                requested: "bigquery".to_string(),
            })
        );
    }

    #[test]
    fn test_column_upstream_default_dialect() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from(["dlin", "column", "upstream", "orders"]).unwrap(),
        );
        assert!(args.dialect.is_none());
    }

    #[test]
    fn test_column_downstream_with_dialect() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
                "--dialect",
                "snowflake",
            ])
            .unwrap(),
        );
        assert_eq!(
            args.dialect,
            Some(DialectArg {
                dialect: DlinDialect::Snowflake,
                requested: "snowflake".to_string(),
            })
        );
    }

    #[test]
    fn test_dialect_all_known_values_parse() {
        let dialects = [
            "generic",
            "postgresql",
            "postgres",
            "mysql",
            "hive",
            "databricks",
            "snowflake",
            "bigquery",
            "duckdb",
            "sqlite",
            "spark",
            "spark2",
            "trino",
            "presto",
            "redshift",
            "tsql",
            "mssql",
            "sqlserver",
            "oracle",
            "clickhouse",
            "athena",
            "teradata",
            "doris",
            "starrocks",
            "materialize",
            "risingwave",
            "singlestore",
            "memsql",
            "cockroachdb",
            "cockroach",
            "tidb",
            "druid",
            "solr",
            "tableau",
            "dune",
            "fabric",
            "drill",
            "dremio",
            "exasol",
            "datafusion",
            "arrow-datafusion",
            "arrow_datafusion",
        ];
        for dialect in dialects {
            let cli =
                Cli::try_parse_from(["dlin", "column", "upstream", "model", "--dialect", dialect]);
            assert!(
                cli.is_ok(),
                "dialect '{}' should parse successfully, got: {:?}",
                dialect,
                cli.err()
            );
        }
    }

    #[test]
    fn test_dialect_invalid_value_rejected() {
        let result = Cli::try_parse_from([
            "dlin",
            "column",
            "upstream",
            "model",
            "--dialect",
            "posgres",
        ]);
        let error = result.expect_err("unknown dialect should be rejected by clap");
        let message = error.to_string();
        assert!(message.contains("posgres"));
        assert!(message.contains("generic, postgresql, postgres"));
    }

    // -- ColumnOutputFormat tests --------------------------------------------

    #[test]
    fn test_column_upstream_default_output_is_json() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from(["dlin", "column", "upstream", "orders"]).unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Json));
    }

    #[test]
    fn test_column_upstream_output_plain() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from(["dlin", "column", "upstream", "orders", "-o", "plain"]).unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Plain));
    }

    #[test]
    fn test_column_upstream_output_mermaid() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from(["dlin", "column", "upstream", "orders", "-o", "mermaid"]).unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Mermaid));
    }

    #[test]
    fn test_column_upstream_output_dot() {
        let args = unwrap_column_upstream(
            Cli::try_parse_from(["dlin", "column", "upstream", "orders", "-o", "dot"]).unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Dot));
    }

    #[test]
    fn test_column_downstream_output_dot() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
                "-o",
                "dot",
            ])
            .unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Dot));
    }

    #[test]
    fn test_column_upstream_invalid_output_rejected() {
        let result = Cli::try_parse_from(["dlin", "column", "upstream", "orders", "-o", "ascii"]);
        assert!(result.is_err(), "ascii is not a valid column output format");
    }

    #[test]
    fn test_column_downstream_default_output_is_json() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
            ])
            .unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Json));
    }

    #[test]
    fn test_column_downstream_output_plain() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
                "-o",
                "plain",
            ])
            .unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Plain));
    }

    #[test]
    fn test_column_downstream_output_mermaid() {
        let args = unwrap_column_downstream(
            Cli::try_parse_from([
                "dlin",
                "column",
                "downstream",
                "stg_orders",
                "--column",
                "order_id",
                "--output",
                "mermaid",
            ])
            .unwrap(),
        );
        assert!(matches!(args.output, ColumnOutputFormat::Mermaid));
    }

    // -- Debug subcommand tests -----------------------------------------------

    fn unwrap_debug(cli: Cli) -> DebugArgs {
        match cli.command {
            Command::Debug(args) => args,
            _ => panic!("Expected Debug subcommand"),
        }
    }

    #[test]
    fn test_debug_parse_sql_positional_arg() {
        let args =
            unwrap_debug(Cli::try_parse_from(["dlin", "debug", "parse-sql", "SELECT 1"]).unwrap());
        match args.command {
            DebugCommand::ParseSql(ref a) => {
                assert_eq!(a.sql.as_deref(), Some("SELECT 1"));
                assert!(matches!(a.format, DebugOutputFormat::Ast));
            }
            _ => panic!("Expected ParseSql"),
        }
    }

    #[test]
    fn test_debug_parse_sql_no_arg_ok() {
        // No positional arg is allowed (stdin will be read at runtime)
        let args = unwrap_debug(Cli::try_parse_from(["dlin", "debug", "parse-sql"]).unwrap());
        match args.command {
            DebugCommand::ParseSql(ref a) => {
                assert!(a.sql.is_none());
            }
            _ => panic!("Expected ParseSql"),
        }
    }

    #[test]
    fn test_debug_parse_sql_format_ast() {
        let args = unwrap_debug(
            Cli::try_parse_from(["dlin", "debug", "parse-sql", "SELECT 1", "--format", "ast"])
                .unwrap(),
        );
        match args.command {
            DebugCommand::ParseSql(ref a) => {
                assert!(matches!(a.format, DebugOutputFormat::Ast));
            }
            _ => panic!("Expected ParseSql"),
        }
    }

    #[test]
    fn test_debug_parse_sql_format_json() {
        let args = unwrap_debug(
            Cli::try_parse_from(["dlin", "debug", "parse-sql", "SELECT 1", "--format", "json"])
                .unwrap(),
        );
        match args.command {
            DebugCommand::ParseSql(ref a) => {
                assert!(matches!(a.format, DebugOutputFormat::Json));
            }
            _ => panic!("Expected ParseSql"),
        }
    }

    #[test]
    fn test_debug_parse_sql_with_dialect() {
        let args = unwrap_debug(
            Cli::try_parse_from([
                "dlin",
                "debug",
                "parse-sql",
                "SELECT 1",
                "--dialect",
                "bigquery",
            ])
            .unwrap(),
        );
        match args.command {
            DebugCommand::ParseSql(ref a) => {
                assert_eq!(a.dialect, DlinDialect::BigQuery);
            }
            _ => panic!("Expected ParseSql"),
        }
    }

    #[test]
    fn test_debug_parse_sql_default_dialect_is_generic() {
        let args =
            unwrap_debug(Cli::try_parse_from(["dlin", "debug", "parse-sql", "SELECT 1"]).unwrap());
        match args.command {
            DebugCommand::ParseSql(ref a) => {
                assert_eq!(a.dialect, DlinDialect::Generic);
            }
            _ => panic!("Expected ParseSql"),
        }
    }

    #[test]
    fn test_debug_trace_column_basic() {
        let args = unwrap_debug(
            Cli::try_parse_from([
                "dlin",
                "debug",
                "trace-column",
                "SELECT a FROM t",
                "--column",
                "a",
            ])
            .unwrap(),
        );
        match args.command {
            DebugCommand::TraceColumn(ref a) => {
                assert_eq!(a.sql.as_deref(), Some("SELECT a FROM t"));
                assert_eq!(a.column, "a");
                assert!(a.schema.is_none());
                assert_eq!(a.dialect, DlinDialect::Generic);
            }
            _ => panic!("Expected TraceColumn"),
        }
    }

    #[test]
    fn test_debug_trace_column_with_schema() {
        let args = unwrap_debug(
            Cli::try_parse_from([
                "dlin",
                "debug",
                "trace-column",
                "SELECT * FROM t",
                "--column",
                "a",
                "--schema",
                "t:a,b,c",
            ])
            .unwrap(),
        );
        match args.command {
            DebugCommand::TraceColumn(ref a) => {
                assert_eq!(a.schema.as_deref(), Some("t:a,b,c"));
            }
            _ => panic!("Expected TraceColumn"),
        }
    }

    #[test]
    fn test_debug_trace_column_no_sql_ok() {
        // No positional arg is allowed (stdin will be read at runtime)
        let args = unwrap_debug(
            Cli::try_parse_from(["dlin", "debug", "trace-column", "--column", "x"]).unwrap(),
        );
        match args.command {
            DebugCommand::TraceColumn(ref a) => {
                assert!(a.sql.is_none());
                assert_eq!(a.column, "x");
            }
            _ => panic!("Expected TraceColumn"),
        }
    }

    #[test]
    fn test_debug_trace_column_requires_column() {
        let result = Cli::try_parse_from(["dlin", "debug", "trace-column", "SELECT a FROM t"]);
        assert!(result.is_err(), "trace-column should require --column");
    }

    #[test]
    fn test_debug_no_subcommand_shows_help() {
        let result = Cli::try_parse_from(["dlin", "debug"]);
        assert!(result.is_err());
    }

    // -- Collapse CLI parsing tests -------------------------------------------

    #[test]
    fn test_collapse_none_by_default() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph"]).unwrap());
        assert_eq!(args.collapse, None);
    }

    #[test]
    fn test_collapse_bare_defaults_to_endpoints() {
        let args = unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--collapse"]).unwrap());
        assert_eq!(args.collapse, Some(CollapseMode::Endpoints));
    }

    #[test]
    fn test_collapse_explicit_endpoints() {
        let args =
            unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--collapse=endpoints"]).unwrap());
        assert_eq!(args.collapse, Some(CollapseMode::Endpoints));
    }

    #[test]
    fn test_collapse_focal() {
        let args =
            unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--collapse=focal"]).unwrap());
        assert_eq!(args.collapse, Some(CollapseMode::Focal));
    }

    #[test]
    fn test_collapse_invalid_mode_rejected() {
        let result = Cli::try_parse_from(["dlin", "graph", "--collapse=invalid"]);
        assert!(result.is_err());
    }

    #[test]
    fn test_collapse_bare_does_not_consume_model() {
        // --collapse without = must not swallow the next positional as MODE
        let args =
            unwrap_graph(Cli::try_parse_from(["dlin", "graph", "--collapse", "orders"]).unwrap());
        assert_eq!(args.collapse, Some(CollapseMode::Endpoints));
        assert_eq!(args.model, vec!["orders".to_string()]);
    }
}
