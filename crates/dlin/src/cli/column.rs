use std::path::PathBuf;

use super::*;
use clap::Subcommand;

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
