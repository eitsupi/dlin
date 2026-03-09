use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

use dlin::cli::{self, CheckManifestArgs, CheckManifestOutputFormat, Cli, Command, GraphArgs, ListArgs, SourceType};
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
fn main() -> Result<()> {
    #[cfg(unix)]
    reset_sigpipe();

    let cli = Cli::parse();

    match cli.command {
        Command::Graph(args) => {
            dlin::set_quiet(args.quiet);
            run_graph_command(args)
        }
        Command::List(args) => {
            dlin::set_quiet(args.quiet);
            run_list_command(args)
        }
        Command::CheckManifest(args) => {
            dlin::set_quiet(args.quiet);
            run_check_manifest_command(args)
        }
        Command::Impact {
            model,
            project_dir,
            output,
            source,
            manifest_path,
            cache_dir,
            no_cache,
            quiet,
        } => {
            dlin::set_quiet(quiet);
            run_impact_command(&model, &project_dir, &output, &source, manifest_path.as_ref(), cache_dir.as_deref(), no_cache)
        }
    }
}

/// Run the `graph` subcommand
#[cfg(not(tarpaulin_include))]
fn run_graph_command(args: GraphArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    // Validate flag combinations before building DAG
    validate_source_flags(&args.source, args.manifest_path.as_ref())?;
    if args.source == SourceType::Sql && args.include_tests {
        dlin::warn!("--include-tests has no effect with --source sql; tests are only available with --source manifest");
    }

    let dag = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), cache_dir.as_deref(), no_cache)?;

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
        &graph::filter::NodeTypeFilter {
            include_tests: args.include_tests,
            include_seeds: args.include_seeds,
            include_snapshots: args.include_snapshots,
            include_exposures: args.include_exposures,
        },
        &selectors,
    )?;

    // Apply output node-type filter
    let filtered = if let Some(ref type_names) = args.node_types {
        for t in &graph::filter::validate_node_type_names(type_names) {
            dlin::warn!("unknown node type '{}'. Known types: {}", t, graph::filter::KNOWN_NODE_TYPE_LABELS.join(", "));
        }
        graph::filter::filter_output_node_types(&filtered, type_names)
    } else {
        filtered
    };

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
        Some(collect_sql_contents(&filtered, &project_dir))
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
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;
    if args.source == SourceType::Sql && args.include_tests {
        dlin::warn!("--include-tests has no effect with --source sql; tests are only available with --source manifest");
    }

    let dag = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), cache_dir.as_deref(), no_cache)?;

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
        &graph::filter::NodeTypeFilter {
            include_tests: args.include_tests,
            include_seeds: args.include_seeds,
            include_snapshots: args.include_snapshots,
            include_exposures: args.include_exposures,
        },
        &selectors,
    )?;

    // Apply output node-type filter
    let filtered = if let Some(ref type_names) = args.node_types {
        for t in &graph::filter::validate_node_type_names(type_names) {
            dlin::warn!("unknown node type '{}'. Known types: {}", t, graph::filter::KNOWN_NODE_TYPE_LABELS.join(", "));
        }
        graph::filter::filter_output_node_types(&filtered, type_names)
    } else {
        filtered
    };

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
        Some(collect_sql_contents(&filtered, &project_dir))
    } else {
        None
    };

    render::list::render_list(&filtered, &args.output, &json_fields, sql_contents.as_ref());

    Ok(())
}

/// Build the lineage DAG from either a manifest file or by parsing SQL files
#[cfg(not(tarpaulin_include))]
fn build_dag(
    project_dir: &Path,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
) -> Result<graph::types::LineageGraph> {
    match source {
        SourceType::Manifest => {
            let resolved = resolve_manifest_path_or_default(manifest_path, project_dir)?;
            parser::manifest::build_graph_from_manifest(&resolved)
        }
        SourceType::Sql => {
            let project = parser::project::DbtProject::load(project_dir)?;
            let paths = project.resolve_paths(project_dir);
            let files = parser::discovery::discover_files(&paths)?;
            graph::builder::build_graph(project_dir, &files, cache_dir, no_cache, &project.vars)
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
    models: &[String],
    project_dir: &Path,
    output: &cli::ImpactOutputFormat,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
) -> Result<()> {
    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    validate_source_flags(source, manifest_path)?;
    let dag = build_dag(&project_dir, source, manifest_path, cache_dir, no_cache)?;

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
    let is_stale = !stale_files.is_empty();

    match args.output {
        CheckManifestOutputFormat::Text => {
            if !args.quiet {
                if is_stale {
                    println!(
                        "manifest.json is stale ({} file{} newer):",
                        stale_files.len(),
                        if stale_files.len() == 1 { "" } else { "s" }
                    );
                    for f in &stale_files {
                        println!("  {}", f.display());
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
