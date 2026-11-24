# Changelog

## [0.3.0] - 2026-11-24

### Breaking Changes

#### Model-level lineage

- SQL mode now interprets Jinja to find dependencies. It sets `execute` to false, skips `statement` blocks, follows branches selected by known values, and keeps literal dependencies from branches whose conditions are unknown. Dependency names that require runtime values stay unresolved, and repeated references produce one edge (#262, #263)
- In SQL mode, `env_var()` defaults can now select branches. Calls without a default and calls for `DBT_ENV_SECRET*` stay unknown. The environment is still ignored (#262, #263)
- Missing `ref()` and `source()` targets are now reported as `unresolved_ref` and `unresolved_source` reasons on the referencing node. Graph-building commands print a `dependency_analysis_summary` on stderr when there are unresolved reasons or project gaps. Use `--verbose` for individual diagnostics, `--error-format json` for typed fields, or `--quiet` to suppress them (#281)
- SQL mode no longer uses `.dlin_cache/extraction_cache.json`. Cache flags still apply to manifest mode and column lineage (#263)
- Short names prefer a model when other node types share its name. Remaining ambiguities fail with a list of candidate IDs, including `ambiguous_node_name` errors in MCP. Select a node by its exact unique ID or, for SQL files, by its path (#300)
- Impact reports exclude tests by default. Select the reported types with `--node-type` or MCP `node_types`, for example `--node-type model,test` to include tests. Paths can still pass through any node type. JSON counts for excluded types and severity when no selected node is reached are now null (#284)

#### Column-level lineage

- Column lineage JSON, including MCP results, has a new schema. Upstream lists output slots in `output.columns[]` and replaces `traced_columns` and `total_columns` with `listed_columns`, `value_complete_columns`, and `value_partial_columns`. Downstream reports `confirmed`, `possible`, and `exploration`. Both subcommands document the schemas in `--help` (#244, #245, #246, #249, #250, #297)
- List output columns in compiled SQL order, preserving duplicates and quoting. Match names using the SQL dialect's rules, report ambiguous upstream selections, and omit YAML-only columns with a diagnostic (#222, #225, #245, #246)
- Use `unsupported_syntax` or `unsupported_statement` for some diagnostics previously reported as `parse_failure`. Reject unequal-width `VALUES` rows with `VALUES_ROW_ARITY_MISMATCH` (#251, #255)

#### MCP

- Read tool results from the single compact JSON text entry in `content`. The `structuredContent` field has been removed. Impact and column lineage default to summaries, with `detail: "full"` available for paths and evidence. Request compiled SQL from `find_nodes` with `include_sql: true` (#274)

#### Rust API

- Replace the `sqllineage` Git dependency with the bundled `dlin-sqllineage` crate and its structured analysis API. The `dlin-core` column-lineage and column-impact result types follow the new output model (#211, #215, #244, #245, #246)
- Pass `(project_dir, project_name, files, vars)` to `build_graph` and `build_graph_report`. For dispatch resolution, use `build_graph_report_with_macro_resolution` with `DbtProject::macro_resolution()`. The legacy SQL extraction functions are removed, and `ManifestNode::relation_name()` is now `identifier()` (#257, #263, #264, #302)
- Use `compute_impact(&build, source_idx, &scope)` with a `GraphBuild` and `ImpactScope`. It returns `Option<ImpactReport>`. Use accessors for `impacted_nodes`, `source_node`, and `exploration`. Exploration types are in `graph::exploration` (#281, #284, #300)
- Update struct literals for `NodeData::dependency_analysis`, `DbtProject::dispatch`, and `DiscoveredFiles::macro_inventory_incomplete` (#258, #264, #302)

### Features

#### Model-level lineage

- Report typed `dependency_analysis` reasons per node in `list` and `impact` JSON, and in `graph` JSON with `--json-full` or `--json-fields dependency_analysis`. Impact reports identify the source node and include exploration gaps that could hide downstream nodes, including skipped inputs and unassessed scopes. The README documents how to check for no impact in CI (#258, #264, #281)
- Report upstream exploration gaps in focused SQL-mode graph queries, including reached nodes with unresolved dependencies within the requested depth and skipped project inputs. Output filters preserve these diagnostics. The MCP `get_lineage` tool also reports them (#300)
- Evaluate Jinja dependencies in generic test arguments, exposure `depends_on` entries, and YAML snapshot `relation` values (#264)
- Render Jinja strings returned by `var()` at call time, as [dbt Fusion](https://github.com/dbt-labs/dbt-fusion) does. Variables scoped to the root project take precedence over global variables (#301)
- Resolve macros called through the root project's namespace. Resolve explicit `adapter.dispatch()` calls into that namespace when a default implementation is available, no adapter-specific implementation competes, and no dispatch override applies (#302)

#### Column-level lineage

- Remove the experimental label. Preserve known sources for partial columns, report where tracing stopped, and distinguish confirmed downstream impacts from possible ones (#215, #244, #246, #290)
- Cite schema evidence from compiled SQL, catalogs, seed headers, or YAML. Plain, Mermaid, and DOT output flag conclusions that rely on an exhaustive YAML column list or an enforced contract (#248, #249, #250)
- Infer undeclared seed columns from CSV headers, including installed package seeds, and invalidate cached lineage when a header it relies on changes or disappears (#273, #274)
- Trace values through scalar and ARRAY subqueries, `IN`, `UNNEST`, positional column aliases, aliased joins, and `TABLE` queries. Support strict BigQuery `BY NAME` and DuckDB and Snowflake `UNION BY NAME` when both sides have the same known, unique column names (#226, #252, #254, #255, #256)
- Trace individual STRUCT fields and expand STRUCT stars, including rows or STRUCTs selected by a single array subscript. Support BigQuery `SELECT AS STRUCT` and `SELECT AS VALUE` value tables (#283, #286, #288, #289)

#### MCP

- Paginate node searches, lineage graphs, impact results, and column lineage. Report manifest freshness, grouped load warnings, and skipped unit test counts in the project summary (#274)

### Bug Fixes

- Report the defining file path for exposures and load generic test definitions under `tests/generic/` as macros, so they no longer appear as singular tests (#264, #265, #266)
- Match upstream manifest relations by the adapter's rendered `relation_name` when usable, including ClickHouse two-part names (#257)
- Check manifest freshness against installed package paths and track dependency declarations and the lockfile. Report missing packages as unverified and missing files within installed packages as stale. Skip valid dbt unit tests without warnings, including in strict mode (#274)
- Keep column lineage partial when unknown schemas prevent verification of a column's provider. These cases report an `unverified` reason and candidate providers and exit successfully in the absence of fatal errors. Confirmed ambiguities still fail (#272, #273, #285)
- Correct column resolution for joins, `LATERAL` scopes, unaliased derived tables, star modifiers, and explicit target column lists. Prevent a stack overflow in `LATERAL` queries (#227, #231, #232, #233, #238, #253, #256)
- Trace `EXCEPT` values from the left operand and include the partition and ordering columns of named or inherited windows. Stop attributing values to failed analyses or constants, and keep unresolved references partial for opaque FROM items and unsupported expressions or wildcard arguments (#243, #246, #251, #255, #277)
- Preserve source schema prefixes and match manifest sources by their physical relation names (#276)
- Resolve BigQuery `INFORMATION_SCHEMA` references with their full namespace, trace fields from unaliased `UNNEST` inputs, and recognize normalization modes such as `NFKC` in BigQuery, PostgreSQL, and Trino (#272)
- Report downstream column-lineage diagnostics in a stable order (#294)

### Performance

- Across 11 public dbt projects ([Tuva](https://github.com/tuva-health/tuva), [jaffle-shop](https://github.com/dbt-labs/jaffle-shop), [cal-itp data-infra](https://github.com/cal-itp/data-infra) and the Fivetran packages [dbt_hubspot](https://github.com/fivetran/dbt_hubspot), [dbt_jira](https://github.com/fivetran/dbt_jira), [dbt_netsuite](https://github.com/fivetran/dbt_netsuite), [dbt_quickbooks](https://github.com/fivetran/dbt_quickbooks), [dbt_salesforce](https://github.com/fivetran/dbt_salesforce), [dbt_shopify](https://github.com/fivetran/dbt_shopify), [dbt_stripe](https://github.com/fivetran/dbt_stripe), [dbt_zendesk](https://github.com/fivetran/dbt_zendesk)), `dlin list --source sql --no-cache -o json -q` took about 15–86% of the time of 0.2.6 in release builds. For example, dbt_shopify went from 557 ms to 81 ms and Tuva from 1,092 ms to 678 ms (#262, #263, #299, #304)
- Reduce the cost of tracing wide models. A single-column upstream query across eight chained models of 200 columns takes about 50 ms without a cache and 23 ms with a warm cache (#292, #293)

### Known Limitations

- In SQL mode, macro calls into installed packages, such as `fivetran_utils.union_connections()` from [dbt_fivetran_utils](https://github.com/fivetran/dbt_fivetran_utils), and `adapter.dispatch()` calls into namespaces outside the root project can leave dependencies missing. Affected nodes are marked partial with `unsupported_macro_namespace` or `unsupported_dispatch`

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
