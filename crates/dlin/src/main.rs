use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;
use path_slash::PathExt as _;
use polyglot_sql::{DialectType, Schema as _};

mod cli;

use cli::{
    CheckManifestArgs, CheckManifestOutputFormat, Cli, Command, DebugCommand, DebugOutputFormat,
    Direction, ErrorFormat, GraphArgs, GroupBy, ListArgs, SourceType, SummaryArgs,
    SummaryOutputFormat,
};
use dlin_core::graph;
use dlin_core::input;
use dlin_core::parser;
use dlin_core::render;

/// Reset SIGPIPE to default behavior so broken pipes terminate the process
/// silently instead of causing panics. Rust's runtime sets SIG_IGN on SIGPIPE,
/// which turns pipe closures into EPIPE errors that panic via `.unwrap()`.
#[cfg(unix)]
fn reset_sigpipe() {
    // Safety: signal() is a standard POSIX function. Restoring SIG_DFL for
    // SIGPIPE is safe and matches the expected behavior of CLI tools.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(tarpaulin_include))]
fn main() {
    #[cfg(unix)]
    reset_sigpipe();

    let cli = Cli::parse();

    // Set error format before anything else so warnings/errors use it
    dlin_core::set_error_format_json(cli.error_format == ErrorFormat::Json);

    let result = match cli.command {
        Command::Graph(args) => {
            dlin_core::set_quiet(args.quiet);
            run_graph_command(args)
        }
        Command::List(args) => {
            dlin_core::set_quiet(args.quiet);
            run_list_command(args)
        }
        Command::Summary(args) => {
            dlin_core::set_quiet(args.quiet);
            run_summary_command(args)
        }
        Command::CheckManifest(args) => {
            dlin_core::set_quiet(args.quiet);
            run_check_manifest_command(args)
        }
        Command::ColumnLineage {
            model,
            column,
            dialect,
            project_dir,
            manifest_path,
            cache_dir,
            no_cache,
            refresh_cache,
            quiet,
        } => {
            dlin_core::set_quiet(quiet);
            run_column_lineage_command(
                model,
                &column,
                dialect,
                &project_dir,
                manifest_path.as_ref(),
                cache_dir.as_deref(),
                no_cache,
                refresh_cache,
            )
        }
        Command::ColumnImpact {
            model,
            column,
            dialect,
            project_dir,
            manifest_path,
            cache_dir,
            no_cache,
            refresh_cache,
            quiet,
        } => {
            dlin_core::set_quiet(quiet);
            run_column_impact_command(
                &model,
                &column,
                dialect,
                &project_dir,
                manifest_path.as_ref(),
                cache_dir.as_deref(),
                no_cache,
                refresh_cache,
            )
        }
        Command::Debug(args) => run_debug_command(args),
        Command::Impact {
            model,
            project_dir,
            output,
            source,
            manifest_path,
            cache_dir,
            no_cache,
            refresh_cache,
            quiet,
        } => {
            dlin_core::set_quiet(quiet);
            let stdin_lines = input::read_stdin_lines();
            let mut raw_inputs = model;
            raw_inputs.extend(stdin_lines);
            if raw_inputs.is_empty() {
                Err(anyhow::anyhow!(
                    "no model names provided (specify as arguments or via stdin)"
                ))
            } else {
                run_impact_command(
                    raw_inputs,
                    &project_dir,
                    &output,
                    &source,
                    manifest_path.as_ref(),
                    cache_dir.as_deref(),
                    no_cache,
                    refresh_cache,
                )
            }
        }
    };

    if let Err(err) = result {
        let diag = dlin_core::Diagnostic::from_error(&err);
        eprintln!("{}", dlin_core::format_diagnostic(&diag));
        std::process::exit(1);
    }
}

/// Run the `graph` subcommand
#[cfg(not(tarpaulin_include))]
fn run_graph_command(args: GraphArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let refresh_cache = args.refresh_cache;
    let project_dir = args.project_dir.canonicalize().unwrap_or(args.project_dir);

    // Validate flag combinations before building DAG
    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, manifest) = build_dag(
        &project_dir,
        &args.source,
        args.manifest_path.as_ref(),
        cache_dir.as_deref(),
        no_cache,
        refresh_cache,
    )?;

    // Merge CLI positional args and stdin, then resolve file paths to node names
    let stdin_lines = input::read_stdin_lines();
    let mut raw_inputs = args.model;
    raw_inputs.extend(stdin_lines);
    let models = if input::has_path_like_input(&raw_inputs) {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let project = parser::project::DbtProject::load(&project_dir)?;
        let resolved_paths = project.resolve_paths(&project_dir);
        input::resolve_stdin_inputs(&raw_inputs, &dag, &resolved_paths, &project_dir, &cwd)
    } else {
        raw_inputs
    };

    // Parse selectors
    let selectors = args
        .select
        .as_deref()
        .map(graph::filter::parse_selectors)
        .unwrap_or_default();

    // Filter graph
    let transitive = !args.no_transitive;
    let filtered = graph::filter::filter_graph(
        &dag,
        &models,
        args.upstream,
        args.downstream,
        &selectors,
        transitive,
    )?;

    // Apply node-type filter (default: all types; use --node-type to restrict)
    let type_names = graph::filter::resolve_node_types(args.node_types);
    for t in &graph::filter::validate_node_type_names(&type_names) {
        dlin_core::warn!(
            "unknown node type '{}'. Known types: {}",
            t,
            graph::filter::KNOWN_NODE_TYPE_LABELS.join(", ")
        );
    }
    let filtered =
        graph::filter::filter_output_node_types(&filtered, &type_names, !args.no_transitive);
    warn_sql_mode_test_limitation(
        &args.source,
        filtered
            .node_weights()
            .any(|n| n.node_type == graph::types::NodeType::Test),
    );

    // Collapse intermediate nodes if requested
    let filtered = if let Some(collapse_mode) = args.collapse {
        if args.no_transitive {
            dlin_core::warn!(
                "--collapse has no effect with --no-transitive (transitive edges are required to preserve connectivity)"
            );
            filtered
        } else {
            // Resolve focus models deterministically: look up unique_ids in the
            // original DAG first, then find those exact ids in the filtered graph.
            // This avoids re-resolving suffix-ambiguous names nondeterministically.
            let focus_unique_ids: std::collections::HashSet<String> = models
                .iter()
                .filter_map(|name| {
                    graph::filter::try_resolve_node_quiet(&dag, name)
                        .map(|idx| dag[idx].unique_id.clone())
                })
                .collect();
            let preserve: std::collections::HashSet<_> = filtered
                .node_indices()
                .filter(|&idx| focus_unique_ids.contains(&filtered[idx].unique_id))
                .collect();
            graph::filter::collapse_intermediate(&filtered, collapse_mode, &preserve)
        }
    } else {
        filtered
    };

    // Resolve JSON fields
    let json_fields =
        render::json::resolve_graph_fields(args.json_fields.as_deref(), args.json_full)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Warn if --json-fields/--json-full used with non-JSON output
    if !matches!(args.output, cli::OutputFormat::Json)
        && (args.json_fields.is_some() || args.json_full)
    {
        dlin_core::warn!(
            "--json-fields/--json-full have no effect with -o {}",
            args.output.label()
        );
    }

    // Collect SQL contents only when sql_content field is requested
    let sql_contents = if json_fields.contains("sql_content") {
        Some(collect_sql_contents_for_source(
            manifest.as_ref(),
            &project_dir,
            &filtered,
        ))
    } else {
        None
    };

    // Warn if --group-by used with unsupported output format
    if args.group_by.is_some()
        && !matches!(
            args.output,
            cli::OutputFormat::Dot | cli::OutputFormat::Mermaid
        )
    {
        dlin_core::warn!(
            "--group-by has no effect with -o {} (supported: dot, mermaid)",
            args.output.label()
        );
    }

    // Warn if --direction used with unsupported output format
    if args.direction != Direction::LR
        && !matches!(
            args.output,
            cli::OutputFormat::Dot | cli::OutputFormat::Mermaid
        )
    {
        dlin_core::warn!(
            "--direction has no effect with -o {} (supported: dot, mermaid)",
            args.output.label()
        );
    }

    // Warn if --show-columns used with unsupported output format
    if args.show_columns && !matches!(args.output, cli::OutputFormat::Mermaid) {
        dlin_core::warn!(
            "--show-columns has no effect with -o {} (supported: mermaid)",
            args.output.label()
        );
    }

    render_output(
        &args.output,
        &filtered,
        sql_contents.as_ref(),
        &json_fields,
        args.group_by,
        args.direction,
        args.show_columns,
    );

    Ok(())
}

