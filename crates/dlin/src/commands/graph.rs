use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use regex::RegexBuilder;

use super::shared::*;
use crate::cli::{self, Direction, GraphArgs, GroupBy, ListArgs, SourceType};
use dlin_core::graph;
use dlin_core::input;
use dlin_core::parser;
use dlin_core::render;

pub(crate) fn run_graph_command(args: GraphArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let refresh_cache = args.refresh_cache;
    let project_dir = args.project_dir.canonicalize().unwrap_or(args.project_dir);

    // Validate flag combinations before building DAG
    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, project_opt, manifest_diagnostics, manifest_opt) = build_dag(
        &project_dir,
        &args.source,
        args.manifest_path.as_ref(),
        cache_dir.as_deref(),
        no_cache,
        refresh_cache,
    )?;
    warn_manifest_diagnostics(&manifest_diagnostics);

    // Merge CLI positional args and stdin, then resolve file paths to node names
    let stdin_lines = input::read_stdin_lines();
    let mut raw_inputs = args.model;
    raw_inputs.extend(stdin_lines);
    let models = if input::has_path_like_input(&raw_inputs) {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let resolved_paths =
            resolve_paths_for_path_input(args.source, &project_dir, project_opt.as_ref())?;
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
            &args.source,
            &project_dir,
            manifest_opt.as_ref(),
            args.manifest_path.as_ref(),
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
pub(crate) fn run_list_command(args: ListArgs) -> Result<()> {
    let cache_dir = args.cache_dir;
    let no_cache = args.no_cache;
    let refresh_cache = args.refresh_cache;
    let project_dir = args.project_dir.canonicalize().unwrap_or(args.project_dir);

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, project_opt, manifest_diagnostics, manifest_opt) = build_dag(
        &project_dir,
        &args.source,
        args.manifest_path.as_ref(),
        cache_dir.as_deref(),
        no_cache,
        refresh_cache,
    )?;
    warn_manifest_diagnostics(&manifest_diagnostics);

    // Merge CLI positional args and stdin, then resolve file paths to node names
    let stdin_lines = input::read_stdin_lines();
    let mut raw_inputs = args.model;
    raw_inputs.extend(stdin_lines);
    let models = if input::has_path_like_input(&raw_inputs) {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let resolved_paths =
            resolve_paths_for_path_input(args.source, &project_dir, project_opt.as_ref())?;
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

    // Compile search patterns and apply filter (AND across all patterns)
    let search_patterns = args
        .search
        .iter()
        .map(|p| {
            RegexBuilder::new(p)
                .case_insensitive(true)
                .build()
                .map_err(|e| anyhow::anyhow!("invalid --search pattern {:?}: {}", p, e))
        })
        .collect::<Result<Vec<_>>>()?;
    let filtered = graph::filter::filter_by_search(&filtered, &search_patterns);
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
            &args.source,
            &project_dir,
            manifest_opt.as_ref(),
            args.manifest_path.as_ref(),
            &filtered,
        ))
    } else {
        None
    };

    render::list::render_list(&filtered, &args.output, &json_fields, sql_contents.as_ref());

    Ok(())
}

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
/// - **manifest**: reads `compiled_code` from manifest.json.
///   Users must run `dbt compile` beforehand so the manifest contains compiled SQL.
/// - **sql**: reads raw SQL files from disk.
#[cfg(not(tarpaulin_include))]
fn collect_sql_contents_for_source(
    source: &SourceType,
    project_dir: &Path,
    manifest: Option<&parser::manifest::Manifest>,
    manifest_path: Option<&PathBuf>,
    graph: &graph::types::LineageGraph,
) -> HashMap<String, String> {
    match source {
        SourceType::Manifest => {
            if let Some(manifest) = manifest {
                manifest.collect_sql_contents()
            } else {
                let Ok(resolved) = resolve_manifest_path_or_default(manifest_path, project_dir)
                else {
                    return HashMap::new();
                };
                let Ok(manifest) = parser::manifest::load_manifest(&resolved) else {
                    return HashMap::new();
                };
                manifest.collect_sql_contents()
            }
        }
        SourceType::Sql => collect_sql_contents(graph, project_dir),
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
            if !parser::project::is_sql_file(rel_path, true) {
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
pub(crate) fn run_impact_command(
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
    let (dag, project_opt, manifest_diagnostics, _manifest) = build_dag(
        &project_dir,
        source,
        manifest_path,
        cache_dir,
        no_cache,
        refresh_cache,
    )?;
    warn_manifest_diagnostics(&manifest_diagnostics);

    // Resolve file paths to model names (same as graph/list commands)
    let models = if input::has_path_like_input(&raw_inputs) {
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let resolved_paths =
            resolve_paths_for_path_input(*source, &project_dir, project_opt.as_ref())?;
        input::resolve_stdin_inputs(&raw_inputs, &dag, &resolved_paths, &project_dir, &cwd)
    } else {
        raw_inputs
    };

    // Deduplicate while preserving order
    let mut seen = HashSet::new();
    let models: Vec<String> = models
        .into_iter()
        .filter(|m| seen.insert(m.clone()))
        .collect();

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
