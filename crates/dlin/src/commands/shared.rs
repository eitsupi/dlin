use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;

use crate::cli::{DialectArg, SourceType};
use dlin_core::graph;
use dlin_core::graph::column_lineage::{DialectClassification, DlinDialect};
use dlin_core::parser;

#[cfg(not(tarpaulin_include))]
type DagBuildResult = Result<(
    graph::types::LineageGraph,
    Option<parser::project::DbtProject>,
    Vec<parser::manifest::ManifestDiagnostic>,
    Option<parser::manifest::Manifest>,
)>;

/// Build the lineage DAG from either a manifest file or by parsing SQL files.
#[cfg(not(tarpaulin_include))]
pub(crate) fn build_dag(
    project_dir: &Path,
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
) -> DagBuildResult {
    match source {
        SourceType::Manifest => {
            let resolved = resolve_manifest_path_or_default(manifest_path, project_dir)?;
            let mut cache = if no_cache {
                parser::manifest_cache::ManifestGraphCache::disabled()
            } else if refresh_cache {
                parser::manifest_cache::ManifestGraphCache::fresh(project_dir, cache_dir)
            } else {
                parser::manifest_cache::ManifestGraphCache::load(project_dir, cache_dir)
            };

            // Parse exactly once so diagnostics are available on cache hits.
            // Graph construction is deferred until after the cache lookup.
            let (load_report, parsed_fingerprint) =
                load_manifest_report_with_fingerprint(&resolved)?;
            let (manifest, diagnostics) = load_report.into_parts();
            let manifest = manifest.ok_or_else(|| {
                let message = diagnostics
                    .iter()
                    .find(|diagnostic| {
                        diagnostic.severity == parser::manifest::ManifestDiagnosticSeverity::Error
                    })
                    .map(|diagnostic| diagnostic.message.clone())
                    .unwrap_or_else(|| "manifest could not be loaded".to_string());
                anyhow::anyhow!(message)
            })?;
            let cached_graph = parsed_fingerprint.as_ref().and_then(|fingerprint| {
                cache.get_with_fingerprint(
                    &resolved,
                    (
                        fingerprint.mtime_secs,
                        fingerprint.mtime_nanos,
                        fingerprint.size_bytes,
                        fingerprint.content_hash,
                    ),
                )
            });
            if let Some(cached_graph) = cached_graph {
                let graph = cached_graph.clone();
                return Ok((graph, None, diagnostics, Some(manifest)));
            }
            let graph_report = parser::manifest::build_graph_from_load_report(
                parser::manifest::ManifestLoadReport::from_parts(Some(manifest), diagnostics),
            )?;
            let parser::manifest::ManifestGraphReport {
                graph,
                diagnostics,
                manifest,
                ..
            } = graph_report;
            if let Some(parsed_fingerprint) = parsed_fingerprint {
                let expected = (
                    parsed_fingerprint.mtime_secs,
                    parsed_fingerprint.mtime_nanos,
                    parsed_fingerprint.size_bytes,
                    parsed_fingerprint.content_hash,
                );
                if cache.insert_if_fingerprint_matches(&resolved, &graph, expected) {
                    cache.save();
                } else {
                    dlin_core::warn!(
                        "manifest changed after read/parse; skipping manifest graph cache write for {}",
                        resolved.display()
                    );
                }
            } else {
                dlin_core::warn!(
                    "could not compute manifest fingerprint; skipping manifest graph cache write for {}",
                    resolved.display()
                );
            }

            Ok((graph, None, diagnostics, Some(manifest)))
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
            Ok((graph, Some(project), Vec::new(), None))
        }
    }
}