/// Run the `list` subcommand
#[cfg(not(tarpaulin_include))]
fn run_list_command(args: ListArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let refresh_cache = args.refresh_cache;
    let project_dir = args.project_dir.canonicalize().unwrap_or(args.project_dir);

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, manifest) = build_dag(
        &project_dir,
        &args.source,
        args.manifest_path.as_ref(),
        cache_dir.as_deref(),
        no_cache,
        refresh_cache,
    )?;

    // Merge CLI positional args and stdin, then resolve file paths to node names
    let stdin_lines = input::read_stdin_lines();
    let mut raw_inputs = args.model;
    raw_inputs.extend(stdin_lines);
    let models = if input::has_path_like_input(&raw_inputs) {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let project = parser::project::DbtProject::load(&project_dir)?;
        let resolved_paths = project.resolve_paths(&project_dir);
        input::resolve_stdin_inputs(&raw_inputs, &dag, &resolved_paths, &project_dir, &cwd)
    } else {
        raw_inputs
    };

    // Parse selectors
    let selectors = args
        .select
        .as_deref()
        .map(graph::filter::parse_selectors)
        .unwrap_or_default();

    // Filter graph — when models are specified, use depth 0 (no traversal)
    let (upstream, downstream) = if models.is_empty() {
        (None, None)
    } else {
        (Some(0), Some(0))
    };
    // List output doesn't render edges, so transitive edge completion is unnecessary.
    let filtered =
        graph::filter::filter_graph(&dag, &models, upstream, downstream, &selectors, false)?;

    // Apply node-type filter (default: all types; use --node-type to restrict)
    let type_names = graph::filter::resolve_node_types(args.node_types);
    for t in &graph::filter::validate_node_type_names(&type_names) {
        dlin_core::warn!(
            "unknown node type '{}'. Known types: {}",
            t,
            graph::filter::KNOWN_NODE_TYPE_LABELS.join(", ")
        );
    }
    let filtered = graph::filter::filter_output_node_types(&filtered, &type_names, false);
    warn_sql_mode_test_limitation(
        &args.source,
        filtered
            .node_weights()
            .any(|n| n.node_type == graph::types::NodeType::Test),
    );

    // Resolve JSON fields for list
    let json_fields =
        render::list::resolve_list_fields(args.json_fields.as_deref(), args.json_full)
            .map_err(|e| anyhow::anyhow!("{}", e))?;

    if !matches!(args.output, cli::ListOutputFormat::Json)
        && (args.json_fields.is_some() || args.json_full)
    {
        dlin_core::warn!("--json-fields/--json-full have no effect with -o plain");
    }

    // Collect SQL contents only when sql_content field is requested
    let sql_contents = if json_fields.contains("sql_content") {
        Some(collect_sql_contents_for_source(
            manifest.as_ref(),
            &project_dir,
            &filtered,
        ))
    } else {
        None
    };

    render::list::render_list(&filtered, &args.output, &json_fields, sql_contents.as_ref());

    Ok(())
}

