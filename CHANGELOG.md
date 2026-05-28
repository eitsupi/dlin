# Changelog

## [0.2.0-beta.2] - 2026-05-28

### Features

- Add `--search` option to `dlin list` for filtering nodes by name or description (#48)

### Bug Fixes

- Column lineage: fix stdin file path matching so `git diff --name-only | dlin column upstream` works correctly with real dbt projects (#52)
- Column lineage: fix empty-table sources being displayed as blank instead of `(literal)` in mermaid, dot, and plain output (#49)
- Column lineage: fix non-direct intermediate hops in cross-model lineage being silently dropped in plain text output (#51)
- Column lineage: fix BigQuery `SAFE.function()` calls generating phantom `safe` column references (via polyglot-sql 0.4.2) (#50)

## [0.2.0-beta.1] - 2026-05-24

### Breaking Changes

- Column lineage output now includes intermediate hops with per-hop transformation labels, which changes upstream/downstream graph payload structure (#45)

### Features

- Add DOT/Graphviz output to `dlin column upstream` and `dlin column downstream` for visualizing column-level lineage as graphs (#42)
- Add stdin model input support to `dlin column upstream` for smoother shell-based workflows (#40)
- Improve model-not-found guidance and column subcommand help text for faster issue diagnosis (#41)

### Bug Fixes

- Fix JOIN alias resolution in column lineage so aliases correctly map back to actual model names (#44)

## [0.2.0-alpha.2] - 2026-05-23

### Breaking Changes

- Column lineage subcommands renamed: `dlin column graph` → `dlin column upstream`, `dlin column impact` → `dlin column downstream` (#37)

### Features

- Add `--format` option to column subcommands (`upstream`/`downstream`) for controlling output format (#35)

### Bug Fixes

- Fix incorrect transformation type for function calls and CTE pass-throughs in column lineage (#36)

## [0.2.0-alpha.1] - 2026-05-22

### Features

- Experimental column lineage subcommands: `dlin column graph` and `dlin column impact` for tracing column-level data flows across dbt models (#33)

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
