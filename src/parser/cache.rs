use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::jinja::JinjaExtraction;

/// Cache file name used when cache is stored under the project directory
const CACHE_DIR: &str = ".dlin_cache";
const CACHE_FILENAME: &str = "extraction_cache.json";

/// A single cached extraction entry
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    /// File modification time (seconds since UNIX epoch)
    mtime_secs: u64,
    /// File size in bytes (secondary check for same-second modifications)
    file_size: u64,
    /// Extraction result
    extraction: JinjaExtraction,
}

/// On-disk cache for minijinja extraction results
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    /// Hash of the macro prefix used during extraction.
    /// If macros change, all entries are invalidated.
    macro_prefix_hash: u64,
    /// Per-file extraction results keyed by relative path
    entries: HashMap<String, CacheEntry>,
}

/// In-memory extraction cache that can be loaded from and saved to disk
pub struct ExtractionCache {
    macro_prefix_hash: u64,
    entries: HashMap<String, CacheEntry>,
    /// `None` when the cache is disabled (no-op mode).
    cache_path: Option<PathBuf>,
    dirty: bool,
}

impl ExtractionCache {
    /// Create a no-op cache that never reads from or writes to disk.
    pub fn disabled() -> Self {
        Self {
            macro_prefix_hash: 0,
            entries: HashMap::new(),
            cache_path: None,
            dirty: false,
        }
    }

    /// Load the cache from disk, or create an empty one.
    /// If the macro prefix hash doesn't match, all entries are discarded.
    ///
    /// When `cache_dir` is `None`, the cache is stored under
    /// `<project_dir>/.dlin_cache/extraction_cache.json`. When `cache_dir` is
    /// provided, the cache file is placed directly inside it.
    pub fn load(project_dir: &Path, macro_prefix: &str, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(CACHE_FILENAME),
            None => project_dir.join(CACHE_DIR).join(CACHE_FILENAME),
        };
        let hash = hash_str(macro_prefix);

        let entries = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<CacheFile>(&content).ok())
            .filter(|cf| cf.macro_prefix_hash == hash)
            .map(|cf| cf.entries)
            .unwrap_or_default();

        Self {
            macro_prefix_hash: hash,
            entries,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Look up a cached extraction for the given file path.
    /// Returns `None` if not cached or if the file has been modified.
    pub fn get(&self, path: &Path, project_dir: &Path) -> Option<&JinjaExtraction> {
        let key = relative_key(path, project_dir);
        let entry = self.entries.get(&key)?;
        let stat = file_stat(path)?;
        if entry.mtime_secs == stat.mtime_secs && entry.file_size == stat.file_size {
            Some(&entry.extraction)
        } else {
            None
        }
    }

    /// Insert an extraction result into the cache.
    pub fn insert(&mut self, path: &Path, project_dir: &Path, extraction: &JinjaExtraction) {
        let key = relative_key(path, project_dir);
        if let Some(stat) = file_stat(path) {
            self.entries.insert(
                key,
                CacheEntry {
                    mtime_secs: stat.mtime_secs,
                    file_size: stat.file_size,
                    extraction: extraction.clone(),
                },
            );
            self.dirty = true;
        }
    }

    /// Save the cache to disk if it has been modified.
    pub fn save(&self) {
        let cache_path = match (&self.cache_path, self.dirty) {
            (Some(p), true) => p,
            _ => return,
        };
        let cf = CacheFile {
            macro_prefix_hash: self.macro_prefix_hash,
            entries: self.entries.clone(),
        };
        if let Some(parent) = cache_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                crate::warn!("could not create cache directory: {}", parent.display());
                return;
            }
            // Auto-create .gitignore to prevent accidental commits
            let gitignore = parent.join(".gitignore");
            if !gitignore.exists() {
                if let Err(e) = std::fs::write(
                    &gitignore,
                    "# Automatically created by dlin\n*\n",
                ) {
                    crate::warn!("could not create {}: {}", gitignore.display(), e);
                }
            }
        }
        match serde_json::to_string(&cf) {
            Ok(json) => {
                if let Err(e) = std::fs::write(cache_path, json) {
                    crate::warn!("could not write cache file {}: {}", cache_path.display(), e);
                }
            }
            Err(e) => {
                crate::warn!("could not serialize cache: {}", e);
            }
        }
    }
}

/// Simple string hash using FNV-1a for deterministic, fast hashing
fn hash_str(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// File metadata relevant for cache invalidation
struct FileStat {
    mtime_secs: u64,
    file_size: u64,
}

/// Get file modification time and size from a single stat call
fn file_stat(path: &Path) -> Option<FileStat> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(FileStat {
        mtime_secs,
        file_size: meta.len(),
    })
}

/// Convert an absolute path to a relative key string for cache storage
fn relative_key(path: &Path, project_dir: &Path) -> String {
    path.strip_prefix(project_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::sql::{RefCall, SqlConfig};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_cache_hit() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", None);
        assert!(cache.get(&sql_file, project_dir).is_none());

        let extraction = JinjaExtraction {
            refs: vec![RefCall {
                package: None,
                name: "orders".to_string(),
            }],
            sources: vec![],
            config: SqlConfig::default(),
        };
        cache.insert(&sql_file, project_dir, &extraction);
        cache.save();

        // Reload from disk
        let cache2 = ExtractionCache::load(project_dir, "prefix", None);
        let hit = cache2.get(&sql_file, project_dir).unwrap();
        assert_eq!(hit.refs.len(), 1);
        assert_eq!(hit.refs[0].name, "orders");
    }

    #[test]
    fn test_cache_invalidated_by_macro_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix_v1", None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Different macro prefix → cache miss
        let cache2 = ExtractionCache::load(project_dir, "prefix_v2", None);
        assert!(cache2.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_cache_invalidated_by_file_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Modify file (change both mtime and size)
        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&sql_file, "SELECT 1, 2, 3").unwrap();

        let cache2 = ExtractionCache::load(project_dir, "prefix", None);
        assert!(cache2.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_cache_invalidated_by_size_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());

        // Tamper with the entry to have the correct mtime but wrong size
        let key = relative_key(&sql_file, project_dir);
        if let Some(entry) = cache.entries.get_mut(&key) {
            entry.file_size += 1;
        }

        assert!(cache.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_gitignore_created_on_save() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        let gitignore = project_dir.join(".dlin_cache/.gitignore");
        assert!(gitignore.exists());
        let content = fs::read_to_string(&gitignore).unwrap();
        assert!(content.contains("*"));
    }

    #[test]
    fn test_gitignore_not_overwritten() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        // Pre-create .gitignore with custom content
        let dlin_dir = project_dir.join(".dlin_cache");
        fs::create_dir_all(&dlin_dir).unwrap();
        fs::write(dlin_dir.join(".gitignore"), "custom\n").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        let content = fs::read_to_string(dlin_dir.join(".gitignore")).unwrap();
        assert_eq!(content, "custom\n");
    }

    #[test]
    fn test_custom_cache_dir() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let cache_dir = tmp.path().join("my_cache");
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", Some(&cache_dir));
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Cache file should be directly in cache_dir, not nested under .dlin_cache/
        assert!(cache_dir.join(CACHE_FILENAME).exists());
        assert!(!cache_dir.join(CACHE_DIR).exists());
    }
}