/// Build the lineage DAG from either a manifest file or by parsing SQL files.
///
/// Returns the graph and, in manifest mode, the parsed `Manifest` so that
/// callers can extract additional data (e.g. `compiled_code`) without
/// re-parsing the JSON.
#[cfg(not(tarpaulin_include))]
fn build_dag(
    project_dir: &Path,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> Result<(
    graph::types::LineageGraph,
    Option<parser::manifest::Manifest>,
)> {
    match source {
        SourceType::Manifest => {
            let resolved = resolve_manifest_path_or_default(manifest_path, project_dir)?;
            let manifest = parser::manifest::load_manifest(&resolved)?;
            let graph = parser::manifest::build_graph_from_parsed_manifest(&manifest)?;
            Ok((graph, Some(manifest)))
        }
        SourceType::Sql => {
            let project = parser::project::DbtProject::load(project_dir)?;
            let paths = project.resolve_paths(project_dir);
            let files = parser::discovery::discover_files(&paths)?;
            let graph = graph::builder::build_graph(
                project_dir,
                &files,
                cache_dir,
                no_cache,
                refresh_cache,
                &project.vars,
            )?;
            Ok((graph, None))
        }
    }
}

/// Dispatch rendering based on output format
#[cfg(not(tarpaulin_include))]
fn render_output(
    format: &cli::OutputFormat,
    graph: &graph::types::LineageGraph,
    sql_contents: Option<&HashMap<String, String>>,
    json_fields: &std::collections::HashSet<String>,
    group_by: Option<GroupBy>,
    direction: Direction,
    show_columns: bool,
) {
    match format {
        cli::OutputFormat::Ascii => render::ascii::render_ascii(graph),
        cli::OutputFormat::Dot => render::dot::render_dot(graph, group_by, direction),
        cli::OutputFormat::Json => render::json::render_json(graph, sql_contents, json_fields),
        cli::OutputFormat::Mermaid => {
            render::mermaid::render_mermaid(graph, group_by, direction, show_columns)
        }
        cli::OutputFormat::Plain => render::plain::render_plain(graph),
        cli::OutputFormat::Svg => render::svg::render_svg(graph),
        cli::OutputFormat::Html => render::html::render_html(graph),
    }
}

/// Collect SQL contents based on the data source.
///
/// - **manifest** (`Some`): reads `compiled_code` from the already-parsed manifest.
///   Users must run `dbt compile` beforehand so the manifest contains compiled SQL.
/// - **sql** (`None`): reads raw SQL files from disk.
#[cfg(not(tarpaulin_include))]
fn collect_sql_contents_for_source(
    manifest: Option<&parser::manifest::Manifest>,
    project_dir: &Path,
    graph: &graph::types::LineageGraph,
) -> HashMap<String, String> {
    match manifest {
        Some(m) => m.collect_sql_contents(),
        None => collect_sql_contents(graph, project_dir),
    }
}

/// Collect SQL file contents for all nodes that have a `.sql` file_path.
/// Nodes whose file_path points to a non-SQL file (e.g. YAML schema files
/// for generic tests) are skipped.
#[cfg(not(tarpaulin_include))]
fn collect_sql_contents(
    graph: &graph::types::LineageGraph,
    project_dir: &Path,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for idx in graph.node_indices() {
        let node = &graph[idx];
        if let Some(ref rel_path) = node.file_path {
            let is_sql = rel_path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"));
            if !is_sql {
                continue;
            }
            let full_path = project_dir.join(rel_path);
            match std::fs::read_to_string(&full_path) {
                Ok(content) => {
                    map.insert(node.unique_id.clone(), content);
                }
                Err(e) => {
                    dlin_core::warn!("could not read {}: {}", full_path.display(), e);
                }
            }
        }
    }
    map
}

/// Run the `impact` subcommand
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
fn run_impact_command(
    raw_inputs: Vec<String>,
    project_dir: &Path,
    output: &cli::ImpactOutputFormat,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> Result<()> {
    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    validate_source_flags(source, manifest_path)?;
    let (dag, _manifest) = build_dag(
        &project_dir,
        source,
        manifest_path,
        cache_dir,
        no_cache,
        refresh_cache,
    )?;

    // Resolve file paths to model names (same as graph/list commands)
    let models = if input::has_path_like_input(&raw_inputs) {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let project = parser::project::DbtProject::load(&project_dir)?;
        let resolved_paths = project.resolve_paths(&project_dir);
        input::resolve_stdin_inputs(&raw_inputs, &dag, &resolved_paths, &project_dir, &cwd)
    } else {
        raw_inputs
    };

    let reports: Vec<_> = models
        .iter()
        .filter_map(|model| {
            let source_idx = graph::filter::try_resolve_node(&dag, model)?;
            Some(graph::impact::compute_impact(&dag, source_idx))
        })
        .collect();

    if reports.is_empty() {
        anyhow::bail!("no models found matching: {}", models.join(", "));
    }

    warn_sql_mode_test_limitation(source, reports.iter().any(|r| r.affected_tests > 0));

    match output {
        cli::ImpactOutputFormat::Text => {
            for report in &reports {
                render::impact::render_impact_text(report);
            }
        }
        cli::ImpactOutputFormat::Json => render::impact::render_impact_json(&reports),
    }

    Ok(())
}

/// Warn when sql mode is used and the output involves test nodes, since
/// YAML-defined generic tests are inferred from declarations only.
#[cfg(not(tarpaulin_include))]
fn warn_sql_mode_test_limitation(source: &SourceType, has_tests: bool) {
    if matches!(source, SourceType::Sql) && has_tests {
        dlin_core::warn!(
            "sql mode infers generic tests from YAML declarations; \
             test IDs are dlin-specific and do not match dbt's naming. \
             Use --source manifest for exact dependency resolution"
        );
    }
}

/// Validate that --source and --manifest-path flags are consistent.
#[cfg(not(tarpaulin_include))]
fn validate_source_flags(source: &SourceType, manifest_path: Option<&PathBuf>) -> Result<()> {
    if let SourceType::Sql = source
        && manifest_path.is_some()
    {
        anyhow::bail!(
            "--manifest-path cannot be used with --source sql; did you mean --source manifest?"
        );
    }
    Ok(())
}

/// Resolve manifest_path, falling back to `<project_dir>/target/manifest.json` when not specified.
#[cfg(not(tarpaulin_include))]
fn resolve_manifest_path_or_default(
    manifest_path: Option<&PathBuf>,
    project_dir: &Path,
) -> Result<PathBuf> {
    match manifest_path {
        Some(p) => resolve_manifest_path(p),
        None => {
            let default = project_dir.join("target").join("manifest.json");
            if default.exists() {
                Ok(default)
            } else {
                anyhow::bail!(
                    "No manifest.json found at {}. Use --manifest-path or run `dbt compile` first.",
                    default.display()
                );
            }
        }
    }
}

/// Resolve the manifest path from the --manifest-path argument.
/// If the path is a directory, look for `target/manifest.json` inside it.
/// If it's a file, use it directly.
#[cfg(not(tarpaulin_include))]
fn resolve_manifest_path(manifest_arg: &Path) -> Result<PathBuf> {
    if manifest_arg.is_dir() {
        let candidate = manifest_arg.join("target").join("manifest.json");
        if candidate.exists() {
            Ok(candidate)
        } else {
            anyhow::bail!(
                "No manifest.json found at {}. Expected target/manifest.json in the directory.",
                candidate.display()
            );
        }
    } else if manifest_arg.exists() {
        Ok(manifest_arg.to_path_buf())
    } else {
        anyhow::bail!("Manifest path does not exist: {}", manifest_arg.display());
    }
}

/// Run the `column-lineage` subcommand
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
fn run_column_lineage_command(
    models: Vec<String>,
    columns: &[String],
    dialect: Option<DialectType>,
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> Result<()> {
    let dialect = dialect.unwrap_or(DialectType::Generic);

    if models.is_empty() {
        anyhow::bail!("no model names provided (specify as arguments)");
    }

    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    let resolved = resolve_manifest_path_or_default(manifest_path, &project_dir)?;
    let manifest = parser::manifest::load_manifest(&resolved)?;

    let mut cache = if no_cache {
        graph::column_lineage::ColumnLineageCache::disabled()
    } else if refresh_cache {
        graph::column_lineage::ColumnLineageCache::fresh(&project_dir, cache_dir)
    } else {
        graph::column_lineage::ColumnLineageCache::load(&project_dir, cache_dir)
    };

    let column_filter: HashSet<&str> = columns.iter().map(|s| s.as_str()).collect();

    let reports: Vec<_> = models
        .iter()
        .map(|model| {
            let mut report = graph::column_lineage::compute_cross_model_column_lineage(
                &manifest, model, dialect, &mut cache,
            );
            if !column_filter.is_empty() {
                report
                    .columns
                    .retain(|entry| column_filter.contains(entry.column.as_str()));
                // Only recompute counts and filter errors when analysis was actually attempted.
                // total_columns==0 indicates a load error (model not found, no
                // compiled_code, etc.) — preserve the zero so callers can
                // distinguish "nothing requested" from "nothing found".
                if report.total_columns > 0 {
                    // Remove per-column errors for columns outside the filter and
                    // the stale partial-failure summary; regenerate the summary below.
                    // Global errors (e.g. SQL parse failures) are always preserved.
                    report.errors.retain(|err| {
                        if let Some(rest) = err.strip_prefix("column '")
                            && let Some(col_end) = rest.find('\'')
                        {
                            return column_filter.contains(&rest[..col_end]);
                        }
                        // Drop the stale partial-failure summary (will be regenerated).
                        if err.starts_with("model '")
                            && err.contains("': traced ")
                            && err.ends_with(" failed)")
                        {
                            return false;
                        }
                        // Preserve all other errors (global analysis failures, SQL parse errors, etc.)
                        true
                    });
                    report.traced_columns = report.columns.len();
                    report.total_columns = column_filter.len();

                    // When there are no global errors, explicitly flag requested columns
                    // that are absent from both the output and per-column errors.
                    let has_global_errors =
                        report.errors.iter().any(|err| !err.starts_with("column '"));
                    if !has_global_errors {
                        let mut sorted_cols: Vec<&str> = column_filter.iter().copied().collect();
                        sorted_cols.sort_unstable();
                        for col in sorted_cols {
                            let in_output = report.columns.iter().any(|c| c.column == col);
                            let col_error_prefix = format!("column '{}': ", col);
                            let has_col_error = report
                                .errors
                                .iter()
                                .any(|err| err.starts_with(&col_error_prefix));
                            if !in_output && !has_col_error {
                                report
                                    .errors
                                    .push(format!("column '{}': not found in model output", col));
                            }
                        }
                    }

                    // Regenerate partial-failure summary only when per-column errors exist.
                    // When failures are due to global errors (e.g. SQL parse failure), the
                    // summary would be misleading — the global error itself is sufficient.
                    let has_per_col_errors =
                        report.errors.iter().any(|err| err.starts_with("column '"));
                    let failed = report.total_columns - report.traced_columns;
                    if failed > 0 && has_per_col_errors {
                        report.errors.insert(
                            0,
                            format!(
                                "model '{}': traced {}/{} columns ({} failed)",
                                report.model, report.traced_columns, report.total_columns, failed,
                            ),
                        );
                    }
                }
            }
            report
        })
        .collect();

    // Print warnings for errors
    let mut has_errors = false;
    for report in &reports {
        for err in &report.errors {
            dlin_core::warn!("{}", err);
            has_errors = true;
        }
    }

    // Output JSON
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let pretty = std::io::IsTerminal::is_terminal(&stdout);
    let res = if pretty {
        serde_json::to_writer_pretty(&mut out, &reports)
    } else {
        serde_json::to_writer(&mut out, &reports)
    };
    if let Err(e) = res {
        if e.io_error_kind() != Some(std::io::ErrorKind::BrokenPipe) {
            return Err(anyhow::anyhow!(e));
        }
    } else if let Err(e) = std::io::Write::write_all(&mut out, b"\n")
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }

    cache.save();

    if has_errors {
        anyhow::bail!("column lineage analysis completed with errors");
    }
    Ok(())
}

/// Run the `column-impact` subcommand
#[cfg(not(tarpaulin_include))]
#[allow(clippy::too_many_arguments)]
fn run_column_impact_command(
    model: &str,
    columns: &[String],
    dialect: Option<DialectType>,
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> Result<()> {
    let dialect = dialect.unwrap_or(DialectType::Generic);

    if columns.is_empty() {
        anyhow::bail!("no columns specified (use --column)");
    }

    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    let resolved = resolve_manifest_path_or_default(manifest_path, &project_dir)?;
    let manifest = parser::manifest::load_manifest(&resolved)?;

    let mut cache = if no_cache {
        graph::column_lineage::ColumnLineageCache::disabled()
    } else if refresh_cache {
        graph::column_lineage::ColumnLineageCache::fresh(&project_dir, cache_dir)
    } else {
        graph::column_lineage::ColumnLineageCache::load(&project_dir, cache_dir)
    };

    let reports: Vec<_> = columns
        .iter()
        .map(|col| {
            graph::column_lineage::compute_column_impact(&manifest, model, col, dialect, &mut cache)
        })
        .collect();

    // Print warnings for errors
    let mut has_errors = false;
    for report in &reports {
        for err in &report.errors {
            dlin_core::warn!("{}", err);
            has_errors = true;
        }
    }

    // Output JSON
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let pretty = std::io::IsTerminal::is_terminal(&stdout);
    let res = if pretty {
        serde_json::to_writer_pretty(&mut out, &reports)
    } else {
        serde_json::to_writer(&mut out, &reports)
    };
    if let Err(e) = res {
        if e.io_error_kind() != Some(std::io::ErrorKind::BrokenPipe) {
            return Err(anyhow::anyhow!(e));
        }
    } else if let Err(e) = std::io::Write::write_all(&mut out, b"\n")
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(e.into());
    }

    cache.save();

    if has_errors {
        anyhow::bail!("column impact analysis completed with errors");
    }
    Ok(())
}

/// Run the `summary` subcommand
#[cfg(not(tarpaulin_include))]
fn run_summary_command(args: SummaryArgs) -> Result<()> {
    let project_dir = args.project_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot resolve project directory '{}': {}",
            args.project_dir.display(),
            e
        )
    })?;

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let project = parser::project::DbtProject::load(&project_dir)?;
    let vars_count = project.vars.len();
    let project_name = project.name.clone();

    let (dag, _manifest) = build_dag(
        &project_dir,
        &args.source,
        args.manifest_path.as_ref(),
        args.cache_dir.as_deref(),
        args.no_cache,
        args.refresh_cache,
    )?;

    let node_counts = render::summary::count_nodes(&dag);
    let edge_count = dag.edge_count();

    // Check manifest freshness (best-effort)
    let manifest_status =
        check_manifest_freshness(&project_dir, args.manifest_path.as_ref(), &project);

    let report = render::summary::SummaryReport {
        project_name,
        source_mode: match args.source {
            SourceType::Sql => "sql".to_string(),
            SourceType::Manifest => "manifest".to_string(),
        },
        node_counts,
        edge_count,
        vars_count,
        manifest_status,
    };

    match args.output {
        SummaryOutputFormat::Text => render::summary::render_summary_text_stdout(&report),
        SummaryOutputFormat::Json => render::summary::render_summary_json_stdout(&report),
    }

    Ok(())
}

