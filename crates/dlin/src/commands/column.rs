use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use super::shared::*;
use crate::cli::{ColumnOutputFormat, DialectArg, SourceType};
use dlin_core::graph;
use dlin_core::input;
use dlin_core::parser;
use dlin_core::render;

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_column_lineage_command(
    cli_models: Vec<String>,
    columns: &[String],
    output: &ColumnOutputFormat,
    dialect: Option<DialectArg>,
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> Result<()> {
    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    // Merge CLI positional args and stdin before loading the manifest so that a missing
    // manifest does not mask a "no model names provided" error from the user.
    // raw_input_set captures every name the user supplied (CLI + stdin) so the model-only
    // filter below preserves both explicit CLI args and explicit stdin names that happen to
    // resolve to non-model DAG nodes — those should surface proper errors, not silent drops.
    let stdin_lines = input::read_stdin_lines();
    let mut raw_inputs = cli_models;
    raw_inputs.extend(stdin_lines);

    if raw_inputs.is_empty() {
        anyhow::bail!("no model names provided (specify as arguments or via stdin)");
    }

    // Load manifest once — reused for both path resolution and column lineage analysis.
    let resolved_manifest_path = resolve_manifest_path_or_default(manifest_path, &project_dir)?;
    let manifest_report = parser::manifest::load_manifest_report(&resolved_manifest_path)?;
    warn_manifest_diagnostics(&manifest_report.diagnostics);
    let manifest = manifest_report.into_manifest()?;

    let resolved_dialect = resolve_dialect(dialect.as_ref(), &manifest)?;
    if let Some(warning) = &resolved_dialect.warning {
        dlin_core::warn!("{}", warning);
    }
    let dialect = resolved_dialect.dialect;

    let models = if input::has_path_like_input(&raw_inputs) {
        let dag = parser::manifest::build_graph_from_parsed_manifest(&manifest)?;
        let cwd = std::env::current_dir()
            .map_err(|e| anyhow::anyhow!("failed to determine current working directory: {}", e))?;
        let resolved_paths =
            resolve_paths_for_path_input(SourceType::Manifest, &project_dir, None)?;
        // Snapshot all user-provided inputs (both bare names and file paths) before path
        // expansion.  Path-like strings (e.g. "models/stg.sql") are stored verbatim and
        // will never match the expanded model names produced by resolve_stdin_inputs, so
        // they remain subject to the model-only filter below.  Bare names (e.g.
        // "raw.orders") appear unchanged in both this set and all_resolved, which exempts
        // them from the model-only filter so they reach the analyzer and surface a proper
        // error rather than being silently dropped.
        let raw_input_set: std::collections::HashSet<&str> =
            raw_inputs.iter().map(|s| s.as_str()).collect();
        let all_resolved =
            input::resolve_stdin_inputs(&raw_inputs, &dag, &resolved_paths, &project_dir, &cwd);
        // Filter out non-model nodes (sources, tests, analyses) that may come from YAML/SQL
        // file-path expansion — column lineage only supports resource_type == "model".
        // Unsupported resource kinds are omitted from the DAG, so check the
        // manifest directly when applying the model-only filter.
        let manifest_model_names: std::collections::HashSet<&str> = manifest
            .nodes
            .values()
            .filter(|n| n.resource_type == "model")
            .map(|n| n.name.as_str())
            .collect();
        all_resolved
            .into_iter()
            .filter(|name| {
                raw_input_set.contains(name.as_str())
                    || manifest_model_names.contains(name.as_str())
                    || graph::filter::try_resolve_node_quiet(&dag, name).is_none()
            })
            .collect()
    } else {
        raw_inputs
    };

    if models.is_empty() {
        anyhow::bail!("no model names provided (specify as arguments or via stdin)");
    }

    // Deduplicate while preserving order
    let mut seen = HashSet::new();
    let models: Vec<String> = models
        .into_iter()
        .filter(|m| seen.insert(m.clone()))
        .collect();

    let mut cache = if no_cache {
        graph::column_lineage::ColumnLineageCache::disabled()
    } else if refresh_cache {
        graph::column_lineage::ColumnLineageCache::fresh(&project_dir, cache_dir)
    } else {
        graph::column_lineage::ColumnLineageCache::load(&project_dir, cache_dir)
    };

    let column_filter: Vec<&str> = columns.iter().map(|s| s.as_str()).collect();
    let mut analysis =
        graph::column_lineage::ColumnLineageAnalysis::new(&manifest, dialect, &mut cache);

    let reports: Vec<_> = models
        .iter()
        .map(|model| {
            let mut report = analysis.compute_cross_model_column_lineage(model);
            if !column_filter.is_empty() {
                report.select_columns(&column_filter, dialect);
            }
            report
        })
        .collect();

    // Print warnings for errors
    let mut has_errors = false;
    for report in &reports {
        for err in &report.errors {
            dlin_core::warn!("{}", err);
            has_errors |= err.is_fatal();
        }
    }

    match output {
        ColumnOutputFormat::Json => {
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
        }
        ColumnOutputFormat::Plain => {
            render::column_graph::render_column_graph_plain(&reports);
        }
        ColumnOutputFormat::Mermaid => {
            render::column_graph::render_column_graph_mermaid(&reports);
        }
        ColumnOutputFormat::Dot => {
            render::column_graph::render_column_graph_dot(&reports);
        }
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
pub(crate) fn run_column_impact_command(
    model: &str,
    columns: &[String],
    output: &ColumnOutputFormat,
    dialect: Option<DialectArg>,
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> Result<()> {
    if columns.is_empty() {
        anyhow::bail!("no columns specified (use --column)");
    }

    let project_dir = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.to_path_buf());

    let resolved = resolve_manifest_path_or_default(manifest_path, &project_dir)?;
    let manifest_report = parser::manifest::load_manifest_report(&resolved)?;
    warn_manifest_diagnostics(&manifest_report.diagnostics);
    let manifest = manifest_report.into_manifest()?;

    let resolved_dialect = resolve_dialect(dialect.as_ref(), &manifest)?;
    if let Some(warning) = &resolved_dialect.warning {
        dlin_core::warn!("{}", warning);
    }
    let dialect = resolved_dialect.dialect;

    let mut cache = if no_cache {
        graph::column_lineage::ColumnLineageCache::disabled()
    } else if refresh_cache {
        graph::column_lineage::ColumnLineageCache::fresh(&project_dir, cache_dir)
    } else {
        graph::column_lineage::ColumnLineageCache::load(&project_dir, cache_dir)
    };

    let mut analysis =
        graph::column_lineage::ColumnLineageAnalysis::new(&manifest, dialect, &mut cache);

    let reports: Vec<_> = columns
        .iter()
        .map(|col| {
            analysis.compute_column_impact(
                model,
                col,
                graph::column_lineage::ImpactScope::ValueOnly,
            )
        })
        .collect();

    // Print warnings for errors
    let mut has_errors = false;
    for report in &reports {
        for err in &report.errors {
            dlin_core::warn!("{}", err);
            has_errors |= err.is_fatal();
        }
    }

    match output {
        ColumnOutputFormat::Json => {
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
        }
        ColumnOutputFormat::Plain => {
            render::column_graph::render_column_impact_plain(&reports);
        }
        ColumnOutputFormat::Mermaid => {
            render::column_graph::render_column_impact_mermaid(&reports);
        }
        ColumnOutputFormat::Dot => {
            render::column_graph::render_column_impact_dot(&reports);
        }
    }

    cache.save();

    if has_errors {
        anyhow::bail!("column downstream analysis completed with errors");
    }
    Ok(())
}
