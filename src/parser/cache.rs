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
    /// dlin version that created this cache.
    /// If the version changes, all entries are invalidated.
    #[serde(default)]
    version: String,
    /// Hash of the macro prefix used during extraction.
    /// If macros change, all entries are invalidated.
    macro_prefix_hash: u64,
    /// Hash of serialized dbt project vars.
    /// If vars change, all entries are invalidated.
    #[serde(default)]
    vars_hash: u64,
    /// Per-file extraction results keyed by relative path
    entries: HashMap<String, CacheEntry>,
}

/// In-memory extraction cache that can be loaded from and saved to disk
pub struct ExtractionCache {
    version: String,
    macro_prefix_hash: u64,
    vars_hash: u64,
    entries: HashMap<String, CacheEntry>,
    /// `None` when the cache is disabled (no-op mode).
    cache_path: Option<PathBuf>,
    dirty: bool,
}

impl ExtractionCache {
    /// Create a no-op cache that never reads from or writes to disk.
    pub fn disabled() -> Self {
        Self {
            version: String::new(),
            macro_prefix_hash: 0,
            vars_hash: 0,
            entries: HashMap::new(),
            cache_path: None,
            dirty: false,
        }
    }

    /// Load the cache from disk, or create an empty one.
    /// Entries are discarded when the dlin version, macro prefix hash, or vars
    /// hash doesn't match.
    ///
    /// When `cache_dir` is `None`, the cache is stored under
    /// `<project_dir>/.dlin_cache/extraction_cache.json`. When `cache_dir` is
    /// provided, the cache file is placed directly inside it.
    pub fn load(
        project_dir: &Path,
        macro_prefix: &str,
        vars: &HashMap<String, serde_json::Value>,
        cache_dir: Option<&Path>,
    ) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(CACHE_FILENAME),
            None => project_dir.join(CACHE_DIR).join(CACHE_FILENAME),
        };
        let version = env!("CARGO_PKG_VERSION").to_string();
        let macro_hash = hash_str(macro_prefix);
        let vars_hash = hash_vars(vars);

        let entries = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<CacheFile>(&content).ok())
            .filter(|cf| {
                cf.version == version
                    && cf.macro_prefix_hash == macro_hash
                    && cf.vars_hash == vars_hash
            })
            .map(|cf| cf.entries)
            .unwrap_or_default();

        Self {
            version,
            macro_prefix_hash: macro_hash,
            vars_hash,
            entries,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Create an empty cache that ignores any existing on-disk entries but
    /// still writes results to disk on [`save`](Self::save).
    /// Used by `--refresh-cache` to rebuild the cache from scratch.
    pub fn fresh(
        project_dir: &Path,
        macro_prefix: &str,
        vars: &HashMap<String, serde_json::Value>,
        cache_dir: Option<&Path>,
    ) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(CACHE_FILENAME),
            None => project_dir.join(CACHE_DIR).join(CACHE_FILENAME),
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            macro_prefix_hash: hash_str(macro_prefix),
            vars_hash: hash_vars(vars),
            entries: HashMap::new(),
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
            version: self.version.clone(),
            macro_prefix_hash: self.macro_prefix_hash,
            vars_hash: self.vars_hash,
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

/// Hash project vars deterministically (sorted keys → JSON → FNV-1a)
fn hash_vars(vars: &HashMap<String, serde_json::Value>) -> u64 {
    if vars.is_empty() {
        return 0;
    }
    let mut keys: Vec<&String> = vars.keys().collect();
    keys.sort();
    let mut s = String::new();
    for k in keys {
        s.push_str(k);
        s.push('=');
        s.push_str(&vars[k].to_string());
        s.push('\n');
    }
    hash_str(&s)
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

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
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
        let cache2 = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
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

        let mut cache = ExtractionCache::load(project_dir, "prefix_v1", &HashMap::new(), None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Different macro prefix → cache miss
        let cache2 = ExtractionCache::load(project_dir, "prefix_v2", &HashMap::new(), None);
        assert!(cache2.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_cache_invalidated_by_file_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Modify file (change both mtime and size)
        std::thread::sleep(std::time::Duration::from_secs(1));
        fs::write(&sql_file, "SELECT 1, 2, 3").unwrap();

        let cache2 = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(cache2.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_cache_invalidated_by_size_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
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

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
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

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
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

        let mut cache =
            ExtractionCache::load(project_dir, "prefix", &HashMap::new(), Some(&cache_dir));
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Cache file should be directly in cache_dir, not nested under .dlin_cache/
        assert!(cache_dir.join(CACHE_FILENAME).exists());
        assert!(!cache_dir.join(CACHE_DIR).exists());
    }

    #[test]
    fn test_cache_invalidated_by_version_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Tamper with version in the saved file
        let cache_path = project_dir.join(CACHE_DIR).join(CACHE_FILENAME);
        let content = fs::read_to_string(&cache_path).unwrap();
        let mut cf: CacheFile = serde_json::from_str(&content).unwrap();
        cf.version = "0.0.0-fake".to_string();
        fs::write(&cache_path, serde_json::to_string(&cf).unwrap()).unwrap();

        // Reload → entries should be discarded
        let cache2 = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(cache2.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_cache_invalidated_by_vars_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut vars = HashMap::new();
        vars.insert("schema".to_string(), serde_json::json!("staging"));

        let mut cache = ExtractionCache::load(project_dir, "prefix", &vars, None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Different vars → cache miss
        let mut vars2 = HashMap::new();
        vars2.insert("schema".to_string(), serde_json::json!("production"));
        let cache2 = ExtractionCache::load(project_dir, "prefix", &vars2, None);
        assert!(cache2.get(&sql_file, project_dir).is_none());
    }

    #[test]
    fn test_cache_valid_with_same_vars() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        let mut vars = HashMap::new();
        vars.insert("schema".to_string(), serde_json::json!("staging"));

        let mut cache = ExtractionCache::load(project_dir, "prefix", &vars, None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Same vars → cache hit
        let cache2 = ExtractionCache::load(project_dir, "prefix", &vars, None);
        assert!(cache2.get(&sql_file, project_dir).is_some());
    }

    #[test]
    fn test_fresh_ignores_existing_but_saves() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        fs::write(&sql_file, "SELECT 1").unwrap();

        // Populate cache
        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        cache.insert(&sql_file, project_dir, &JinjaExtraction::default());
        cache.save();

        // Fresh cache ignores existing entries
        let fresh = ExtractionCache::fresh(project_dir, "prefix", &HashMap::new(), None);
        assert!(fresh.get(&sql_file, project_dir).is_none());

        // But can still save new entries
        let mut fresh = ExtractionCache::fresh(project_dir, "prefix", &HashMap::new(), None);
        let extraction = JinjaExtraction {
            refs: vec![RefCall {
                package: None,
                name: "fresh_ref".to_string(),
            }],
            sources: vec![],
            config: SqlConfig::default(),
        };
        fresh.insert(&sql_file, project_dir, &extraction);
        fresh.save();

        // Reload → new entry is there
        let reloaded = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let hit = reloaded.get(&sql_file, project_dir).unwrap();
        assert_eq!(hit.refs[0].name, "fresh_ref");
    }
}
