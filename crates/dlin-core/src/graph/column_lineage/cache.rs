use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::parser::cache::hash_str;

use super::ModelColumnLineage;
use super::backend::{BackendId, DlinDialect};

// --- Column lineage disk cache ---

pub(super) const COLUMN_LINEAGE_CACHE_FILENAME: &str = "column_lineage_cache.json";
pub(super) const CACHE_DIR: &str = ".dlin_cache";

/// Cache key identifying which backend and dialect produced a cached analysis.
/// Distinct from the source-code hash: two analyses of the same SQL under a
/// different backend or dialect are not interchangeable.
fn analysis_key(backend: BackendId, dialect: DlinDialect) -> String {
    format!(
        "column-lineage/v2;backend={};dialect={}",
        backend.as_str(),
        dialect.as_str()
    )
}

/// The cache file format version. Bumped independently of `analysis_key` so
/// that changing what a cache entry's key looks like (as opposed to what it
/// contains) also invalidates old entries: their key was computed from a
/// different, incompatible scheme (the debug-formatted spelling of an
/// external dialect enum, with no backend identity at all).
fn cache_format_version() -> String {
    format!("{}:column-lineage-cache-v2", env!("CARGO_PKG_VERSION"))
}

/// A single cached column lineage entry for one model
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ColumnLineageCacheEntry {
    /// FNV-1a hash of the model's compiled SQL
    compiled_code_hash: u64,
    /// Backend and dialect that produced this entry (see [`analysis_key`])
    analysis_key: String,
    /// FNV-1a hash covering the model's YAML columns, compiled SQL, and the same
    /// for all transitive upstream dependencies. Captures any schema or SQL change
    /// that could alter the lineage result, not just manifest column definitions.
    /// Defaults to 0 for cache entries created before this field was added,
    /// which effectively invalidates them since the computed hash will differ.
    #[serde(default)]
    manifest_columns_hash: u64,
    #[serde(default)]
    manifest_mtime_secs: u64,
    #[serde(default)]
    manifest_mtime_nanos: u32,
    #[serde(default)]
    manifest_size_bytes: u64,
    /// Cached lineage result
    lineage: ModelColumnLineage,
}

/// On-disk cache file structure
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ColumnLineageCacheFile {
    /// dlin version that created this cache
    #[serde(default)]
    pub(super) version: String,
    /// Per-model cached entries keyed by model name
    entries: HashMap<String, ColumnLineageCacheEntry>,
}

/// In-memory cache for column lineage results that can be loaded from and saved to disk
pub struct ColumnLineageCache {
    version: String,
    entries: HashMap<String, ColumnLineageCacheEntry>,
    /// `None` when the cache is disabled (no-op mode).
    cache_path: Option<PathBuf>,
    dirty: bool,
}

impl ColumnLineageCache {
    /// Create a no-op cache that never reads from or writes to disk.
    pub fn disabled() -> Self {
        Self {
            version: String::new(),
            entries: HashMap::new(),
            cache_path: None,
            dirty: false,
        }
    }

    /// Load the cache from disk, or create an empty one.
    /// Entries are discarded when the dlin version doesn't match.
    pub fn load(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(COLUMN_LINEAGE_CACHE_FILENAME),
            None => project_dir
                .join(CACHE_DIR)
                .join(COLUMN_LINEAGE_CACHE_FILENAME),
        };
        let version = cache_format_version();

        let entries = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<ColumnLineageCacheFile>(&content).ok())
            .filter(|cf| cf.version == version)
            .map(|cf| cf.entries)
            .unwrap_or_default();

        Self {
            version,
            entries,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Create an empty cache that ignores existing on-disk entries but
    /// still writes results to disk on [`save`](Self::save).
    pub fn fresh(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(COLUMN_LINEAGE_CACHE_FILENAME),
            None => project_dir
                .join(CACHE_DIR)
                .join(COLUMN_LINEAGE_CACHE_FILENAME),
        };
        Self {
            version: cache_format_version(),
            entries: HashMap::new(),
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Look up a cached lineage result for the given model.
    /// Returns `None` if not cached or if compiled_code/dialect/manifest_columns_hash have changed.
    pub fn get(
        &self,
        model_name: &str,
        compiled_code: &str,
        dialect: DlinDialect,
        backend: BackendId,
        manifest_path: Option<&Path>,
        manifest_columns_hash: Option<u64>,
    ) -> Option<&ModelColumnLineage> {
        let entry = self.entries.get(model_name)?;
        let code_hash = hash_str(compiled_code);
        let key = analysis_key(backend, dialect);
        if entry.compiled_code_hash != code_hash || entry.analysis_key != key {
            return None;
        }
        // Fast negative check: if manifest stat differs, this cache entry is stale.
        if let Some(path) = manifest_path
            && let Some(stat) = manifest_stat(path)
            && (entry.manifest_mtime_secs != stat.mtime_secs
                || entry.manifest_mtime_nanos != stat.mtime_nanos
                || entry.manifest_size_bytes != stat.size_bytes)
        {
            return None;
        }
        if let Some(hash) = manifest_columns_hash
            && entry.manifest_columns_hash == hash
        {
            return Some(&entry.lineage);
        }
        None
    }

    /// Insert a lineage result into the cache.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        model_name: &str,
        compiled_code: &str,
        dialect: DlinDialect,
        backend: BackendId,
        manifest_columns_hash: u64,
        manifest_path: Option<&Path>,
        lineage: ModelColumnLineage,
    ) {
        let stat = manifest_path.and_then(manifest_stat);
        self.entries.insert(
            model_name.to_string(),
            ColumnLineageCacheEntry {
                compiled_code_hash: hash_str(compiled_code),
                analysis_key: analysis_key(backend, dialect),
                manifest_columns_hash,
                manifest_mtime_secs: stat.as_ref().map_or(0, |s| s.mtime_secs),
                manifest_mtime_nanos: stat.as_ref().map_or(0, |s| s.mtime_nanos),
                manifest_size_bytes: stat.as_ref().map_or(0, |s| s.size_bytes),
                lineage,
            },
        );
        self.dirty = true;
    }

    /// Save the cache to disk if it has been modified.
    pub fn save(&self) {
        let cache_path = match (&self.cache_path, self.dirty) {
            (Some(p), true) => p,
            _ => return,
        };
        let cf = ColumnLineageCacheFile {
            version: self.version.clone(),
            entries: self.entries.clone(),
        };
        if let Some(parent) = cache_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                crate::warn!("could not create cache directory: {}", parent.display());
                return;
            }
            // Auto-create .gitignore to prevent accidental commits
            let gitignore = parent.join(".gitignore");
            if !gitignore.exists()
                && let Err(e) = std::fs::write(&gitignore, "# Automatically created by dlin\n*\n")
            {
                crate::warn!("could not create {}: {}", gitignore.display(), e);
            }
        }
        match serde_json::to_string(&cf) {
            Ok(json) => {
                if let Err(e) = std::fs::write(cache_path, json) {
                    crate::warn!("could not write cache file {}: {}", cache_path.display(), e);
                }
            }
            Err(e) => {
                crate::warn!("could not serialize column lineage cache: {}", e);
            }
        }
    }
}

struct ManifestStat {
    mtime_secs: u64,
    mtime_nanos: u32,
    size_bytes: u64,
}

fn manifest_stat(path: &Path) -> Option<ManifestStat> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?;
    Some(ManifestStat {
        mtime_secs: mtime.as_secs(),
        mtime_nanos: mtime.subsec_nanos(),
        size_bytes: meta.len(),
    })
}
