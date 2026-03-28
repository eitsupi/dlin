# dlin

[![Crates.io](https://img.shields.io/crates/v/dlin)](https://crates.io/crates/dlin)
[![PyPI](https://img.shields.io/pypi/v/dlin-cli)](https://pypi.org/project/dlin-cli/)

dbt lineage analysis CLI that parses SQL files directly. No `dbt compile`, no Python, no `manifest.json`.

Builds a dependency graph from `ref()` and `source()` calls in SQL. Designed for AI agents and CI pipelines.

## Motivation

When I edited dbt models in VS Code, [dbt Power User](https://marketplace.visualstudio.com/items?itemName=innoverio.vscode-dbt-power-user) was my go-to companion for navigating lineage. AI agents have no such companion. I watched them `grep` through dbt projects to find model dependencies. It works, but they end up calling `grep` repeatedly and relying on fragile string matching to piece together `ref()` and `source()` relationships.

dlin is designed to fill that gap: a CLI tool that lets AI agents understand a dbt project's structure without falling back to `grep`. It is equally useful for humans, and its stdin/stdout interface makes it easy to combine with `jq`, `git diff`, and other CLI tools.

To replace `grep`, speed and size matter. dlin is a small, self-contained binary with no runtime dependencies. It parses SQL directly, evaluates common Jinja patterns without Python, parallelizes file I/O, and caches aggressively.

The key idea behind dlin is that finding the right models fast is what matters most. AI agents can read SQL and trace column-level relationships on their own; the hard part is knowing which models to look at in the first place. So dlin focuses on model-level lineage and makes that as fast as possible.

## Install

### Cargo (Rust)

```sh
cargo install dlin
```

### pip / uv (Python)

For convenience, dlin is also available as a Python package. The installed binary is native and does not require Python at runtime.

```sh
pip install dlin-cli   # or: uv tool install dlin-cli
```

### GitHub Releases

Pre-built binaries for Linux, macOS, and Windows are available on the [Releases](https://github.com/eitsupi/dlin/releases) page. You can also use the installer scripts:

macOS / Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/eitsupi/dlin/releases/latest/download/dlin-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/eitsupi/dlin/releases/latest/download/dlin-installer.ps1 | iex"
```

## Quick start

```sh
# Full lineage graph
dlin graph -p path/to/dbt/project

# Downstream impact analysis
dlin impact orders

# List models as JSON
dlin list -o json --json-fields unique_id,file_path

# Pipe changed files into lineage
git diff --name-only main | dlin graph -o json
```

## Features

- **No dependencies**: single binary, no Python, no `manifest.json`
- **Recursive upstream / downstream**: `-u N` / `-d N` to control traversal depth
- **Impact analysis with severity**: `dlin impact` scores downstream nodes and flags exposure reachability
- **Composable**: stdin accepts model names or file paths; pipe with `jq`, `dlin list`, `git diff`, etc.
- **Agent-friendly**: `--error-format json` emits structured `{"level","what","why","hint"}` on stderr; `--help` is designed for tool discovery

## Mermaid diagrams

dlin outputs Mermaid flowcharts that render natively on GitHub, GitLab, Notion, and other Markdown environments.

### Simplified graphs with `--collapse`

Automatically remove intermediate nodes to see just the endpoints (nodes with no predecessors or no successors); everything in between becomes transitive "(via N)" edges:

```sh
# Collapse intermediate models — only endpoints remain
dlin graph --collapse -o mermaid

# Focal mode: keep only sources, exposures, and specified focus models
# (ignores BFS window pseudo-endpoints — ideal with -u/-d limits)
dlin graph orders --collapse=focal -u 3 -o mermaid
```

```mermaid
flowchart LR
    exposure_weekly_report>"weekly_report"]
    model_combined_orders["combined_orders"]
    model_order_summary["order_summary"]
    source_raw_customers(["raw.customers"])
    source_raw_orders(["raw.orders"])
    source_raw_payments(["raw.payments"])

    source_raw_customers ==>|"exposure (via 2)"| exposure_weekly_report
    source_raw_orders ==>|"exposure (via 3)"| exposure_weekly_report
    source_raw_orders -.->|"source (via 1)"| model_combined_orders
    source_raw_orders -.->|"source (via 1)"| model_order_summary
    source_raw_payments ==>|"exposure (via 3)"| exposure_weekly_report
    source_raw_payments -.->|"source (via 1)"| model_order_summary

    classDef model fill:#4A90D9,stroke:#333,color:#fff
    classDef source fill:#27AE60,stroke:#333,color:#fff
    classDef exposure fill:#E74C3C,stroke:#333,color:#fff
    class exposure_weekly_report exposure
    class model_combined_orders model
    class model_order_summary model
    class source_raw_customers source
    class source_raw_orders source
    class source_raw_payments source
```

Positional focus models are always preserved during collapse, so `dlin graph orders --collapse` keeps `orders` even if it would otherwise be intermediate.

### Pipe to build focused diagrams

Combine `dlin list`, `jq`, and `dlin graph` to extract exactly the nodes you want:

```sh
# Staging models → 1 hop downstream, models only, grouped by directory
dlin list -s 'path:models/staging' -o json | jq -r '.[].label' |
  dlin graph -d 1 --node-type model --group-by directory -o mermaid
```

```mermaid
flowchart LR
    subgraph models_marts["models/marts"]
        model_combined_orders["combined_orders"]
        model_customers["customers"]
        model_order_summary["order_summary"]
        model_orders["orders"]
    end
    subgraph models_staging["models/staging"]
        model_stg_customers["stg_customers"]
        model_stg_online_orders["stg_online_orders"]
        model_stg_orders["stg_orders"]
        model_stg_payments["stg_payments"]
        model_stg_retail_orders["stg_retail_orders"]
    end

    model_orders -->|ref| model_customers
    model_stg_customers -->|ref| model_customers
    model_stg_online_orders -->|ref| model_combined_orders
    model_stg_orders -->|ref| model_order_summary
    model_stg_orders -->|ref| model_orders
    model_stg_payments -->|ref| model_order_summary
    model_stg_payments -->|ref| model_orders
    model_stg_retail_orders -->|ref| model_combined_orders

    classDef model fill:#4A90D9,stroke:#333,color:#fff
    class model_combined_orders model
    class model_customers model
    class model_order_summary model
    class model_orders model
    class model_stg_customers model
    class model_stg_online_orders model
    class model_stg_orders model
    class model_stg_payments model
    class model_stg_retail_orders model
```

### Column names in nodes with `--show-columns`

Add `--show-columns` to include column names inside Mermaid node labels — useful for understanding what each model produces at a glance:

```sh
dlin graph orders -u 1 -d 0 --show-columns --node-type model,source -o mermaid
```

```mermaid
flowchart LR
    model_orders["orders<br/>---<br/>order_id, customer_id, order_date, status, total_amount, payment_method"]
    model_stg_orders["stg_orders<br/>---<br/>order_id, customer_id, order_date, status"]
    model_stg_payments["stg_payments<br/>---<br/>payment_id, order_id, amount, payment_method"]

    model_stg_orders -->|ref| model_orders
    model_stg_payments -->|ref| model_orders

    classDef model fill:#4A90D9,stroke:#333,color:#fff
    class model_orders model
    class model_stg_orders model
    class model_stg_payments model
```

Combines well with `--collapse` to show rich detail on fewer endpoint nodes.

### Other graph options

```sh
dlin graph orders -u 2 -d 1                            # focus on specific model
dlin graph -o mermaid --collapse --show-columns        # columns in collapsed nodes
dlin graph orders --collapse=focal -u 3 -o mermaid    # focal: sources + exposures + orders
dlin graph -o mermaid --group-by directory             # group by directory
dlin graph -o mermaid --direction tb                   # top-to-bottom layout
dlin graph --node-type source,exposure                 # filter by node type
dlin graph -o dot | dot -Tsvg > out.svg                # Graphviz rendering
```

Output formats: ASCII (default), JSON, Mermaid, Graphviz DOT, Plain, SVG, HTML.

## Key subcommands

### `list`

```sh
dlin list                                                   # all models and sources
dlin list orders -o json --json-fields unique_id,file_path  # specific model as JSON
dlin list --node-type source                                # sources only
```

### `impact`

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
  [critical] weekly_report (exposure, distance: 1)
  [high    ] customers (model, distance: 1) [models/marts/customers.sql]
  [low     ] assert_orders_positive_amount (test, distance: 1)
```

## Filtering

```sh
dlin graph -s tag:finance,path:marts  # selector expressions (union)
dlin graph --node-type model,source   # filter by node type
```

## Data sources

dlin aims to work without `dbt compile`. By default it parses SQL files directly, but it can also leverage a pre-compiled `manifest.json` for additional accuracy when one is available.

**SQL parsing (default)**: extracts `ref()` and `source()` from SQL via regex + Jinja template evaluation. No Python or dbt needed.

**Manifest mode** (`--source manifest`): reads a pre-compiled `manifest.json` for full accuracy with complex Jinja logic.

### Limitations of SQL parse mode

- `var()` resolves from `dbt_project.yml` only (`--vars` CLI overrides not supported)
- Runtime context (`target.type`, `env_var()`) is not evaluated
- Conditional Jinja branches use default values; non-default paths may be missed

When these limitations matter, use `--source manifest`.

## Credits

Hard fork of [dbt-lineage-viewer](https://github.com/sipemu/dbt-lineage-viewer) by Simon Muller (MIT license). The original focused on TUI-based exploration; dlin removes the TUI and targets non-interactive use: scripting, CI, and AI agents.

## License

MIT
