# dlin

A fast Rust CLI tool for analyzing [dbt](https://www.getdbt.com/) model lineage. Parses SQL files directly to extract `ref()` and `source()` dependencies, builds a DAG, and renders it as ASCII art, Graphviz DOT, SVG, interactive HTML, or a terminal UI.

Supports both direct SQL parsing (no dbt compilation or Python runtime needed) and `manifest.json` for full-fidelity graphs.

## Features

- **Direct SQL parsing** — extracts `ref()` and `source()` calls via regex, no `dbt compile` needed
- **Manifest support** — optionally read `manifest.json` for column metadata, materializations, and full graph fidelity
- **Interactive TUI** — navigate, search, and explore lineage in a terminal UI (ratatui) with Unicode box-drawing nodes, orthogonal edge routing, and full mouse support
- **Impact analysis** — `dlin impact <model>` computes downstream impact with severity scoring (Critical/High/Medium/Low)
- **7 output formats** — ASCII, Graphviz DOT, JSON, Mermaid, Plain, self-contained SVG, and interactive HTML (pan/zoom/search)
- **Stdin/pipe support** — accepts model names or file paths from stdin (e.g., `git diff --name-only | dlin graph`)
- **Run dbt from TUI** — execute `dbt run` / `dbt test` on selected models with scope control (`+upstream`, `downstream+`, `+all+`) via keyboard menu or right-click context menu
- **Run status tracking** — color-coded nodes show success (green), error (red), outdated (yellow), or never-run (default)
- **Path highlighting** — trace upstream/downstream paths with impact analysis in the TUI
- **Selector expressions** — filter by tag, path, or model name (`-s tag:finance,path:marts`)
- **Node type filtering** — filter output by node type (`--node-type model,source`)
- **Node type support** — models, sources, seeds, snapshots, tests, exposures

## Installation

### From crates.io

```sh
cargo install dlin
```

### From source

```sh
git clone https://github.com/eitsupi/dlin.git
cd dlin
cargo install --path .
```

The binary is installed to `~/.cargo/bin/dlin`.

## Usage

### Static output

```sh
# Full lineage of current directory's dbt project
dlin graph

# Focus on a specific model
dlin graph stg_orders

# Focus on multiple models
dlin graph stg_orders orders

# Point at a different project directory
dlin graph -p path/to/dbt/project

# Show 2 levels upstream, 1 downstream
dlin graph stg_orders -u 2 -d 1

# Include seeds, tests, snapshots, exposures
dlin graph --include-seeds --include-tests --include-snapshots --include-exposures

# Selector expressions
dlin graph -s tag:finance,path:marts

# Filter output by node type
dlin graph --node-type model,source

# Use manifest.json instead of parsing SQL
dlin graph --source manifest --manifest-path target/manifest.json

# Output formats
dlin graph -o dot > lineage.dot        # Graphviz DOT
dlin graph -o json                      # JSON graph
dlin graph -o mermaid                   # Mermaid diagram
dlin graph -o plain                     # Plain text (one node per line)
dlin graph -o svg > lineage.svg         # Self-contained SVG
dlin graph -o html > lineage.html       # Interactive HTML (pan/zoom/search)
```

### Pipe / stdin support

```sh
# Pipe file paths from git diff
git diff --name-only main | dlin graph

# Pipe model names
echo -e "stg_orders\norders" | dlin graph
```

When stdin contains file paths (detected by path separators or file extensions), they are automatically resolved to dbt model names.

### Interactive TUI

```sh
dlin graph -i
dlin graph -i -p path/to/dbt/project
dlin graph -i stg_orders -u 3 -d 3
```

### Impact analysis

Compute downstream impact for one or more models with severity scoring:

```sh
dlin impact orders -p path/to/project            # text report
dlin impact orders stg_orders -o json             # JSON for CI (multiple models)
dlin impact orders --show-sql -o json             # include SQL content in output
dlin impact orders --source manifest --manifest-path target/manifest.json
```

Severity levels:
- **Critical** — impacts exposures (dashboards, reports)
- **High** — impacts table/incremental materializations or mart models
- **Medium** — impacts staging or intermediate models
- **Low** — impacts tests only

## CLI Reference

```
Usage: dlin <COMMAND>

Commands:
  graph   Visualize dbt model lineage graph
  impact  Compute downstream impact analysis for a model
```

### `dlin graph`

```
Usage: dlin graph [OPTIONS] [MODEL]...

Arguments:
  [MODEL]...  Model names to focus on (shows full lineage if omitted)

Options:
  -p, --project-dir <PATH>       Path to dbt project directory [default: .]
  -u, --upstream <N>              Upstream levels to show (default: all)
  -d, --downstream <N>            Downstream levels to show (default: all)
  -i, --interactive               Launch interactive TUI mode
  -o, --output <FORMAT>           Output format [default: ascii]
                                  [values: ascii, dot, json, mermaid, plain, svg, html]
  -s, --select <SELECTOR>         Selector expression: tag:X, path:Y, or model name
                                  (comma-separated)
      --node-type <TYPES>         Filter output by node type (comma-separated:
                                  model, source, seed, snapshot, test, exposure)
      --source <SOURCE>           Data source: sql (default) or manifest [default: sql]
      --manifest-path <PATH>      Path to manifest.json file or directory containing
                                  target/manifest.json (required when --source manifest)
      --include-tests             Include test nodes
      --include-seeds             Include seed nodes
      --include-snapshots         Include snapshot nodes
      --include-exposures         Include exposure nodes
      --show-sql                  [Experimental] Include SQL file contents for each
                                  node in JSON and plain output
  -h, --help                      Print help
```

### `dlin impact`

```
Usage: dlin impact [OPTIONS] <MODEL>...

Arguments:
  <MODEL>...  Model names to analyze impact for

Options:
  -p, --project-dir <PATH>       Path to dbt project directory [default: .]
  -o, --output <FORMAT>           Output format: text (default) or json
                                  [default: text] [values: text, json]
      --source <SOURCE>           Data source: sql (default) or manifest [default: sql]
      --manifest-path <PATH>      Path to manifest.json file or directory containing
                                  target/manifest.json (required when --source manifest)
      --show-sql                  [Experimental] Include SQL file contents for each
                                  impacted node
  -h, --help                      Print help
```

## TUI Keybindings

### Navigation

| Key | Action |
|-----|--------|
| `h` `j` `k` `l` / arrow keys | Navigate between nodes (left/down/up/right) |
| `H` `J` `K` `L` | Pan the viewport |
| `+` / `-` | Zoom in / out (adjusts spacing) |
| `Tab` / `Shift+Tab` | Cycle through nodes sequentially |
| `r` | Reset view (center + zoom) |

### Mouse

| Action | Target | Effect |
|--------|--------|--------|
| Left click | Node on graph | Select node (no viewport jump) |
| Left click | Empty graph area | Begin drag to pan |
| Drag | Graph area | Pan the viewport |
| Scroll up / down | Graph area | Zoom in / out |
| Left click | Node list entry | Select node and center viewport |
| Left click | Group header | Collapse / expand group |
| Right click | Node on graph | Open context menu (run options) |

### Search

| Key | Action |
|-----|--------|
| `/` | Open search |
| `Tab` | Next search result |
| `Esc` / `Enter` | Close search |

### Analysis

| Key | Action |
|-----|--------|
| `p` | Toggle path highlighting (upstream/downstream trace with impact analysis) |

### Node list panel

| Key | Action |
|-----|--------|
| `n` | Toggle node list sidebar |
| `c` | Collapse/expand directory group |

### Running dbt

| Key | Action |
|-----|--------|
| `x` | Open run menu for selected node |
| Right click | Open context menu on a node (same run options) |
| `o` | View last run output |

Run menu / context menu options:

| Key | Command |
|-----|---------|
| `r` | `dbt run` (this model) |
| `u` | `dbt run` +upstream |
| `d` | `dbt run` downstream+ |
| `a` | `dbt run` +all+ |
| `t` | `dbt test` |

### General

| Key | Action |
|-----|--------|
| `q` | Quit |
| `Ctrl+C` | Quit (any mode) |

## Node colors in TUI

**By run status** (when `target/run_results.json` exists):

| Color | Meaning |
|-------|---------|
| Green | Last run succeeded |
| Red | Last run failed |
| Yellow | Outdated (source file modified after last run) |
| DarkGray | Skipped |

**By node type** (when never run):

| Color | Type |
|-------|------|
| Blue | Model |
| Green | Source |
| Yellow | Seed |
| Magenta | Snapshot |
| Cyan | Test |
| Red | Exposure |
| DarkGray | Phantom (unresolved ref) |

## How it works

1. **Parse** `dbt_project.yml` to find model/seed/snapshot/analysis paths (or read `manifest.json`)
2. **Walk** those directories, collecting `.sql` and `.yml` files
3. **Extract** `ref('model')` and `source('schema', 'table')` from SQL via regex
4. **Parse** YAML schema files for sources, model descriptions, and exposures
5. **Build** a directed acyclic graph (petgraph) where edges flow from dependency to dependent
6. **Filter** by focus model, depth, selectors, and node type
7. **Layout** using a Sugiyama-style layered algorithm (longest-path layering + barycenter ordering)
8. **Render** as ASCII, DOT, JSON, Mermaid, Plain, SVG, HTML, or interactive TUI

## Limitations of SQL parse mode

When using `--source sql` (the default), dlin extracts dependencies via regex without executing Jinja or Python. This means some patterns cannot be fully resolved:

- **`var()` dynamic references** — `{{ ref(var('model_name')) }}` or `source(var('schema'), var('table'))` cannot be traced because variable values are only known at dbt runtime
- **Runtime context** — expressions like `target.type`, `target.name`, or `env_var()` are not evaluated, so conditional branches depending on them may produce incomplete results
- **Conditional Jinja blocks** — `{% if var('flag') %}...{% endif %}` blocks are evaluated with default values via minijinja; refs inside branches that require non-default values may be missed
- **Column extraction** — column lists are determined from YAML schema definitions (`models:` → `columns:`) when available. If no YAML columns are defined, dlin falls back to best-effort regex extraction from the final SELECT clause. This fallback cannot resolve `SELECT *`, computed columns from CTEs, or Jinja-generated column lists. For accurate column metadata, define columns in your YAML schema files or use `--source manifest`

For full-fidelity graphs, use `--source manifest --manifest-path target/manifest.json` with a pre-compiled manifest.

## uv / virtualenv support

When running dbt from the TUI, the tool auto-detects whether to use `uv run dbt` or plain `dbt`:

- If `uv.lock` or `pyproject.toml` exists in the dbt project directory, uses `uv run dbt`
- Otherwise, if `dbt` is on PATH, uses it directly
- If `dbt` is not on PATH but `uv` is, falls back to `uv run dbt`

## Building without TUI

The TUI is enabled by default. To build a minimal binary with only static output:

```sh
cargo build --release --no-default-features
```

## Credits

This project is a hard fork of [dbt-lineage-viewer](https://github.com/sipemu/dbt-lineage-viewer) by Simon Müller, originally released under the MIT license.

## License

MIT
