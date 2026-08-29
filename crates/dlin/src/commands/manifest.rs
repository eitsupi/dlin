use std::path::{Path, PathBuf};

use anyhow::Result;
use path_slash::PathExt as _;

use super::shared::*;
use crate::cli::{
    CheckManifestArgs, CheckManifestOutputFormat, SourceType, SummaryArgs, SummaryOutputFormat,
};
use dlin_core::parser;
use dlin_core::render;

#[cfg(not(tarpaulin_include))]
pub(crate) fn run_summary_command(args: SummaryArgs) -> Result<()> {
    let project_dir = args.project_dir.canonicalize().map_err(|e| {
        anyhow::anyhow!(
            "cannot resolve project directory '{}': {}",
            args.project_dir.display(),
            e
        )
    })?;

    validate_source_flags(&args.source, args.manifest_path.as_ref())?;

    let (dag, project_opt, manifest_diagnostics, manifest_opt) = build_dag(
        &project_dir,
        &args.source,
        args.manifest_path.as_ref(),
        args.cache_dir.as_deref(),
        args.no_cache,
        args.refresh_cache,
    )?;
    warn_manifest_diagnostics(&manifest_diagnostics);

    let (project_name, vars_count, manifest_status) = match args.source {
        SourceType::Manifest => {
            let name = manifest_opt
                .as_ref()
                .and_then(|manifest| manifest.metadata.project_name.clone())
                .unwrap_or_else(|| "(unknown)".to_string());
            let status = match parser::project::DbtProject::load(&project_dir) {
                Ok(project) => check_manifest_freshness(
                    &project_dir,
                    args.manifest_path.as_ref(),
                    &project,
                    manifest_opt.as_ref(),
                ),
                Err(e) => {
                    let is_not_found = e
                        .downcast_ref::<dlin_core::error::DbtLineageError>()
                        .is_some_and(|de| {
                            matches!(de, dlin_core::error::DbtLineageError::ProjectNotFound(_))
                        });
                    if !is_not_found {
                        return Err(e);
                    }
                    None
                }
            };
            (name, 0, status)
        }
        SourceType::Sql => {
            let project = project_opt.ok_or_else(|| {
                anyhow::anyhow!("internal error: DbtProject not available in sql mode")
            })?;
            let status =
                check_manifest_freshness(&project_dir, args.manifest_path.as_ref(), &project, None);
            let vars_count = project.vars.len();
            let name = project.name.clone();
            (name, vars_count, status)
        }
    };

    let node_counts = render::summary::count_nodes(&dag);
    let edge_count = dag.edge_count();

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

/// Collect files whose mtimes can make a manifest stale.
///
/// The project file and the optional project-level vars file affect dbt's
/// interpretation of the discovered resources, so they are freshness inputs
/// even though they do not live under one of the configured resource paths.
fn collect_manifest_freshness_inputs(
    project_dir: &Path,
    project: &parser::project::DbtProject,
) -> Result<Vec<PathBuf>> {
    let paths = project.resolve_paths(project_dir);
    let files = parser::discovery::discover_files(&paths)?;
    let mut inputs = files
        .model_sql_files
        .into_iter()
        .chain(files.macro_sql_files)
        .chain(files.seed_files)
        .chain(files.snapshot_sql_files)
        .chain(files.test_sql_files)
        .chain(files.yaml_files)
        .collect::<Vec<_>>();

    for name in ["dbt_project.yml", "vars.yml"] {
        let path = project_dir.join(name);
        if path.is_file() {
            inputs.push(path);
        }
    }

    inputs.sort();
    inputs.dedup();

    Ok(inputs)
}

/// Check manifest.json freshness, returning None if manifest is irrelevant.
#[cfg(not(tarpaulin_include))]
pub(crate) fn check_manifest_freshness(
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    project: &parser::project::DbtProject,
    manifest: Option<&parser::manifest::Manifest>,
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

    let files = match collect_manifest_freshness_inputs(project_dir, project) {
        Ok(f) => f,
        Err(_) => return None,
    };

    let mut stale_files: Vec<String> = Vec::new();
    for file in &files {
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
    let deleted_files = match manifest {
        Some(manifest) => find_deleted_manifest_files(manifest, project_dir),
        None => match parser::manifest::load_manifest(&resolved) {
            Ok(manifest) => find_deleted_manifest_files(&manifest, project_dir),
            Err(e) => {
                dlin_core::warn!("cannot parse manifest.json for deleted-file check: {}", e);
                return None;
            }
        },
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
pub(crate) fn run_check_manifest_command(args: CheckManifestArgs) -> Result<()> {
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
    let files = collect_manifest_freshness_inputs(&project_dir, &project)?;

    // Collect all SQL/YAML files and compare mtimes
    let mut stale_files: Vec<PathBuf> = Vec::new();
    for file in &files {
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
    let manifest_report = parser::manifest::load_manifest_report(&manifest_path)?;
    warn_manifest_diagnostics(&manifest_report.diagnostics);
    let manifest = manifest_report.into_manifest()?;
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
