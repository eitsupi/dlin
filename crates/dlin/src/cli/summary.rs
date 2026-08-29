use std::path::PathBuf;

use super::*;

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