/// Emit forward-compatibility diagnostics from the graph report. Missing
/// producer metadata is valid for older manifests and is intentionally not
/// surfaced by command warnings.
#[cfg(not(tarpaulin_include))]
pub(crate) fn warn_manifest_diagnostics(diagnostics: &[parser::manifest::ManifestDiagnostic]) {
    if dlin_core::is_quiet() {
        return;
    }
    for diagnostic in diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.kind.is_user_visible_warning())
    {
        if dlin_core::is_error_format_json() {
            eprintln!("{}", diagnostic.to_warning_json());
        } else {
            eprintln!("{}", diagnostic.to_warning_text());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManifestFingerprint {
    mtime_secs: u64,
    mtime_nanos: u32,
    size_bytes: u64,
    content_hash: u64,
}

fn manifest_fingerprint(meta: &std::fs::Metadata, bytes: &[u8]) -> Option<ManifestFingerprint> {
    let modified = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    Some(ManifestFingerprint {
        mtime_secs: modified.as_secs(),
        mtime_nanos: modified.subsec_nanos(),
        size_bytes: meta.len(),
        content_hash: hash_bytes(bytes),
    })
}

fn load_manifest_report_with_fingerprint(
    manifest_path: &Path,
) -> Result<(
    parser::manifest::ManifestLoadReport,
    Option<ManifestFingerprint>,
)> {
    let meta = std::fs::metadata(manifest_path).map_err(|e| {
        dlin_core::error::DbtLineageError::FileReadError {
            path: manifest_path.to_path_buf(),
            source: e,
        }
    })?;
    let bytes = std::fs::read(manifest_path).map_err(|e| {
        dlin_core::error::DbtLineageError::FileReadError {
            path: manifest_path.to_path_buf(),
            source: e,
        }
    })?;
    let load_report = parser::manifest::load_manifest_report_from_bytes(&bytes, manifest_path);
    let fingerprint = manifest_fingerprint(&meta, &bytes);
    Ok((load_report, fingerprint))
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    // FNV-1a 64-bit constants (offset basis + prime), used for lightweight
    // non-cryptographic fingerprinting of manifest content.
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

pub(crate) fn warn_sql_mode_test_limitation(source: &SourceType, has_tests: bool) {
    if matches!(source, SourceType::Sql) && has_tests {
        dlin_core::warn!(
            "sql mode infers generic tests from YAML declarations; \
             test IDs are dlin-specific and do not match dbt's naming. \
             Use --source manifest for exact dependency resolution"
        );
    }
}

/// Resolve dbt directory paths for file-path input resolution.
///
/// In SQL mode, reuses the already-loaded `DbtProject` from `build_dag`.
/// In manifest mode, tries to load `dbt_project.yml` for accurate path configuration,
/// and falls back to the standard dbt directory layout only when the file is absent.
/// A present-but-invalid `dbt_project.yml` is surfaced as an error.
#[cfg(not(tarpaulin_include))]
pub(crate) fn resolve_paths_for_path_input(
    source: SourceType,
    project_dir: &Path,
    project_opt: Option<&parser::project::DbtProject>,
) -> Result<parser::project::ResolvedPaths> {
    if let Some(project) = project_opt {
        let mut paths = project.resolve_paths(project_dir);
        if matches!(source, SourceType::Manifest) {
            // Manifest file paths are authoritative, so accept dbt's Jinja
            // SQL suffixes even when the current project flag is disabled.
            paths.allow_jinja_file_extensions = true;
        }
        return Ok(paths);
    }
    // In SQL mode, build_dag always loads DbtProject — project_opt should never be None here.
    if matches!(source, SourceType::Sql) {
        anyhow::bail!("internal error: DbtProject not available in sql mode for path resolution");
    }
    // project_opt is None in manifest mode (build_dag does not load DbtProject there).
    // Try loading dbt_project.yml for accurate path config.
    if matches!(source, SourceType::Manifest) {
        match parser::project::DbtProject::load(project_dir) {
            Ok(project) => {
                let mut paths = project.resolve_paths(project_dir);
                // Manifest file paths are authoritative, so accept dbt's
                // Jinja SQL suffixes regardless of project configuration.
                paths.allow_jinja_file_extensions = true;
                return Ok(paths);
            }
            Err(e) => {
                // Fall back to default paths only when the file is simply absent.
                // A present-but-malformed file is a real config error: surface it.
                let is_not_found = e
                    .downcast_ref::<dlin_core::error::DbtLineageError>()
                    .is_some_and(|de| {
                        matches!(de, dlin_core::error::DbtLineageError::ProjectNotFound(_))
                    });
                if !is_not_found {
                    return Err(e);
                }
            }
        }
    }
    let mut paths = parser::project::ResolvedPaths::default_for(project_dir);
    if matches!(source, SourceType::Manifest) {
        // Manifest file paths are authoritative when no project config exists.
        paths.allow_jinja_file_extensions = true;
    }
    Ok(paths)
}

/// Validate that --source and --manifest-path flags are consistent.
#[cfg(not(tarpaulin_include))]
pub(crate) fn validate_source_flags(
    source: &SourceType,
    manifest_path: Option<&PathBuf>,
) -> Result<()> {
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
pub(crate) fn resolve_manifest_path_or_default(
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

/// The effective dialect and an optional compatibility warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedDialect {
    pub(crate) dialect: DlinDialect,
    pub(crate) warning: Option<String>,
}

pub(crate) fn classify_dialect(requested: &str) -> Result<ResolvedDialect> {
    match DlinDialect::classify(requested).map_err(anyhow::Error::msg)? {
        DialectClassification::Supported(dialect) => Ok(ResolvedDialect {
            dialect,
            warning: None,
        }),
        DialectClassification::Removed(_dialect) => Ok(ResolvedDialect {
            dialect: DlinDialect::Generic,
            warning: Some(format!(
                "dialect '{}' is no longer supported by the column-lineage backend; using generic instead",
                requested
            )),
        }),
    }
}

/// Resolve the requested SQL dialect.
///
/// Precedence: CLI flag > manifest adapter_type > error. Recognized dialects
/// that the active backend has removed are downgraded to Generic with a
/// warning; missing, empty, and unknown adapter_type values remain errors.
pub(crate) fn resolve_dialect(
    cli_dialect: Option<&DialectArg>,
    manifest: &parser::manifest::Manifest,
) -> Result<ResolvedDialect> {
    match cli_dialect {
        Some(dialect) => classify_dialect(&dialect.requested),
        None => match manifest.metadata.adapter_type.as_deref() {
            Some(adapter) if !adapter.trim().is_empty() => {
                let requested = adapter.trim();
                classify_dialect(requested).map_err(|_| {
                    anyhow::anyhow!(
                        "manifest adapter_type '{}' has no matching SQL dialect; \
                         use --dialect to specify one explicitly (e.g. --dialect postgres)",
                        requested
                    )
                })
            }
            Some(_) => {
                anyhow::bail!(
                    "manifest adapter_type is empty; \
                     use --dialect to specify the SQL dialect (e.g. --dialect bigquery)"
                )
            }
            None => {
                anyhow::bail!(
                    "manifest does not specify an adapter_type; \
                     use --dialect to specify the SQL dialect (e.g. --dialect bigquery)"
                )
            }
        },
    }
}