/// Collect file paths referenced in manifest that no longer exist on disk.
fn find_deleted_manifest_files(
    manifest: &parser::manifest::Manifest,
    project_dir: &Path,
) -> Vec<String> {
    let mut deleted: Vec<String> = manifest
        .collect_file_paths()
        .into_iter()
        .filter(|p| !project_dir.join(p).exists())
        .collect();
    deleted.sort();
    deleted
}

/// Check manifest.json freshness, returning None if manifest is irrelevant.
#[cfg(not(tarpaulin_include))]
fn check_manifest_freshness(
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    project: &parser::project::DbtProject,
) -> Option<render::summary::ManifestStatus> {
    let not_found = render::summary::ManifestStatus {
        found: false,
        is_stale: false,
        stale_file_count: 0,
        stale_files: vec![],
        deleted_file_count: 0,
        deleted_files: vec![],
    };

    let resolved = match resolve_manifest_path_or_default(manifest_path, project_dir) {
        Ok(p) => p,
        Err(_) => return Some(not_found),
    };

    let manifest_mtime = match std::fs::metadata(&resolved).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return Some(not_found),
    };

    let paths = project.resolve_paths(project_dir);
    let files = match parser::discovery::discover_files(&paths) {
        Ok(f) => f,
        Err(_) => return None,
    };

    let mut stale_files: Vec<String> = Vec::new();
    let all_files = files
        .model_sql_files
        .iter()
        .chain(files.macro_sql_files.iter())
        .chain(files.seed_files.iter())
        .chain(files.snapshot_sql_files.iter())
        .chain(files.test_sql_files.iter())
        .chain(files.yaml_files.iter());

    for file in all_files {
        if let Ok(meta) = std::fs::metadata(file)
            && let Ok(mtime) = meta.modified()
            && mtime > manifest_mtime
        {
            let rel = file.strip_prefix(project_dir).unwrap_or(file);
            stale_files.push(rel.to_slash_lossy().into_owned());
        }
    }
    stale_files.sort();

    // Check for deleted files: paths referenced in manifest but missing on disk
    let deleted_files = match parser::manifest::load_manifest(&resolved) {
        Ok(manifest) => find_deleted_manifest_files(&manifest, project_dir),
        Err(e) => {
            dlin_core::warn!("cannot parse manifest.json for deleted-file check: {}", e);
            return None;
        }
    };

    let is_stale = !stale_files.is_empty() || !deleted_files.is_empty();
    Some(render::summary::ManifestStatus {
        found: true,
        is_stale,
        stale_file_count: stale_files.len(),
        stale_files,
        deleted_file_count: deleted_files.len(),
        deleted_files,
    })
}

