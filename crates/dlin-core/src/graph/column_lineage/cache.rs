use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::InternalModelColumnLineage;
use super::backend::DlinDialect;

// --- Column lineage disk cache ---

pub(super) const COLUMN_LINEAGE_CACHE_FILENAME: &str = "column_lineage_cache.json";
pub(super) const CACHE_DIR: &str = ".dlin_cache";

/// The package version is the sole persistent cache compatibility boundary.
/// Cache formats are intentionally not versioned independently.
fn package_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// A single cached column lineage entry for one model
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ColumnLineageCacheEntry {
    /// Digest of the model and its transitive semantic dependencies.
    semantic_digest: u64,
    /// Dialect that produced this entry. The backend is fixed by this package
    /// and covered by the package-version compatibility boundary.
    dialect: String,
    /// Cached lineage result
    lineage: InternalModelColumnLineage,
}

/// On-disk cache file structure
#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ColumnLineageCacheFile {
    /// dlin version that created this cache
    #[serde(default)]
    pub(super) version: String,
    /// Per-model cached entries keyed by canonical manifest unique_id
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
        let version = package_version();

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
            version: package_version(),
            entries: HashMap::new(),
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Look up a cached lineage result for a canonical manifest unique_id.
    /// Returns `None` if the semantic digest or effective dialect changed.
    pub(super) fn get(
        &self,
        model_unique_id: &str,
        dialect: DlinDialect,
        semantic_digest: u64,
    ) -> Option<&InternalModelColumnLineage> {
        let entry = self.entries.get(model_unique_id)?;
        if entry.dialect != dialect.as_str() || entry.semantic_digest != semantic_digest {
            return None;
        }
        Some(&entry.lineage)
    }

    /// Insert a lineage result using a canonical manifest unique_id.
    pub(super) fn insert(
        &mut self,
        model_unique_id: &str,
        dialect: DlinDialect,
        semantic_digest: u64,
        lineage: InternalModelColumnLineage,
    ) {
        self.entries.insert(
            model_unique_id.to_string(),
            ColumnLineageCacheEntry {
                dialect: dialect.as_str().to_string(),
                semantic_digest,
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
