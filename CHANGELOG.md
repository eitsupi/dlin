# Changelog

## [0.1.0-rc.2] - 2026-03-24

Second pre-release of **dlin**, a hard fork of [dbt-lineage-viewer 0.2.0](https://github.com/sipemu/dbt-lineage-viewer).

### Features

- Subcommands: `graph`, `impact`, `list`, `summary`, `check-manifest`
- Output formats: JSON, plain text, DOT, Mermaid, SVG, HTML
- Two data sources: direct SQL parsing (default) or `manifest.json`
- Stdin/stdout piping for composable workflows
- File-based extraction cache
- Windows binary support

### Differences from upstream dbt-lineage-viewer

- Interactive TUI (`-i` / `--interactive`) is not included — dlin focuses on non-interactive output formats suitable for CI and scripting workflows