/// Run the `check-manifest` subcommand
#[cfg(not(tarpaulin_include))]
fn run_check_manifest_command(args: CheckManifestArgs) -> Result<()> {
    let project_dir = args.project_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot resolve project directory '{}': {}",
            args.project_dir.display(),
            e
        )
    })?;

    let manifest_path =
        resolve_manifest_path_or_default(args.manifest_path.as_ref(), &project_dir)?;

    let manifest_mtime = std::fs::metadata(&manifest_path)
        .and_then(|m| m.modified())
        .map_err(|e| {
            anyhow::anyhow!(
                "cannot read manifest.json at {}: {}",
                manifest_path.display(),
                e
            )
        })?;

    // Discover project files
    let project = parser::project::DbtProject::load(&project_dir)?;
    let paths = project.resolve_paths(&project_dir);
    let files = parser::discovery::discover_files(&paths)?;

    // Collect all SQL/YAML files and compare mtimes
    let mut stale_files: Vec<PathBuf> = Vec::new();
    let all_files = files
        .model_sql_files
        .iter()
        .chain(files.macro_sql_files.iter())
        .chain(files.seed_files.iter())
        .chain(files.snapshot_sql_files.iter())
        .chain(files.test_sql_files.iter())
        .chain(files.yaml_files.iter());

    for file in all_files {
        match std::fs::metadata(file) {
            Ok(meta) => {
                if let Ok(mtime) = meta.modified()
                    && mtime > manifest_mtime
                {
                    let rel = file.strip_prefix(&project_dir).unwrap_or(file);
                    stale_files.push(rel.to_path_buf());
                }
            }
            Err(e) => {
                dlin_core::warn!("cannot read metadata for {}: {}", file.display(), e);
                // Treat unreadable files as stale to fail safe
                let rel = file.strip_prefix(&project_dir).unwrap_or(file);
                stale_files.push(rel.to_path_buf());
            }
        }
    }

    stale_files.sort();

    // Check for deleted files: paths referenced in manifest but missing on disk
    let manifest = parser::manifest::load_manifest(&manifest_path)?;
    let deleted_files: Vec<PathBuf> = find_deleted_manifest_files(&manifest, &project_dir)
        .into_iter()
        .map(PathBuf::from)
        .collect();

    let is_stale = !stale_files.is_empty() || !deleted_files.is_empty();

    match args.output {
        CheckManifestOutputFormat::Text => {
            if !args.quiet {
                if is_stale {
                    let mut parts = Vec::new();
                    if !stale_files.is_empty() {
                        parts.push(format!(
                            "{} file{} newer",
                            stale_files.len(),
                            if stale_files.len() == 1 { "" } else { "s" }
                        ));
                    }
                    if !deleted_files.is_empty() {
                        parts.push(format!("{} deleted", deleted_files.len(),));
                    }
                    println!("manifest.json is stale ({}):", parts.join(", "));
                    if !stale_files.is_empty() {
                        println!("Files newer than manifest:");
                        for f in &stale_files {
                            println!("  {}", f.display());
                        }
                    }
                    if !deleted_files.is_empty() {
                        println!("Files referenced in manifest but not found:");
                        for f in &deleted_files {
                            println!("  {}", f.display());
                        }
                    }
                } else {
                    println!("manifest.json is up-to-date");
                }
            }
        }
        CheckManifestOutputFormat::Json => {
            use std::io::Write;
            let result = serde_json::json!({
                "manifest_path": manifest_path.to_string_lossy(),
                "is_stale": is_stale,
                "stale_file_count": stale_files.len(),
                "stale_files": stale_files.iter().map(|f| f.to_slash_lossy().into_owned()).collect::<Vec<_>>(),
                "deleted_file_count": deleted_files.len(),
                "deleted_files": deleted_files.iter().map(|f| f.to_slash_lossy().into_owned()).collect::<Vec<_>>(),
            });
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let res = if std::io::IsTerminal::is_terminal(&stdout) {
                serde_json::to_writer_pretty(&mut out, &result)
            } else {
                serde_json::to_writer(&mut out, &result)
            };
            if let Err(e) = res {
                if e.io_error_kind() != Some(std::io::ErrorKind::BrokenPipe) {
                    return Err(anyhow::anyhow!(e));
                }
            } else if let Err(e) = writeln!(out)
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e.into());
            }
        }
    }

    if is_stale {
        // Flush stdout before exiting to ensure buffered output is written
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::process::exit(1);
    }
    Ok(())
}

