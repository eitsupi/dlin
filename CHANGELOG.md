# Changelog

## [0.2.1] - 2026-06-01

### Features

- Experimental MCP stdio server: new `dlin mcp` subcommand exposes project summary, model search, lineage, impact, and column lineage as MCP tools for use with AI assistants; requires `--dialect` and a `manifest.json` (#76)
- Improve manifest-mode cache behavior for column lineage to reduce unnecessary recomputation and speed up repeated analysis runs (#74)

### Bug Fixes

- Fix stale cache invalidation paths in manifest mode so column lineage results are recomputed when manifest file state changes (#74)

## [0.2.0] - 2026-05-30

### Features

- Experimental column-level lineage: new `dlin column upstream` and `dlin column downstream` subcommands for tracing column data flows across dbt models, with JSON, Mermaid, DOT/Graphviz, and plain text output formats; requires `manifest.json`
- All commands now work with `manifest.json` alone, without requiring a full dbt project directory (#65)
- Add `--search` option to `dlin list` for filtering nodes by name or description (#48)

## [0.1.2] - 2026-04-11

### Features

- New `dlin-core` library crate is now published to crates.io, allowing Rust projects to integrate dbt lineage analysis programmatically (#29)
- Infer generic tests from YAML declarations in SQL mode (#27)

### Bug Fixes

- Fix CLI help text formatting with proper line breaks (#28)

## [0.1.1] - 2026-03-30

### Features

- Add `--collapse=focal` mode for BFS-aware lineage simplification, and preserve focus models during collapse (#22, #23)

## [0.1.0] - 2026-03-28

Initial release of **dlin**.
