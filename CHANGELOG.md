# Changelog

## [0.1.0-rc.1] - 2026-03-22

First pre-release of **dlin**, a hard fork of [dbt-lineage-viewer 0.2.0](https://github.com/sipemu/dbt-lineage-viewer).

### Features

- Subcommands: `graph`, `impact`, `list`, `summary`, `check-manifest`
- Output formats: JSON, plain text, DOT, Mermaid, SVG, HTML, interactive TUI
- Two data sources: direct SQL parsing (default) or `manifest.json`
- Stdin/stdout piping for composable workflows
- File-based extraction cache
