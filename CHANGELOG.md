# Changelog

## [0.2.5] - 2026-08-30

### Features

- Read project variables from the dbt-standard `vars.yml` file and support dbt's Jinja-suffixed SQL filenames (`.sql.j2`, `.sql.jinja`, and `.sql.jinja2`) during project discovery (#172, #173)
- Report forward-compatibility issues in newer or partially unsupported manifests as structured diagnostics, while retaining permissive loading and deterministic resource identity/lookup behavior (#169, #170, #171)

### Breaking Changes

- In manifest mode, output node IDs are now always fully qualified dbt `unique_id` values instead of shortened IDs. Short names and aliases remain accepted as input selectors (#169, #170)
- Raw cache access in `dlin-core` is now crate-private; the legacy free-function column-lineage API has been replaced by session-oriented APIs, and `ManifestGraphCache` has been replaced by `ManifestAnalysisCache` (#175, #176, #177, #180)

### Bug Fixes

- Preserve structured and nested-field source paths across column-lineage analysis, including cross-model field access (#164)
- Correct cache invalidation and identity handling so cached results are reused only when the relevant manifest, SQL, dialect, and semantic inputs still match; project-root inputs and unreadable freshness inputs are handled safely (#175, #176, #177, #178, #179, #180)
- Recover dependencies from runtime-dependent Jinja branches and reachable local/project macros that a single default render would otherwise miss (#185)
- Distinguish unproven partial column lineage from missing or ambiguous columns, preserve the nearest honest terminal, and prevent diagnostics from unrelated or excluded columns from leaking; indeterminate-only results remain exit 0 while retaining warnings and JSON diagnostics

### Performance

- Reduce repeated manifest and SQL work by sharing a consistent manifest snapshot and memoizing semantic digest and lineage computations, improving repeated graph and column-lineage operations (#177, #180)
- Compile each Jinja template once across extraction passes and discard unused rendered SQL output, reducing dependency-analysis overhead (#182, #184)

## [0.2.4] - 2026-08-24

### Changes

- Column lineage (experimental) now runs on a new SQL analysis backend, `sqllineage`, replacing `polyglot-sql`. This is the headline change of this release. Lineage is resolved more accurately for `SELECT *` and qualified or aliased stars, `UNION`/`INTERSECT`/`EXCEPT` set operations, CTEs and derived tables, `UNNEST` value tables, array and nested-field access, and aggregate expressions such as `COUNT(*)`. Where lineage genuinely cannot be proven, it is now reported as unresolved instead of being attributed to a fabricated source (#134, #135, #136, #137, #138, #139, #141, #143, #144, #145, #147, #149, #151, #152, #153, #155, #157, #158, #159, #160, #161, #162)
- Some `--dialect` values that the previous backend accepted (e.g. `presto`, `oracle`, `athena`, `teradata`) are no longer supported. They now fall back to generic parsing with a warning. Given the experimental status of column lineage, other behavior may have shifted as well (#153)

## [0.2.3] - 2026-06-11

### Bug Fixes

- Detect `ref()` calls nested inside Jinja macro arguments (#92)

## [0.2.2] - 2026-06-06

### Features

- Auto-detect SQL dialect from manifest `adapter_type` in column lineage commands, so `--dialect` is no longer required when using a manifest (#78)
- Support dbt Semantic Layer nodes (metrics, semantic models, saved queries, etc.) in both SQL mode and manifest mode (#85)
- Filter Virtual lineage nodes from output (#86)

### Bug Fixes

- Support dbt model versioning and `ref()` `version=` argument (#81)
- Support YAML-only snapshots introduced in dbt v1.9+ (#83)
- Fix forward reference resolution between YAML-only snapshots (#84)

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
