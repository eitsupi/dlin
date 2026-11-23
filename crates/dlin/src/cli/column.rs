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
        long_about = concat!("\
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

Output columns:
  The output columns of a model are the columns of its compiled SQL, in SQL
  order. Columns with the same name stay separate, and a name written with
  quotes keeps them. Columns declared in YAML but not in the SQL output are
  reported as non-fatal diagnostics, not added to the output.
  --column NAME selects by the dialect's identifier rules (write \"Name\" or
  `Name` to select a quoted name). A name shared by several columns is reported
  as ambiguous and selects none of them.

", column_limitations!(), "
  In JSON, check output.columns[].lineage.status and lineage.unresolved[],
  output.shape for unexpanded stars, boundaries[] for where tracing stopped,
  and errors[] for diagnostics.

Stdin/pipe support:
  Accepts model names or file paths on stdin (one per line).
  SQL paths select a node by unique ID. YAML paths expand to resource names.
  Without dbt_project.yml, path inputs use the standard dbt layout; use model
  names with a standalone manifest from a project with custom paths.

Output format (-o/--output):
  json (default)  JSON array per model with the following structure:
    model             model name
    unique_id         dbt unique id of the model
    listed_columns    number of output columns listed (0 when the SQL could not
                      be analysed). A column after an unexpanded star counts
                      once if it is listed; the star itself is not counted
    value_complete_columns  listed columns whose lineage is complete
    value_partial_columns   listed columns whose lineage is partial
    boundaries[]     where tracing stopped, with a reason and resource kind;
                     external relations and source-free outputs are valid ends,
                     while failed, missing, ambiguous, cyclic and unsupported
                     resources make lineage partial. evidence[] (omitted if
                     empty) lists the schema evidence ids the trace relied on
                     to reach it
    evidence[]        the schema evidence the result cites, and what it was
                      derived from:
      id              evidence id, such as yaml:<unique_id> or sql:<unique_id>
      subject         the dbt node or relation the schema describes
      origin          sql_output, catalog_artifact or yaml_declaration
      coverage        exhaustive or partial
      coverage_basis  enumerated, assumed_by_yaml_policy (a YAML list treated
                      as exhaustive) or declared_enforced_contract
      order           proven or unknown
      enforcement     what is known about an enforced contract, or null
      derived_from[]  evidence an SQL output schema relied on for its columns
    output            null when the SQL could not be analysed (see errors[]),
                      otherwise:
      shape           {\"kind\": \"closed\"}, or {\"kind\": \"open\", \"unexpanded\": [..]}
                      when a star could not be expanded and more columns may
                      exist than are listed
      evidence[]      evidence ids that decide which columns are listed and
                      whether there are more (omitted if empty)
      columns[]       output columns in SQL order
        id            position of this entry in columns[] (tells columns with
                      the same name apart)
        ordinal       zero-based SQL position when proven (null after a star)
        name          {\"kind\": \"explicit\"|\"inferred\", \"ident\": {\"value\", \"quote\"}},
                      or {\"kind\": \"anonymous\"} for an unnamed expression
        transformation  how the column was derived:
                        direct       passed through unchanged (including renames)
                        aggregation  aggregate function (SUM, COUNT, etc.)
                        expression   arithmetic or other expression
                        cast         type cast (CAST(x AS INT))
                        conditional  CASE WHEN expression
                        unknown      could not classify
        evidence      evidence ids the column relies on (omitted if empty):
                      structure[] for its name and position, lineage[] for its
                      lineage as a whole
        lineage
          status      \"complete\" when every contributing source is known, or
                      \"partial\" when some contributions could not be resolved
          known[]     sources proven to contribute to the value
            table     source model or raw table name
            column    source column identifier ({\"value\", \"quote\"})
            model_path[]  intermediate hops traversed, each with the model output
                      (unique_id, model, slot, column) and its transformation
                      (omitted if empty)
            evidence[]  evidence ids this source relies on along its path
                      (omitted if empty)
          unresolved[]  (partial only) contributions that could not be resolved:
            reason    structured reason, tagged by \"kind\"
            origin    model output where the unresolved part arose
            model_path[]  hops from this column to the origin
            evidence[]  evidence ids that leave this part unresolved, and
                      those of the path to it (omitted if empty)
    errors[]    parse or resolution diagnostics. An indeterminate per-column
                diagnostic and a difference between the YAML columns and the
                SQL output are non-fatal and do not change the exit code;
                model-level, missing-column, ambiguous, and parse diagnostics
                are fatal (exit code 1). A possible overlap with an
                unexpanded star is indeterminate and nonfatal.
  plain           human-readable text, one model per block. An \"assumes\" line
                  names each assumed column list (a YAML list taken as
                  exhaustive, or an enforced contract) and the columns that
                  rely on it
  mermaid         Mermaid flowchart (LR) with subgraphs per model, boundary
                  notes and assumption notes
  dot             Graphviz DOT format; models as clusters, columns as nodes
                  color-coded by transformation type, with distinct notes for
                  legitimate ends, incomplete boundaries and assumptions
                  (pipe to `dot -Tsvg`)

Exit codes:
  0   Success
  1   Error (model not found, no manifest, fatal analysis errors, etc.)"),
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
        long_about = concat!("\
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

", column_limitations!(), "
  In JSON, possible[] lists unconfirmed targets, exploration[] lists gaps
  that may hide further targets, and errors[] holds diagnostics.

Output format (-o/--output):
  json (default)  JSON array per column:
    model, column     the analysed input
    input             the model output that the column names, or absent when it
                      names none or several (the exploration then has an
                      \"unidentified_input\" gap; dependents that name the
                      column are still searched by that name, and their paths
                      start at an \"unidentified_column\" that does not stand
                      for one particular output of the model)
    scope             \"value_only\"
    confirmed[]       downstream columns that depend on the input through known
                      edges (target, kind, transformation, path of edges).
                      evidence[] (omitted if empty) lists the schema evidence
                      ids the path relies on
    possible[]        concrete downstream targets that may depend on the input
                      through finite unresolved paths; paths remain separate.
                      evidence[] as for confirmed[]
    exploration[]     per dependency kind, \"complete\" or \"incomplete\" with the
                      gaps (unresolved lineage, failed analysis, a dependent
                      model with unlisted columns, ...) that may hide
                      further impacts. \"no impact\" is shown only when there
                      are no confirmed or possible targets and every requested
                      dependency kind is complete; it relies on the
                      evidence[] of every exploration.
    evidence[]        the schema evidence the report cites, described as for
                      `dlin column upstream`
    errors[]          diagnostics of the input column and of the models the
                      search could not analyse
  plain           human-readable text with confirmed and possible counts, scope,
                  per-kind gaps and concrete paths. An \"assumes\" line names
                  each assumed column list and the conclusions that rely on it
  mermaid         Mermaid flowchart (LR) showing impacted columns across models,
                  with gap and assumption notes
  dot             Graphviz DOT format; models as clusters, columns as nodes
                  color-coded by transformation type (pipe to `dot -Tsvg`)

Exit codes:
  0   Success
  1   Error (model not found, no manifest, fatal analysis errors, etc.)"),
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