/// Read SQL input from positional argument or stdin.
#[cfg(not(tarpaulin_include))]
fn read_sql_input(sql: Option<&str>) -> Result<String> {
    if let Some(s) = sql {
        return Ok(s.to_string());
    }
    // Read from stdin
    let mut stdin = std::io::stdin();
    if std::io::IsTerminal::is_terminal(&stdin) {
        anyhow::bail!("provide SQL as an argument or via stdin");
    }
    let mut buf = String::new();
    std::io::Read::read_to_string(&mut stdin, &mut buf)?;
    if buf.is_empty() {
        anyhow::bail!("no SQL input received from stdin");
    }
    Ok(buf)
}

/// Run the `debug` subcommand
#[cfg(not(tarpaulin_include))]
fn run_debug_command(args: cli::DebugArgs) -> Result<()> {
    match args.command {
        DebugCommand::ParseSql(args) => run_debug_parse_sql(args),
        DebugCommand::TraceColumn(args) => run_debug_trace_column(args),
    }
}

/// Run `debug parse-sql`
#[cfg(not(tarpaulin_include))]
fn run_debug_parse_sql(args: cli::DebugParseSqlArgs) -> Result<()> {
    use std::io::Write;

    let sql = read_sql_input(args.sql.as_deref())?;
    let expr = polyglot_sql::parse_one(&sql, args.dialect)
        .map_err(|e| anyhow::anyhow!("parse error: {}", e))?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    match args.format {
        DebugOutputFormat::Sql => {
            let regenerated = polyglot_sql::generate(&expr, args.dialect)
                .map_err(|e| anyhow::anyhow!("generate error: {}", e))?;
            writeln!(out, "{}", regenerated)?;
        }
        DebugOutputFormat::Ast => {
            writeln!(out, "{:#?}", expr)?;
        }
        DebugOutputFormat::Json => {
            let pretty = std::io::IsTerminal::is_terminal(&stdout);
            let res = if pretty {
                serde_json::to_writer_pretty(&mut out, &expr)
            } else {
                serde_json::to_writer(&mut out, &expr)
            };
            if let Err(e) = res {
                if e.io_error_kind() != Some(std::io::ErrorKind::BrokenPipe) {
                    return Err(anyhow::anyhow!(e));
                }
            } else if let Err(e) = writeln!(out)
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e.into());
            }
        }
    }
    Ok(())
}

