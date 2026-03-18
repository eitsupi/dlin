use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

use dlin::cli::{self, CheckManifestArgs, CheckManifestOutputFormat, Cli, Command, ErrorFormat, GraphArgs, ListArgs, SourceType, SummaryArgs, SummaryOutputFormat};
use dlin::graph;
use dlin::input;
use dlin::parser;
use dlin::render;

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
    dlin::set_error_format_json(cli.error_format == ErrorFormat::Json);

    let result = match cli.command {
        Command::Graph(args) => {
            dlin::set_quiet(args.quiet);
            run_graph_command(args)
        }
        Command::List(args) => {
            dlin::set_quiet(args.quiet);
            run_list_command(args)
        }
        Command::Summary(args) => {
            dlin::set_quiet(args.quiet);
            run_summary_command(args)
        }
        Command::CheckManifest(args) => {
            dlin::set_quiet(args.quiet);
            run_check_manifest_command(args)
        }
        Command::ColumnLineage {
            model,
            project_dir,
            manifest_path,
            quiet,
        } => {
            dlin::set_quiet(quiet);
            run_column_lineage_command(model, &project_dir, manifest_path.as_ref())
        }
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
            dlin::set_quiet(quiet);
            let stdin_lines = input::read_stdin_lines();
            let mut raw_inputs = model;
            raw_inputs.extend(stdin_lines);
            if raw_inputs.is_empty() {
                Err(anyhow::anyhow!("no model names provided (specify as arguments or via stdin)"))
            } else {
                run_impact_command(raw_inputs, &project_dir, &output, &source, manifest_path.as_ref(), cache_dir.as_deref(), no_cache, refresh_cache)
            }
        }
    };

    if let Err(err) = result {
        eprintln!("{}", dlin::format_error(&err));
        std::process::exit(1);
    }
}

/// Run the `graph` subcommand
#[cfg(not(tarpaulin_include))]
fn run_graph_command(args: GraphArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let refresh_cache = args.refresh_cache;
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    // Validate flag combinations before building DAG
    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, manifest) = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), cache_dir.as_deref(), no_cache, refresh_cache)?;

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
    let filtered = graph::filter::filter_graph(
        &dag,
        &models,
        args.upstream,
        args.downstream,
        &selectors,
    )?;

    // Apply node-type filter (default: model,source; --node-type-all for all types)
    let type_names = graph::filter::resolve_node_types(args.node_types, args.node_type_all);
    for t in &graph::filter::validate_node_type_names(&type_names) {
        dlin::warn!("unknown node type '{}'. Known types: {}", t, graph::filter::KNOWN_NODE_TYPE_LABELS.join(", "));
    }
    let filtered = graph::filter::filter_output_node_types(&filtered, &type_names);

    // Resolve JSON fields
    let json_fields = render::json::resolve_graph_fields(
        args.json_fields.as_deref(),
        args.json_full,
    ).map_err(|e| anyhow::anyhow!("{}", e))?;

    // Warn if --json-fields/--json-full used with non-JSON output
    if !matches!(args.output, cli::OutputFormat::Json)
        && (args.json_fields.is_some() || args.json_full)
    {
        dlin::warn!("--json-fields/--json-full have no effect with -o {}", args.output.label());
    }

    // Render
    #[cfg(feature = "tui")]
    if args.interactive {
        dlin::tui::run_tui(filtered, project_dir)?;
        return Ok(());
    }

    #[cfg(not(feature = "tui"))]
    if args.interactive {
        anyhow::bail!("TUI feature not enabled. Rebuild with --features tui");
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

    render_output(&args.output, &filtered, sql_contents.as_ref(), &json_fields);

    Ok(())
}

/// Run the `list` subcommand
#[cfg(not(tarpaulin_include))]
fn run_list_command(args: ListArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let refresh_cache = args.refresh_cache;
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, manifest) = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), cache_dir.as_deref(), no_cache, refresh_cache)?;

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
    let filtered = graph::filter::filter_graph(
        &dag,
        &models,
        upstream,
        downstream,
        &selectors,
    )?;

    // Apply node-type filter (default: model,source; --node-type-all for all types)
    let type_names = graph::filter::resolve_node_types(args.node_types, args.node_type_all);
    for t in &graph::filter::validate_node_type_names(&type_names) {
        dlin::warn!("unknown node type '{}'. Known types: {}", t, graph::filter::KNOWN_NODE_TYPE_LABELS.join(", "));
    }
    let filtered = graph::filter::filter_output_node_types(&filtered, &type_names);

    // Resolve JSON fields for list
    let json_fields = render::list::resolve_list_fields(
        args.json_fields.as_deref(),
        args.json_full,
    ).map_err(|e| anyhow::anyhow!("{}", e))?;

    if !matches!(args.output, cli::ListOutputFormat::Json)
        && (args.json_fields.is_some() || args.json_full)
    {
        dlin::warn!("--json-fields/--json-full have no effect with -o plain");
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
) -> Result<(graph::types::LineageGraph, Option<parser::manifest::Manifest>)> {
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
            let graph = graph::builder::build_graph(project_dir, &files, cache_dir, no_cache, refresh_cache, &project.vars)?;
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
) {
    match format {
        cli::OutputFormat::Ascii => render::ascii::render_ascii(graph),
        cli::OutputFormat::Dot => render::dot::render_dot(graph),
        cli::OutputFormat::Json => render::json::render_json(graph, sql_contents, json_fields),
        cli::OutputFormat::Mermaid => render::mermaid::render_mermaid(graph),
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

/// Collect SQL file contents for all nodes that have a file_path.
#[cfg(not(tarpaulin_include))]
fn collect_sql_contents(
    graph: &graph::types::LineageGraph,
    project_dir: &Path,
) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for idx in graph.node_indices() {
        let node = &graph[idx];
        if let Some(ref rel_path) = node.file_path {
            let full_path = project_dir.join(rel_path);
            match std::fs::read_to_string(&full_path) {
                Ok(content) => {
                    map.insert(node.unique_id.clone(), content);
                }
                Err(e) => {
                    dlin::warn!("could not read {}: {}", full_path.display(), e);
                }
            }
        }
    }
    map
}

/// Run the `impact` subcommand
#[cfg(not(tarpaulin_include))]
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
    let (dag, _manifest) = build_dag(&project_dir, source, manifest_path, cache_dir, no_cache, refresh_cache)?;

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
        anyhow::bail!(
            "no models found matching: {}",
            models.join(", ")
        );
    }

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

