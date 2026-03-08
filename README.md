# dlin

A fast CLI tool for [dbt](https://www.getdbt.com/) model lineage analysis, written in Rust.

Parses SQL files directly — no `dbt compile`, no Python runtime needed. Builds a dependency graph from `ref()` and `source()` calls, then outputs it as JSON, ASCII art, DOT, Mermaid, SVG, HTML, or an interactive terminal UI.

## Highlights

- **Fast** — parallel SQL extraction with [rayon](https://github.com/rayon-rs/rayon); disk cache for instant subsequent runs
- **Zero Python dependency** — works from a single binary; no virtualenv, no dbt installation required
- **Machine-readable output** — deterministic JSON with `--json-fields` for field selection, designed for CI and AI agent pipelines
- **Two data sources** — parse SQL directly (default) or read `manifest.json` for full-fidelity graphs
- **Composable** — stdin/stdout piping across subcommands (`impact` → `list` → `jq`)

## Installation

```sh
cargo install --git https://github.com/eitsupi/dlin.git
```

## Quick start

```sh
$ dlin graph -p path/to/dbt/project
 [ src:raw.payments ]     [ stg_payments ]        [ orders ]        [ customers ]
  [ src:raw.orders ]       [ stg_orders ]      [ order_summary ]
[ src:raw.customers ]    [ stg_customers ]

Edges:
  stg_orders ──ref──> order_summary
  stg_payments ──ref──> order_summary
  stg_customers ──ref──> customers
  orders ──ref──> customers
  stg_orders ──ref──> orders
  stg_payments ──ref──> orders
  src:raw.orders ──src──> stg_orders
  src:raw.payments ──src──> stg_payments
  src:raw.customers ──src──> stg_customers
```

## Usage examples

### Graph — visualize lineage

```sh
dlin graph                                # full lineage (ASCII)
dlin graph orders -u 1 -d 1              # 1 hop upstream/downstream
dlin graph -o json                        # JSON for programmatic use
dlin graph -o dot | dot -Tsvg > out.svg   # Graphviz rendering
dlin graph -o json --json-fields unique_id,label  # select specific fields
dlin graph -i                             # interactive TUI
```

### List — enumerate nodes

```sh
$ dlin list
model   customers
model   order_summary
model   orders
model   stg_customers
model   stg_orders
model   stg_payments
source  raw.customers
source  raw.orders
source  raw.payments
```

Filter to specific models and output as JSON:

```sh
$ dlin list orders -o json --json-fields unique_id,file_path
[{"file_path":"models/marts/orders.sql","unique_id":"model.orders"}]
```

### Impact — downstream impact analysis

```sh
$ dlin impact orders
Impact Analysis: orders
==================================================
Overall Severity: CRITICAL

Summary:
  Affected models:    1
  Affected tests:     1
  Affected exposures: 1

Impacted Nodes:
  [critical] weekly_report (exposure, 1 hops)
  [high    ] customers (model, 1 hops) [models/marts/customers.sql]
  [low     ] assert_orders_positive_amount (test, 1 hops)
```

### Pipelines — compose subcommands

```sh
# Get impacted model names, then fetch their SQL
dlin impact orders -o json \
  | jq -r '.[].impacted_nodes[].unique_id' \
  | dlin list -o json --json-fields unique_id,sql_content

# Lineage of changed files
git diff --name-only main | dlin graph -o json

# List changed models with metadata
git diff --name-only main | dlin list -o json --json-fields unique_id,label,description
```

Stdin accepts model names or file paths. File paths (detected by extension or path separators) are automatically resolved to model names using `dbt_project.yml`.

## Performance

dlin is designed for fast feedback loops:

- **Parallel extraction** — SQL files are parsed concurrently using rayon
- **Disk cache** — extraction results are cached to `.dlin_cache/extraction_cache.json` (auto-created, gitignored); invalidated per-file by mtime and size
- **In-memory dedup** — minijinja template rendering is performed once per file and reused across phases
- **No runtime dependency** — single static binary, no Python interpreter startup

Use `--no-cache` to force a fresh parse. Use `--cache-dir` to customize the cache location.

## Data sources

### SQL parsing (default)

Extracts `ref()` and `source()` calls from SQL via regex + [minijinja](https://github.com/mitsuhiko/minijinja) template evaluation. Handles Jinja blocks, macros, and config expressions. No Python or dbt installation required.

### Manifest (`--source manifest`)

Reads a pre-compiled `manifest.json` for full accuracy including column metadata, materializations, and complex Jinja logic that cannot be statically analyzed.

```sh
dlin graph --source manifest --manifest-path target/manifest.json
dlin graph --source manifest --manifest-path path/to/project  # auto-finds target/manifest.json
```

## JSON output

### Field selection

Control which fields appear in JSON node output:

```sh
dlin graph -o json --json-fields unique_id,label        # only these fields
dlin graph -o json --json-full                           # all available fields
dlin list -o json --json-fields unique_id,sql_content    # works on list too
```

Available fields: `unique_id`, `label`, `node_type`, `file_path`, `description`, `materialization`, `tags`, `columns`, `sql_content`

Default (when neither flag is given): `unique_id`, `label`, `node_type`, `file_path`

### Output format

- TTY → pretty-printed JSON
- Pipe/redirect → compact single-line JSON (for `jq`, scripts, etc.)

## Filtering

```sh
dlin graph -s tag:finance,path:marts      # selector expressions (union)
dlin graph --node-type model,source       # post-filter by node type
dlin graph --include-tests --include-seeds  # include optional node types
dlin list --node-type source              # list only sources
```

Selectors support `tag:<name>`, `path:<prefix>`, and bare model names (comma-separated, OR logic).

## Interactive TUI

```sh
dlin graph -i
```

Features: vim-style navigation, mouse support, search (`/`), path highlighting (`p`), run dbt commands (`x`), node list sidebar (`n`).

<details>
<summary>TUI keybindings</summary>

| Key | Action |
|-----|--------|
| `h` `j` `k` `l` / arrows | Navigate nodes |
| `H` `J` `K` `L` | Pan viewport |
| `+` / `-` | Zoom |
| `/` | Search |
| `p` | Toggle path highlighting |
| `n` | Toggle node list |
| `x` | Run menu (dbt run/test) |
| `o` | View last run output |
| `q` | Quit |

Mouse: click to select, drag to pan, scroll to zoom, right-click for context menu.

</details>

The TUI reads `target/run_results.json` when available and color-codes nodes by run status (green = success, red = error, yellow = outdated).

To build without TUI dependencies:

```sh
cargo install dlin --no-default-features
```

## Limitations of SQL parse mode

- **`var()` dynamic references** — `ref(var('name'))` cannot be resolved (variable values are runtime-only)
- **Runtime context** — `target.type`, `env_var()`, etc. are not evaluated
- **Conditional Jinja** — branches are evaluated with default values; non-default paths may be missed
- **Column extraction** — falls back to regex on final SELECT when YAML schema is absent; cannot resolve `SELECT *` or CTE columns

For full accuracy, use `--source manifest`.

## Credits

This project is a hard fork of [dbt-lineage-viewer](https://github.com/sipemu/dbt-lineage-viewer) by Simon Muller, originally released under the MIT license.

## License

MIT
