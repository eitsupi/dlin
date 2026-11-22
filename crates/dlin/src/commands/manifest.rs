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

    let DagBuild {
        dag,
        project: project_opt,
        manifest_diagnostics,
        manifest_context,
        project_gaps: _,
    } = build_dag(
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
            let name = manifest_context
                .as_ref()
                .and_then(|context| context.project_name.clone())
                .unwrap_or_else(|| "(unknown)".to_string());
            let status = match parser::project::DbtProject::load(&project_dir) {
                Ok(project) => check_manifest_freshness_with_referenced_paths(
                    &project_dir,
                    args.manifest_path.as_ref(),
                    &project,
                    manifest_context.as_ref().map(|context| {
                        (
                            context.project_name.as_deref(),
                            context.referenced_files.as_slice(),
                        )
                    }),
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

/// Collect files whose mtimes can make a manifest stale.
///
/// Project configuration and dependency declarations affect which inputs
/// dbt loads, even when they live outside the configured resource paths.
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

    inputs.extend(
        std::iter::once("dbt_project.yml")
            .chain(OPTIONAL_PROJECT_FILES)
            .map(|name| project_dir.join(name))
            .filter(|path| path.is_file()),
    );

    inputs.sort();
    inputs.dedup();

    Ok(inputs)
}

const OPTIONAL_PROJECT_FILES: [&str; 4] = [
    "vars.yml",
    "packages.yml",
    "dependencies.yml",
    "package-lock.yml",
];

/// Return whether a freshness input is newer than the manifest.
///
/// A file that cannot be inspected is treated as stale. The discovery result
/// may become unavailable before freshness checking, and considering that
/// input fresh could make a stale manifest appear up to date.
fn is_freshness_input_stale(file: &Path, manifest_mtime: std::time::SystemTime) -> bool {
    match std::fs::metadata(file).and_then(|metadata| metadata.modified()) {
        Ok(mtime) => mtime > manifest_mtime,
        Err(e) => {
            dlin_core::warn!("cannot read metadata for {}: {}", file.display(), e);
            true
        }
    }
}

/// Check manifest.json freshness, returning None if manifest is irrelevant.
#[cfg(not(tarpaulin_include))]
pub(crate) fn check_manifest_freshness(
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    project: &parser::project::DbtProject,
    manifest: Option<&parser::manifest::Manifest>,
) -> Option<render::summary::ManifestStatus> {
    let files =
        manifest.map(|manifest| manifest.referenced_files().into_iter().collect::<Vec<_>>());
    check_manifest_freshness_with_referenced_paths(
        project_dir,
        manifest_path,
        project,
        manifest
            .zip(files.as_deref())
            .map(|(manifest, files)| (manifest.metadata.project_name.as_deref(), files)),
    )
}

/// Reuse package-owned file references from the model-level cache on warm paths.
#[cfg(not(tarpaulin_include))]
fn check_manifest_freshness_with_referenced_paths(
    project_dir: &Path,
    manifest_path: Option<&PathBuf>,
    project: &parser::project::DbtProject,
    references: Option<(Option<&str>, &[parser::manifest::ManifestFile])>,
) -> Option<render::summary::ManifestStatus> {
    let Ok(resolved) = resolve_manifest_path_or_default(manifest_path, project_dir) else {
        return Some(render::summary::ManifestStatus::default());
    };
    let Ok(mtime) = std::fs::metadata(&resolved).and_then(|m| m.modified()) else {
        return Some(render::summary::ManifestStatus::default());
    };
    let (manifest, files);
    let (name, files) = match references {
        Some(references) => references,
        None => {
            manifest = parser::manifest::load_manifest(&resolved)
                .map_err(|err| {
                    dlin_core::warn!("cannot parse manifest.json for deleted-file check: {}", err);
                })
                .ok()?;
            files = manifest.referenced_files().into_iter().collect::<Vec<_>>();
            (manifest.metadata.project_name.as_deref(), files.as_slice())
        }
    };
    Some(assess_freshness(
        project_dir,
        Some(project),
        name,
        files,
        mtime,
    ))
}

fn assess_freshness(
    project_dir: &Path,
    project: Option<&parser::project::DbtProject>,
    manifest_project: Option<&str>,
    files: &[parser::manifest::ManifestFile],
    manifest_mtime: std::time::SystemTime,
) -> render::summary::ManifestStatus {
    use dlin_core::parser::package_roots::{PackageRoots, relative_resource_path};
    use render::summary::{ManifestStatus, UnverifiedFiles};
    use std::collections::{BTreeMap, BTreeSet};

    let unverified = |reason| ManifestStatus {
        found: true,
        unverified: vec![UnverifiedFiles {
            package: None,
            reason,
        }],
        ..Default::default()
    };
    let Some(project) = project else {
        return unverified("project_unavailable");
    };
    if manifest_project.is_some_and(|name| name != project.name) {
        return unverified("project_mismatch");
    }
    let mut gaps = BTreeSet::new();
    let inputs = match collect_manifest_freshness_inputs(project_dir, project) {
        Ok(files) => files,
        Err(_) => {
            gaps.insert(UnverifiedFiles {
                package: None,
                reason: "project_discovery_failed",
            });
            Vec::new()
        }
    };
    let display = |path: &Path| {
        path.strip_prefix(project_dir)
            .unwrap_or(path)
            .to_slash_lossy()
            .into_owned()
    };
    let mut stale: BTreeSet<_> = inputs
        .iter()
        .filter(|path| is_freshness_input_stale(path, manifest_mtime))
        .map(|path| display(path))
        .collect();
    let mut deleted = BTreeSet::new();
    let mut inspect_file =
        |root: &parser::package_roots::ResourceRoot, relative: &Path, required: bool| {
            let label = display(&root.display_path.join(relative));
            match std::fs::metadata(root.path.join(relative)).and_then(|m| m.modified()) {
                Ok(mtime) if mtime > manifest_mtime => {
                    stale.insert(label);
                    None
                }
                Ok(_) => None,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if required {
                        deleted.insert(label);
                    }
                    None
                }
                Err(_) => Some("unreadable_resource_file"),
            }
        };
    let resolver = PackageRoots::new(project_dir, project);
    let mut roots = BTreeMap::new();
    for file in files {
        let package = file.package.as_deref().filter(|name| *name != project.name);
        let gap = |reason| UnverifiedFiles {
            package: package.map(str::to_owned),
            reason,
        };
        let root = match roots
            .entry(package)
            .or_insert_with(|| resolver.resolve(package))
        {
            Ok(root) => root,
            Err(reason) => {
                gaps.insert(gap(reason));
                continue;
            }
        };
        let Some(relative) = relative_resource_path(&file.path) else {
            gaps.insert(gap("invalid_resource_path"));
            continue;
        };
        if let Some(reason) = inspect_file(root, relative, true) {
            gaps.insert(gap(reason));
        }
    }
    for (package, root) in roots {
        if let (Some(package), Ok(root)) = (package, root) {
            let config_files = std::iter::once(("dbt_project.yml", true))
                .chain(OPTIONAL_PROJECT_FILES.map(|name| (name, false)));
            for (name, required) in config_files {
                if let Some(reason) = inspect_file(&root, Path::new(name), required) {
                    gaps.insert(UnverifiedFiles {
                        package: Some(package.to_owned()),
                        reason,
                    });
                }
            }
        }
    }
    ManifestStatus {
        found: true,
        is_stale: !stale.is_empty() || !deleted.is_empty(),
        stale_file_count: stale.len(),
        stale_files: stale.into_iter().collect(),
        deleted_file_count: deleted.len(),
        deleted_files: deleted.into_iter().collect(),
        unverified: gaps.into_iter().collect(),
        project_verified: true,
    }
}

pub(crate) fn check_loaded_manifest_freshness(
    project_dir: &Path,
    manifest_path: &Path,
    manifest: &parser::manifest::Manifest,
) -> render::summary::ManifestStatus {
    let Ok(mtime) = std::fs::metadata(manifest_path).and_then(|m| m.modified()) else {
        return render::summary::ManifestStatus::default();
    };
    let project = parser::project::DbtProject::load(project_dir).ok();
    let files: Vec<_> = manifest.referenced_files().into_iter().collect();
    assess_freshness(
        project_dir,
        project.as_ref(),
        manifest.metadata.project_name.as_deref(),
        &files,
        mtime,
    )
}

/// Run the `check-manifest` subcommand.
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
    let report = parser::manifest::load_manifest_report(&manifest_path)?;
    warn_manifest_diagnostics(&report.diagnostics);
    let status =
        check_loaded_manifest_freshness(&project_dir, &manifest_path, &report.into_manifest()?);

    match args.output {
        CheckManifestOutputFormat::Text if !args.quiet => {
            if status.is_stale {
                println!(
                    "manifest.json is stale ({} newer, {} deleted):",
                    status.stale_file_count, status.deleted_file_count
                );
                if !status.stale_files.is_empty() {
                    println!("Files newer than manifest:");
                    for file in &status.stale_files {
                        println!("  {file}");
                    }
                }
                if !status.deleted_files.is_empty() {
                    println!("Files referenced in manifest but not found:");
                    for file in &status.deleted_files {
                        println!("  {file}");
                    }
                }
            } else if status.verification() == "verified" {
                println!("manifest.json is up-to-date");
            }
            for gap in &status.unverified {
                println!(
                    "Freshness unverified for {} ({})",
                    gap.package.as_deref().unwrap_or("project"),
                    gap.reason
                );
            }
        }
        CheckManifestOutputFormat::Text => {}
        CheckManifestOutputFormat::Json => {
            use std::io::Write;
            let mut result = serde_json::to_value(&status)?;
            result.as_object_mut().unwrap().remove("found");
            result["manifest_path"] = serde_json::json!(manifest_path);
            result["verification"] = serde_json::json!(status.verification());
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let res = if std::io::IsTerminal::is_terminal(&stdout) {
                serde_json::to_writer_pretty(&mut out, &result)
            } else {
                serde_json::to_writer(&mut out, &result)
            };
            if let Err(e) = res {
                if e.io_error_kind() != Some(std::io::ErrorKind::BrokenPipe) {
                    return Err(e.into());
                }
            } else if let Err(e) = writeln!(out)
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e.into());
            }
        }
    }
    if status.is_stale {
        let _ = std::io::Write::flush(&mut std::io::stdout());
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_freshness_input_stale;
    use std::time::SystemTime;

    #[test]
    fn unavailable_freshness_input_is_stale() {
        let temp_dir = tempfile::tempdir().unwrap();
        let missing_file = temp_dir.path().join("removed.sql");

        assert!(is_freshness_input_stale(&missing_file, SystemTime::now()));
    }
}
