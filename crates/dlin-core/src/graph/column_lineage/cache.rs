use std::collections::HashMap;
use std::path::{Path, PathBuf};

use polyglot_sql::DialectType;
use serde::{Deserialize, Serialize};

use crate::parser::cache::hash_str;

use super::ModelColumnLineage;

// --- Column lineage disk cache ---

pub(super) const COLUMN_LINEAGE_CACHE_FILENAME: &str = "column_lineage_cache.json";
pub(super) const CACHE_DIR: &str = ".dlin_cache";

/// A single cached column lineage entry for one model
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ColumnLineageCacheEntry {
    /// FNV-1a hash of the model's compiled SQL
    compiled_code_hash: u64,
    /// Dialect used for parsing (e.g. "bigquery", "generic")
    dialect: String,
    /// FNV-1a hash covering the model's YAML columns, compiled SQL, and the same
    /// for all transitive upstream dependencies. Captures any schema or SQL change
    /// that could alter the lineage result, not just manifest column definitions.
    /// Defaults to 0 for cache entries created before this field was added,
    /// which effectively invalidates them since the computed hash will differ.
    #[serde(default)]
    manifest_columns_hash: u64,
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
        let version = env!("CARGO_PKG_VERSION").to_string();

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
            version: env!("CARGO_PKG_VERSION").to_string(),
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
        dialect: DialectType,
        manifest_columns_hash: u64,
    ) -> Option<&ModelColumnLineage> {
        let entry = self.entries.get(model_name)?;
        let code_hash = hash_str(compiled_code);
        let dialect_str = format!("{:?}", dialect);
        if entry.compiled_code_hash == code_hash
            && entry.dialect == dialect_str
            && entry.manifest_columns_hash == manifest_columns_hash
        {
            Some(&entry.lineage)
        } else {
            None
        }
    }

    /// Insert a lineage result into the cache.
    pub fn insert(
        &mut self,
        model_name: &str,
        compiled_code: &str,
        dialect: DialectType,
        manifest_columns_hash: u64,
        lineage: ModelColumnLineage,
    ) {
        self.entries.insert(
            model_name.to_string(),
            ColumnLineageCacheEntry {
                compiled_code_hash: hash_str(compiled_code),
                dialect: format!("{:?}", dialect),
                manifest_columns_hash,
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
