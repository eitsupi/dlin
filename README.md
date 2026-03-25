# dlin

dbt lineage analysis CLI that parses SQL files directly. No `dbt compile`, no Python, no `manifest.json`.

Builds a dependency graph from `ref()` and `source()` calls in SQL. Designed for AI agents and CI pipelines.

```sh
cargo install --git https://github.com/eitsupi/dlin.git
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

## Why dlin?

| Capability | `grep` | `dbt ls` | manifest-based tools | **dlin** |
| --- | --- | --- | --- | --- |
| Recursive upstream / downstream | no | yes (`+`) | varies | yes (`-u N` / `-d N`) |
| Impact analysis with severity | no | no | some | **yes** (`impact`) |
| Exposure reachability | no | no | rare | **yes** (in `impact`) |
| Works without `manifest.json` | yes | no | no | **yes** |
| Works without Python / dbt | yes | no | no | **yes** |
| Structured errors for agents | no | no | no | **yes** (`--error-format json`) |

`grep` can't follow the dependency graph. `dbt ls` and manifest-based tools (dbt-meshify, elementary, fal, etc.) require `dbt compile` first. dlin parses SQL directly.

## Agent-friendly design

Built for AI coding agents that discover tools through `--help` and learn from errors.

- **Structured errors**: `--error-format json` emits `{"level","what","why","hint"}` on stderr
- **Actionable hints**: error messages tell the agent what to try next
- **Machine-readable JSON**: `--json-fields` to select fields; compact output when piped
- **Composable**: stdin accepts model names or file paths (`dlin impact` → `dlin list` → `jq`)

## Subcommands

### `graph`

```sh
dlin graph                                        # full lineage (ASCII)
dlin graph orders -u 1 -d 1                       # 1 hop upstream/downstream
dlin graph -o json --json-fields unique_id,label  # select JSON fields
dlin graph -o dot | dot -Tsvg > out.svg           # Graphviz rendering
dlin graph -o mermaid                             # Mermaid diagram
```

Output formats: ASCII (default), JSON, Mermaid, Graphviz DOT, Plain, SVG, HTML.

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
dlin graph -s tag:finance,path:marts        # selector expressions (union)
dlin graph --node-type model,source         # filter by node type
dlin graph --include-tests --include-seeds  # include optional node types
```

## Data sources

**SQL parsing (default)**: extracts `ref()` and `source()` from SQL via regex + Jinja template evaluation. No Python or dbt needed.

**Manifest mode** (`--source manifest`): reads a pre-compiled `manifest.json` for full accuracy with complex Jinja logic.

### Limitations of SQL parse mode

- `var()` resolves from `dbt_project.yml` only (`--vars` CLI overrides not supported)
- Runtime context (`target.type`, `env_var()`) is not evaluated
- Conditional Jinja branches use default values; non-default paths may be missed

For full accuracy, use `--source manifest`.

## Credits

Hard fork of [dbt-lineage-viewer](https://github.com/sipemu/dbt-lineage-viewer) by Simon Muller (MIT license). The original focused on TUI-based exploration; dlin removes the TUI and targets non-interactive use: scripting, CI, and AI agents.

## License

MIT
