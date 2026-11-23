# Changelog

## [Unreleased]

### Breaking Changes

- Impact reports exclude tests by default. Completeness and no-impact proofs apply to the reported `node_types`. JSON uses null for each affected count whose type is outside the selection and for `overall_severity` when the result is empty.
- Rust API: `compute_impact` takes an `ImpactScope` with at least one selected node type.

### Features

- Select downstream resource types with `dlin impact --node-type` or the MCP `get_impact` argument `node_types`. Traversal uses the full graph in both SQL and manifest mode.

## [0.3.0-beta.1] - 2026-11-22

### Breaking Changes

- SQL mode no longer prints `unresolved ref '...'` or `unresolved source '...'` warnings when a reference names a missing target. Every referencing node carries an `unresolved_ref` or `unresolved_source` reason instead, and the stderr summary counts them. Use `--verbose` to see each one with its node and call site (#281)
- Commands that build the lineage graph print a `dependency_analysis_summary` on stderr when the dependency analysis is incomplete, and a configuration warning when a path set in `dbt_project.yml` does not exist. In manifest mode the summary is printed at the `info` level, since manifest dependencies are not assessed. `--quiet` suppresses both (#281)
- Rust API: `compute_impact` takes a `GraphBuild`, which holds the graph together with its project gaps, and returns `Option<ImpactReport>`. Build one with `build_graph_report` or `GraphBuild::from_manifest`. `ImpactReport` exposes `impacted_nodes`, `source_node` and `exploration` through accessors (#281)

### Features

- Impact reports say whether they can miss downstream nodes. JSON reports include `source_node`, the `dependency_analysis` of each impacted node, and an `exploration` that is `complete` or `incomplete` with typed gaps: nodes outside the result whose dependencies are not fully analysed, skipped inputs, and unassessed scopes such as manifest dependencies. Text output shows an `Exploration:` line. The MCP `get_impact` result includes the same information. The README describes how to check “no impact” in CI (#281)
- `list` JSON includes each node's `dependency_analysis` by default, and `graph` JSON includes it with `--json-full` or `--json-fields dependency_analysis` (#281)
- New global `--verbose` flag prints one diagnostic per dependency analysis reason and skipped input. With `--error-format json`, these diagnostics carry a `code` and typed fields (#281)

### Bug Fixes

- Report YAML generic tests as partially analysed: the body of the test macro is not analysed, so a `ref()` written only inside it has no edge. An impact report for a model read only inside such a test no longer claims “no impact” (#278, #281)
- Apply in-place changes, such as `update()`, `pop()` and `append()`, to dicts and lists produced by `dict()`, `var()`, filters and methods, so the changed value selects the dependency. Changes that are not modelled make the value unknown instead of keeping its old value (#280)
- Column lineage no longer reports window functions that use a named or inherited window (`OVER w`) without the window's `PARTITION BY` and `ORDER BY` columns. Wildcard function arguments such as `OBJECT_CONSTRUCT(*)` make lineage partial instead of complete with no inputs (#277)

## [0.3.0-alpha.2] - 2026-11-19

### Breaking Changes

- MCP tools return one compact JSON text result in `content` and no longer return `structuredContent`. Impact tools default to summaries, and `find_nodes` includes compiled SQL only when requested with `include_sql: true` (#274)
- Column commands return partial lineage and exit with status 0 when an unknown schema prevents them from verifying a column's provider, provided there are no other fatal diagnostics. Confirmed ambiguities remain fatal. Column lineage JSON exposes the `unverified` reason and candidate providers (#272, #273)

### Features

- Use CSV headers as column schema evidence for seeds with no columns declared in the manifest, including installed package seeds. Changing or removing a header invalidates cached lineage that used it (#273, #274)
- Paginate MCP node searches, lineage graphs and impact results. Request `detail: "full"` for impact paths and detailed evidence. The project summary reports manifest freshness, grouped load warnings and the number of skipped unit tests (#274)

### Bug Fixes

- Keep the schema of sources declared without one in column lineage output, defaulting it to the source name as dbt does, so a column from `raw.orders` is no longer shown as `orders` in plain, Mermaid and DOT output or in JSON `known[].table` (#276)
- Stop attributing a table to a source whose physical relation has another name. Compiled SQL names a source by its physical relation, so a reference to the source's logical name, such as `raw.orders` for a source stored as `landing.orders_v2`, now reads that table rather than the source (#276)
- Resolve BigQuery `INFORMATION_SCHEMA` references with their project and dataset or region intact. Match these references to declared sources and preserve their namespace in column lineage JSON (#272)
- Trace fields exposed by unaliased BigQuery `UNNEST` to their array inputs. When the element schema is unknown, report possible providers and downstream impact instead of guessing a source (#272)
- Stop treating normalization modes such as `NFKC` as column references in BigQuery `NORMALIZE` and `NORMALIZE_AND_CASEFOLD`, and in PostgreSQL and Trino `normalize` (#272)
- Check manifest paths against their owning project or installed package, including package macros and configuration. Check dependency declarations and the lockfile for updates. Report missing packages as unverified and missing files inside installed packages as stale (#274)
- Skip valid dbt unit tests without warnings, including in strict manifest mode. Strict mode rejects entries with a missing or mismatched resource type or ID (#274)

## [0.3.0-alpha.1] - 2026-11-19

### Highlights

- **Model-level lineage (SQL mode).** A new Jinja interpreter replaces placeholder rendering and regex recovery. It evaluates project variables, macro calls, conditions and loops to extract dependencies from SQL and supported YAML expressions (#262, #263, #264)
- **Column-level lineage (Experimental).** The redesigned SQL analysis engine tracks ordered, quoted output slots, preserves known sources in partial results, and reports trace boundaries, possible impacts and schema evidence (#215, #244, #245, #246, #250)

### Breaking Changes

#### Model-level lineage (SQL mode)

- With `execute` set to false, the interpreter skips `if execute` bodies and standard `statement` blocks. Refs in branches ruled out by project variables or in unused macros no longer produce edges. Names that depend on unknown runtime values no longer produce guessed edges or placeholder warnings. Literal refs under unknown conditions still produce edges. Unresolved literal refs and sources still emit warnings (#262, #263)
- `env_var()` uses its default without reading environment variables. Calls without a default and calls for `DBT_ENV_SECRET*` remain unknown. Both `var()` and `env_var()` accept `default=` (#262, #263)
- Repeated refs to the same model produce a single edge. SQL mode no longer reads or writes `.dlin_cache/extraction_cache.json`. The `--cache-dir`, `--no-cache` and `--refresh-cache` flags have no effect in SQL mode and still control manifest and column lineage caching (#263)

#### Column-level lineage

- Column lineage JSON changes without a compatibility layer, including MCP results. Upstream uses `output.columns[]` and replaces `traced_columns` and `total_columns` with `listed_columns`, `value_complete_columns` and `value_partial_columns`. Downstream reports `confirmed`, `possible` and `exploration`. See both column subcommands' `--help` for the schemas (#244, #245, #246, #249, #250)
- Output columns follow compiled SQL order, preserve duplicates and quoting, and use dialect-specific name matching. Columns declared only in YAML are omitted from the output and produce a non-fatal diagnostic. Ambiguous upstream column selections and ambiguous short model names are reported without choosing a match. Downstream searches still look for references by name when the input slot cannot be identified (#222, #225, #245, #246)
- The right operand of `EXCEPT` contributes no column values. Constants and failed models no longer appear as sources, and references through unmodelled FROM items stay unresolved (#243, #246, #255)
- Some failures previously reported as `parse_failure` now use `unsupported_syntax` or `unsupported_statement`. Unequal-width `VALUES` rows now fail analysis with `VALUES_ROW_ARITY_MISMATCH` (#251, #255)

#### Rust API

- The bundled `dlin-sqllineage` crate replaces the `sqllineage` git dependency and exposes a structured analysis API. The `dlin-core` column lineage and impact result types follow the new output model (#211, #215, #244, #245, #246)
- `build_graph` and `build_graph_report` take `(project_dir, project_name, files, vars)`. The old SQL extraction functions are removed, and `ManifestNode::relation_name()` is now `identifier()` (#257, #263, #264)

### Features

- Interpret dependency expressions in generic test arguments, exposure `depends_on` entries including `source()`, and YAML snapshot `relation` values (#264)
- Partial lineage retains known sources. Trace boundaries distinguish external relations and source-free outputs from incomplete stops such as failed analysis, missing definitions or cycles. Downstream reports possible impacts from finite candidate sets. Confirmed and possible paths to the same output can both be reported. It concludes “no impact” only when both impact lists are empty and exploration is complete (#244, #246)
- Results cite the SQL output, catalog or YAML evidence each conclusion relies on. Plain, Mermaid and DOT output flag conclusions that rely on a YAML column list assumed exhaustive or on a declared enforced contract (#248, #249, #250)
- Improve value tracing through supported scalar and ARRAY subqueries, `IN` and `UNNEST`, and resolve positional column aliases and aliased joins where the dialect and output shape permit. Support `TABLE` when the relation can be identified, and strict `BY NAME` forms in BigQuery and `UNION BY NAME` in DuckDB and Snowflake when both sides have the same known, unique names (#226, #252, #254, #255, #256)
- `dlin-core` exposes per-node dependency analysis through `NodeData::dependency_analysis`. Manifest nodes are marked `not_assessed`. The CLI does not expose this analysis yet (#258, #264)

### Bug Fixes

- Exposures report their defining YAML path in SQL mode and `original_file_path` in manifest mode. Generic test definitions under `tests/generic/` are loaded as macros, and macro files can contain `{% test %}` blocks. Definition files no longer appear as singular test nodes (#264, #265, #266)
- Match upstream manifest relations using the adapter's rendered `relation_name` when available and usable, including ClickHouse two-part names (#257)
- Fix name resolution for joins, `LATERAL` scopes, unaliased derived tables and star modifiers. A `LATERAL` scoping fix prevents a stack overflow. Unsupported expressions such as lambdas now leave lineage partial, and relation reads include subqueries in more clauses (#227, #231, #233, #238, #251, #253, #256)

### Performance

- SQL mode parses macro files once per run, avoiding recompilation for each model. In the interpreter benchmark on six real projects, median elapsed time for `summary --source sql --no-cache -o json --quiet` was 0.17–0.95× the v0.2.6 baseline (#262, #263)

### Known Limitations

- Plain, Mermaid and DOT output and JSON `known[].table` can omit the source prefix. For example, a column from `raw.orders` can appear as `orders.id` in a path and be hard to distinguish from the `orders` model. The boundary shows `raw.orders` in full. A fix is planned for the next release

## [0.2.6] - 2026-10-25

### Bug Fixes

- Keep column-lineage results available when individual columns cannot be resolved, reporting those columns as indeterminate while still surfacing actual analysis errors (#188)
- Avoid adding dependencies from unused project macros when SQL rendering fails, while preserving recovery from reachable macros and runtime-dependent branches; show the directed path when a real lineage cycle is detected (#200)
- Treat calls and chained access on known dbt runtime objects, such as `adapter.get_relation()`, as runtime placeholders so they do not unnecessarily fail SQL rendering (#200)

### Performance

- Reduce manifest-loading overhead for commands such as `check-manifest` and column lineage (#187)

### Known Limitations

- The previous release's whole-prefix recovery incidentally included some dependencies from macros selected through `adapter.dispatch()`. Scoped recovery can omit those dependencies; dispatch-aware macro resolution is not yet supported.

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