/// Validate that --source and --manifest-path flags are consistent.
#[cfg(not(tarpaulin_include))]
fn validate_source_flags(source: &SourceType, manifest_path: Option<&PathBuf>) -> Result<()> {
    if let SourceType::Sql = source {
        if manifest_path.is_some() {
            anyhow::bail!(
                "--manifest-path cannot be used with --source sql; did you mean --source manifest?"
            );
        }
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
fn run_column_lineage_command(
    models: Vec<String>,
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
) -> Result<()> {
    if models.is_empty() {
        anyhow::bail!("no model names provided (specify as arguments)");
    }

    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    let resolved = resolve_manifest_path_or_default(manifest_path, &project_dir)?;
    let manifest = parser::manifest::load_manifest(&resolved)?;

    let reports: Vec<_> = models
        .iter()
        .map(|model| graph::column_lineage::compute_column_lineage(&manifest, model))
        .collect();

    // Print warnings for errors
    for report in &reports {
        for err in &report.errors {
            dlin::warn!("{}", err);
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
    } else if let Err(e) = std::io::Write::write_all(&mut out, b"\n") {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e.into());
        }
    }

    Ok(())
}

/// Run the `summary` subcommand
#[cfg(not(tarpaulin_include))]
fn run_summary_command(args: SummaryArgs) -> Result<()> {
    let project_dir = args
        .project_dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot resolve project directory '{}': {}", args.project_dir.display(), e))?;

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let project = parser::project::DbtProject::load(&project_dir)?;
    let vars_count = project.vars.len();
    let project_name = project.name.clone();

    let (dag, _manifest) = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), args.cache_dir.as_deref(), args.no_cache, args.refresh_cache)?;

    let node_counts = render::summary::count_nodes(&dag);
    let edge_count = dag.edge_count();

    // Check manifest freshness (best-effort)
    let manifest_status = check_manifest_freshness(&project_dir, args.manifest_path.as_ref(), &project);

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
    let all_files = files.model_sql_files.iter()
        .chain(files.macro_sql_files.iter())
        .chain(files.seed_files.iter())
        .chain(files.snapshot_sql_files.iter())
        .chain(files.test_sql_files.iter())
        .chain(files.yaml_files.iter());

    for file in all_files {
        if let Ok(meta) = std::fs::metadata(file) {
            if let Ok(mtime) = meta.modified() {
                if mtime > manifest_mtime {
                    let rel = file.strip_prefix(project_dir).unwrap_or(file);
                    stale_files.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    stale_files.sort();

    // Check for deleted files: paths referenced in manifest but missing on disk
    let deleted_files = match parser::manifest::load_manifest(&resolved) {
        Ok(manifest) => find_deleted_manifest_files(&manifest, project_dir),
        Err(e) => {
            dlin::warn!("cannot parse manifest.json for deleted-file check: {}", e);
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
    let project_dir = args
        .project_dir
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot resolve project directory '{}': {}", args.project_dir.display(), e))?;

    let manifest_path = resolve_manifest_path_or_default(
        args.manifest_path.as_ref(),
        &project_dir,
    )?;

    let manifest_mtime = std::fs::metadata(&manifest_path)
        .and_then(|m| m.modified())
        .map_err(|e| anyhow::anyhow!("cannot read manifest.json at {}: {}", manifest_path.display(), e))?;

    // Discover project files
    let project = parser::project::DbtProject::load(&project_dir)?;
    let paths = project.resolve_paths(&project_dir);
    let files = parser::discovery::discover_files(&paths)?;

    // Collect all SQL/YAML files and compare mtimes
    let mut stale_files: Vec<PathBuf> = Vec::new();
    let all_files = files.model_sql_files.iter()
        .chain(files.macro_sql_files.iter())
        .chain(files.seed_files.iter())
        .chain(files.snapshot_sql_files.iter())
        .chain(files.test_sql_files.iter())
        .chain(files.yaml_files.iter());

    for file in all_files {
        match std::fs::metadata(file) {
            Ok(meta) => {
                if let Ok(mtime) = meta.modified() {
                    if mtime > manifest_mtime {
                        let rel = file.strip_prefix(&project_dir).unwrap_or(file);
                        stale_files.push(rel.to_path_buf());
                    }
                }
            }
            Err(e) => {
                dlin::warn!("cannot read metadata for {}: {}", file.display(), e);
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
                        parts.push(format!("{} file{} newer",
                            stale_files.len(),
                            if stale_files.len() == 1 { "" } else { "s" }
                        ));
                    }
                    if !deleted_files.is_empty() {
                        parts.push(format!("{} deleted",
                            deleted_files.len(),
                        ));
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
                "stale_files": stale_files.iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>(),
                "deleted_file_count": deleted_files.len(),
                "deleted_files": deleted_files.iter().map(|f| f.to_string_lossy().into_owned()).collect::<Vec<_>>(),
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
            } else if let Err(e) = writeln!(out) {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e.into());
                }
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
