use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

use dlin::cli::{self, Cli, Command, GraphArgs, ListArgs, SourceType};
use dlin::graph;
use dlin::input;
use dlin::parser;
use dlin::render;

#[cfg(not(tarpaulin_include))]
fn main() -> Result<()> {
    let cli = Cli::parse();

    let cache_dir = cli.cache_dir;

    match cli.command {
        Command::Graph(args) => {
            dlin::set_quiet(args.quiet);
            run_graph_command(args, cache_dir.as_deref())
        }
        Command::List(args) => {
            dlin::set_quiet(args.quiet);
            run_list_command(args, cache_dir.as_deref())
        }
        Command::Impact {
            model,
            project_dir,
            output,
            source,
            manifest_path,
            show_sql,
            quiet,
        } => {
            dlin::set_quiet(quiet);
            run_impact_command(&model, &project_dir, &output, &source, manifest_path.as_ref(), show_sql, cache_dir.as_deref())
        }
    }
}

/// Run the `graph` subcommand
#[cfg(not(tarpaulin_include))]
fn run_graph_command(args: GraphArgs, cache_dir: Option<&Path>) -> Result<()> {
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    // Validate flag combinations before building DAG
    validate_source_flags(&args.source, args.manifest_path.as_ref())?;
    if args.source == SourceType::Sql && args.include_tests {
        dlin::warn!("--include-tests has no effect with --source sql; tests are only available with --source manifest");
    }

    let dag = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), cache_dir)?;

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

    let sql_contents = if args.show_sql {
        Some(collect_sql_contents(&filtered, &project_dir))
    } else {
        None
    };

    render_output(&args.output, &filtered, sql_contents.as_ref());

    Ok(())
}

/// Run the `list` subcommand
#[cfg(not(tarpaulin_include))]
fn run_list_command(args: ListArgs, cache_dir: Option<&Path>) -> Result<()> {
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;
    if args.source == SourceType::Sql && args.include_tests {
        dlin::warn!("--include-tests has no effect with --source sql; tests are only available with --source manifest");
    }

    let dag = build_dag(&project_dir, &args.source, args.manifest_path.as_ref(), cache_dir)?;

    // Parse selectors
    let selectors = args
        .select
        .as_deref()
        .map(graph::filter::parse_selectors)
        .unwrap_or_default();

    // Filter graph
    let filtered = graph::filter::filter_graph(
        &dag,
        &[],
        None,
        None,
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

    render::list::render_list(&filtered, &args.output);

    Ok(())
}

/// Build the lineage DAG from either a manifest file or by parsing SQL files
#[cfg(not(tarpaulin_include))]
fn build_dag(
    project_dir: &Path,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
) -> Result<graph::types::LineageGraph> {
    match source {
        SourceType::Manifest => {
            let path = manifest_path
                .ok_or_else(|| anyhow::anyhow!("manifest_path is required for SourceType::Manifest (call validate_source_flags first)"))?;
            let resolved = resolve_manifest_path(path)?;
            parser::manifest::build_graph_from_manifest(&resolved)
        }
        SourceType::Sql => {
            let project = parser::project::DbtProject::load(project_dir)?;
            let paths = project.resolve_paths(project_dir);
            let files = parser::discovery::discover_files(&paths)?;
            graph::builder::build_graph(project_dir, &files, cache_dir)
        }
    }
}

/// Dispatch rendering based on output format
#[cfg(not(tarpaulin_include))]
fn render_output(
    format: &cli::OutputFormat,
    graph: &graph::types::LineageGraph,
    sql_contents: Option<&HashMap<String, String>>,
) {
    match format {
        cli::OutputFormat::Ascii => render::ascii::render_ascii(graph),
        cli::OutputFormat::Dot => render::dot::render_dot(graph),
        cli::OutputFormat::Json => render::json::render_json(graph, sql_contents),
        cli::OutputFormat::Mermaid => render::mermaid::render_mermaid(graph),
        cli::OutputFormat::Plain => render::plain::render_plain(graph, sql_contents),
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
    show_sql: bool,
    cache_dir: Option<&Path>,
) -> Result<()> {
    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    validate_source_flags(source, manifest_path)?;
    let dag = build_dag(&project_dir, source, manifest_path, cache_dir)?;

    let mut reports: Vec<_> = models
        .iter()
        .filter_map(|model| {
            let source_idx = graph::filter::try_resolve_node(&dag, model)?;
            Some(graph::impact::compute_impact(&dag, source_idx))
        })
        .collect();

    if show_sql {
        let sql_map = collect_sql_contents(&dag, &project_dir);
        for report in &mut reports {
            for node in &mut report.impacted_nodes {
                if let Some(sql) = sql_map.get(&node.unique_id) {
                    node.sql_content = Some(sql.clone());
                }
            }
        }
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
    match source {
        SourceType::Manifest if manifest_path.is_none() => {
            anyhow::bail!("--manifest-path is required when using --source manifest");
        }
        SourceType::Sql if manifest_path.is_some() => {
            anyhow::bail!(
                "--manifest-path cannot be used with --source sql; did you mean --source manifest?"
            );
        }
        _ => Ok(()),
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
