use std::path::PathBuf;

use super::*;

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