/// Parse a schema string like "table1:col1,col2;table2:col3,col4" into a MappingSchema.
fn parse_schema_string(schema_str: &str) -> Result<polyglot_sql::MappingSchema> {
    let mut schema = polyglot_sql::MappingSchema::new();
    for table_def in schema_str.split(';') {
        let table_def = table_def.trim();
        if table_def.is_empty() {
            continue;
        }
        let (table_name, cols_str) = table_def.split_once(':').ok_or_else(|| {
            anyhow::anyhow!(
                "invalid schema format '{}': expected table:col1,col2",
                table_def
            )
        })?;
        let columns: Vec<(String, polyglot_sql::expressions::DataType)> = cols_str
            .split(',')
            .map(|c| c.trim())
            .filter(|c| !c.is_empty())
            .map(|c| (c.to_string(), polyglot_sql::expressions::DataType::Unknown))
            .collect();
        if columns.is_empty() {
            anyhow::bail!(
                "invalid schema format '{}': table has no columns",
                table_name.trim()
            );
        }
        schema
            .add_table(table_name.trim(), &columns, None)
            .map_err(|e| anyhow::anyhow!("schema error: {}", e))?;
    }
    Ok(schema)
}

/// Run `debug trace-column`
#[cfg(not(tarpaulin_include))]
fn run_debug_trace_column(args: cli::DebugTraceColumnArgs) -> Result<()> {
    use std::io::Write;

    let sql = read_sql_input(args.sql.as_deref())?;
    let mut expr = polyglot_sql::parse_one(&sql, args.dialect)
        .map_err(|e| anyhow::anyhow!("parse error: {}", e))?;

    let schema = args
        .schema
        .as_deref()
        .map(parse_schema_string)
        .transpose()?;

    // Expand CTE stars if schema is provided
    if let Some(ref s) = schema {
        polyglot_sql::lineage::expand_cte_stars(&mut expr, Some(s as &dyn polyglot_sql::Schema));
    }

    let lineage_result = if let Some(ref s) = schema {
        polyglot_sql::lineage::lineage_with_schema(
            &args.column,
            &expr,
            Some(s as &dyn polyglot_sql::Schema),
            Some(args.dialect),
            false,
        )
        .or_else(|err| {
            dlin_core::warn!(
                "lineage_with_schema failed: {}, falling back to schema-less lineage",
                err
            );
            polyglot_sql::lineage::lineage(&args.column, &expr, Some(args.dialect), false)
        })
    } else {
        polyglot_sql::lineage::lineage(&args.column, &expr, Some(args.dialect), false)
    };

    match lineage_result {
        Ok(node) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let pretty = std::io::IsTerminal::is_terminal(&stdout);
            let res = if pretty {
                serde_json::to_writer_pretty(&mut out, &node)
            } else {
                serde_json::to_writer(&mut out, &node)
            };
            if let Err(e) = res {
                if e.io_error_kind() != Some(std::io::ErrorKind::BrokenPipe) {
                    return Err(anyhow::anyhow!(e));
                }
            } else if let Err(e) = writeln!(out)
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e.into());
            }
        }
        Err(e) => {
            return Err(anyhow::anyhow!("lineage error: {}", e));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use polyglot_sql::Schema;

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    #[test]
    fn test_parse_schema_string_single_table() {
        let schema = parse_schema_string("t:a,b,c").unwrap();
        let cols = schema.column_names("t").unwrap();
        assert_eq!(sorted(cols), vec!["a", "b", "c"]);
    }

    #[test]
    fn test_parse_schema_string_multiple_tables() {
        let schema = parse_schema_string("orders:id,amount;customers:id,name").unwrap();
        let order_cols = schema.column_names("orders").unwrap();
        assert_eq!(sorted(order_cols), vec!["amount", "id"]);
        let cust_cols = schema.column_names("customers").unwrap();
        assert_eq!(sorted(cust_cols), vec!["id", "name"]);
    }

    #[test]
    fn test_parse_schema_string_whitespace_tolerance() {
        let schema = parse_schema_string(" t : a , b ; u : x ").unwrap();
        let cols = schema.column_names("t").unwrap();
        assert_eq!(sorted(cols), vec!["a", "b"]);
        let cols2 = schema.column_names("u").unwrap();
        assert_eq!(cols2, vec!["x"]);
    }

    #[test]
    fn test_parse_schema_string_empty() {
        let schema = parse_schema_string("").unwrap();
        assert!(schema.column_names("t").is_err());
    }

    #[test]
    fn test_parse_schema_string_invalid_format() {
        let result = parse_schema_string("no_colon");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_schema_string_empty_columns_rejected() {
        // "t:" (no columns) and "t:," (all empty segments) should error
        assert!(parse_schema_string("t:").is_err());
        assert!(parse_schema_string("t:,").is_err());
    }

    #[test]
    fn test_parse_schema_string_consecutive_commas_ignored() {
        // "t:a,,b" — the empty segment is dropped, result has only a and b
        let schema = parse_schema_string("t:a,,b").unwrap();
        let cols = schema.column_names("t").unwrap();
        assert_eq!(sorted(cols), vec!["a", "b"]);
    }
}
