# Changelog

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
