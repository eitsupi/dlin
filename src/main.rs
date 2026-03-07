use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

use dlin::cli::{self, Cli, Command, GraphArgs, SourceType};
use dlin::graph;
use dlin::parser;
use dlin::render;

#[cfg(not(tarpaulin_include))]
fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Graph(args) => run_graph_command(args),
        Command::Impact {
            model,
            project_dir,
            output,
            source,
            manifest_path,
        } => run_impact_command(&model, &project_dir, &output, &source, manifest_path.as_ref()),
    }
}

/// Run the `graph` subcommand
#[cfg(not(tarpaulin_include))]
fn run_graph_command(args: GraphArgs) -> Result<()> {
    let project_dir = args
        .project_dir
        .canonicalize()
        .unwrap_or(args.project_dir);

    let dag = build_dag(&project_dir, &args.source, args.manifest_path.as_ref())?;

    // Warn when --include-tests is used with SQL source (tests aren't detectable from SQL)
    if args.source == SourceType::Sql && args.include_tests {
        eprintln!("Warning: --include-tests has no effect with --source sql; tests are only available with --source manifest");
    }

    // Parse selectors
    let selectors = args
        .select
        .as_deref()
        .map(graph::filter::parse_selectors)
        .unwrap_or_default();

    // Filter graph
    let filtered = graph::filter::filter_graph(
        &dag,
        args.model.as_deref(),
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

    render_output(&args.output, &filtered);

    Ok(())
}

/// Build the lineage DAG from either a manifest file or by parsing SQL files
#[cfg(not(tarpaulin_include))]
fn build_dag(
    project_dir: &Path,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
) -> Result<graph::types::LineageGraph> {
    match source {
        SourceType::Manifest => {
            let path = manifest_path
                .ok_or_else(|| anyhow::anyhow!("--manifest-path is required when using --source manifest"))?;
            let resolved = resolve_manifest_path(path)?;
            parser::manifest::build_graph_from_manifest(&resolved)
        }
        SourceType::Sql => {
            let project = parser::project::DbtProject::load(project_dir)?;
            let paths = project.resolve_paths(project_dir);
            let files = parser::discovery::discover_files(&paths)?;
            graph::builder::build_graph(project_dir, &files)
        }
    }
}

/// Dispatch rendering based on output format
#[cfg(not(tarpaulin_include))]
fn render_output(format: &cli::OutputFormat, graph: &graph::types::LineageGraph) {
    match format {
        cli::OutputFormat::Ascii => render::ascii::render_ascii(graph),
        cli::OutputFormat::Dot => render::dot::render_dot(graph),
        cli::OutputFormat::Json => render::json::render_json(graph),
        cli::OutputFormat::Mermaid => render::mermaid::render_mermaid(graph),
        cli::OutputFormat::Svg => render::svg::render_svg(graph),
        cli::OutputFormat::Html => render::html::render_html(graph),
    }
}

/// Run the `impact` subcommand
#[cfg(not(tarpaulin_include))]
fn run_impact_command(
    model: &str,
    project_dir: &Path,
    output: &cli::ImpactOutputFormat,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
) -> Result<()> {
    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    let dag = build_dag(&project_dir, source, manifest_path)?;

    // Find the source model node
    let source_idx = dag
        .node_indices()
        .find(|&idx| {
            let node = &dag[idx];
            node.label == model || node.unique_id.ends_with(&format!(".{}", model))
        })
        .ok_or_else(|| anyhow::anyhow!("Model '{}' not found in the graph", model))?;

    let report = graph::impact::compute_impact(&dag, source_idx);

    match output {
        cli::ImpactOutputFormat::Text => render::impact::render_impact_text(&report),
        cli::ImpactOutputFormat::Json => render::impact::render_impact_json(&report),
    }

    Ok(())
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
