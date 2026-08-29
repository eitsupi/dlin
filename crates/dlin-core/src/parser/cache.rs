use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::jinja::JinjaExtraction;

/// Cache file name used when cache is stored under the project directory
const CACHE_DIR: &str = ".dlin_cache";
const CACHE_FILENAME: &str = "extraction_cache.json";

/// A single cached extraction entry
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    /// Hash of the SQL bytes and the effective Jinja environment.
    input_hash: u64,
    /// Extraction result
    extraction: JinjaExtraction,
}

/// Opaque semantic input identity for one SQL extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExtractionInputHash(u64);

/// On-disk cache for minijinja extraction results
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    /// dlin version that created this cache.
    /// If the version changes, all entries are invalidated.
    version: String,
    /// Per-file extraction results keyed by relative path
    entries: HashMap<String, CacheEntry>,
}

/// In-memory extraction cache that can be loaded from and saved to disk
pub(crate) struct ExtractionCache {
    version: String,
    /// Hash of the exact macro prefix and deterministic effective vars used by
    /// MiniJinja. The dlin package version remains the cache compatibility
    /// boundary; there is intentionally no independent cache schema version.
    environment_hash: u64,
    entries: HashMap<String, CacheEntry>,
    /// `None` when the cache is disabled (no-op mode).
    cache_path: Option<PathBuf>,
    dirty: bool,
}

impl ExtractionCache {
    /// Create a no-op cache that never reads from or writes to disk.
    pub(crate) fn disabled() -> Self {
        Self {
            version: String::new(),
            environment_hash: 0,
            entries: HashMap::new(),
            cache_path: None,
            dirty: false,
        }
    }

    /// Load the cache from disk, or create an empty one.
    /// Entries are discarded when the dlin version doesn't match. Each entry
    /// additionally validates its SQL bytes and effective Jinja environment.
    ///
    /// When `cache_dir` is `None`, the cache is stored under
    /// `<project_dir>/.dlin_cache/extraction_cache.json`. When `cache_dir` is
    /// provided, the cache file is placed directly inside it.
    pub(crate) fn load(
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
        let environment_hash = hash_environment(macro_prefix, vars);

        let entries = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<CacheFile>(&content).ok())
            .filter(|cf| cf.version == version)
            .map(|cf| cf.entries)
            .unwrap_or_default();

        Self {
            version,
            environment_hash,
            entries,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Create an empty cache that ignores any existing on-disk entries but
    /// still writes results to disk on [`save`](Self::save).
    /// Used by `--refresh-cache` to rebuild the cache from scratch.
    pub(crate) fn fresh(
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
            environment_hash: hash_environment(macro_prefix, vars),
            entries: HashMap::new(),
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    /// Look up a cached extraction for the given file path.
    /// Returns `None` if not cached or if the SQL bytes have changed.
    pub(crate) fn input_hash(&self, sql_bytes: &[u8]) -> ExtractionInputHash {
        ExtractionInputHash(hash_sql_input(sql_bytes, self.environment_hash))
    }

    pub(crate) fn get(
        &self,
        path: &Path,
        project_dir: &Path,
        input_hash: ExtractionInputHash,
    ) -> Option<&JinjaExtraction> {
        let key = relative_key(path, project_dir);
        let entry = self.entries.get(&key)?;
        if entry.input_hash == input_hash.0 {
            Some(&entry.extraction)
        } else {
            None
        }
    }

    /// Insert an extraction result into the cache.
    pub(crate) fn insert(
        &mut self,
        path: &Path,
        project_dir: &Path,
        input_hash: ExtractionInputHash,
        extraction: &JinjaExtraction,
    ) {
        let key = relative_key(path, project_dir);
        self.entries.insert(
            key,
            CacheEntry {
                input_hash: input_hash.0,
                extraction: extraction.clone(),
            },
        );
        self.dirty = true;
    }

    /// Save the cache to disk if it has been modified.
    pub(crate) fn save(&self) {
        let cache_path = match (&self.cache_path, self.dirty) {
            (Some(p), true) => p,
            _ => return,
        };
        let cf = CacheFile {
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
                crate::warn!("could not serialize cache: {}", e);
            }
        }
    }
}

/// Simple string hash using FNV-1a for deterministic, fast hashing
#[allow(dead_code)]
pub(crate) fn hash_str(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Hash the exact macro prefix and effective vars used by MiniJinja.
fn hash_environment(macro_prefix: &str, vars: &HashMap<String, serde_json::Value>) -> u64 {
    let vars_json = canonical_vars(vars);
    hash_parts(&[
        (b"extraction-cache:macro-prefix", macro_prefix.as_bytes()),
        (b"extraction-cache:effective-vars", vars_json.as_bytes()),
    ])
}

/// Hash SQL bytes together with the effective Jinja environment.
fn hash_sql_input(sql_bytes: &[u8], environment_hash: u64) -> u64 {
    let environment_hash = environment_hash.to_le_bytes();
    hash_parts(&[
        (b"extraction-cache:sql-bytes", sql_bytes),
        (b"extraction-cache:jinja-environment", &environment_hash),
    ])
}

fn hash_parts(parts: &[(&[u8], &[u8])]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for (label, part) in parts {
        hash ^= label.len() as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        for byte in *label {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= part.len() as u64;
        hash = hash.wrapping_mul(0x100000001b3);
        for byte in *part {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    hash
}

/// Serialize project vars with object keys sorted recursively.
fn canonical_vars(vars: &HashMap<String, serde_json::Value>) -> String {
    let mut keys: Vec<&String> = vars.keys().collect();
    keys.sort();
    let mut output = String::from("{");
    for (index, key) in keys.into_iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        output.push_str(&serde_json::to_string(key).expect("string serialization cannot fail"));
        output.push(':');
        write_canonical_json(&vars[key], &mut output);
    }
    output.push('}');
    output
}

fn write_canonical_json(value: &serde_json::Value, output: &mut String) {
    match value {
        serde_json::Value::Object(object) => {
            let mut keys: Vec<&String> = object.keys().collect();
            keys.sort();
            output.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(
                    &serde_json::to_string(key).expect("string serialization cannot fail"),
                );
                output.push(':');
                write_canonical_json(&object[key], output);
            }
            output.push('}');
        }
        serde_json::Value::Array(array) => {
            output.push('[');
            for (index, item) in array.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical_json(item, output);
            }
            output.push(']');
        }
        _ => output.push_str(&value.to_string()),
    }
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
    use filetime::{FileTime, set_file_mtime};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_cache_hit() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(
            cache
                .get(&sql_file, project_dir, cache.input_hash(sql))
                .is_none()
        );

        let extraction = JinjaExtraction {
            refs: vec![RefCall {
                package: None,
                name: "orders".to_string(),
                version: None,
            }],
            sources: vec![],
            config: SqlConfig::default(),
        };
        let input_hash = cache.input_hash(sql);
        cache.insert(&sql_file, project_dir, input_hash, &extraction);
        cache.save();

        // Reload from disk
        let cache2 = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let hit = cache2
            .get(&sql_file, project_dir, cache2.input_hash(sql))
            .unwrap();
        assert_eq!(hit.refs.len(), 1);
        assert_eq!(hit.refs[0].name, "orders");
    }

    #[test]
    fn test_cache_hit_for_jinja_sql_uses_actual_relative_path() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("models/orders.sql.j2");
        fs::create_dir_all(sql_file.parent().unwrap()).unwrap();
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        let cache_path = project_dir.join(CACHE_DIR).join(CACHE_FILENAME);
        let content = fs::read_to_string(cache_path).unwrap();
        let cache_file: CacheFile = serde_json::from_str(&content).unwrap();
        assert!(cache_file.entries.contains_key("models/orders.sql.j2"));

        let reloaded = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(
            reloaded
                .get(&sql_file, project_dir, reloaded.input_hash(sql))
                .is_some()
        );
    }

    #[test]
    fn test_cache_invalidated_by_macro_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix_v1", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        // Different macro prefix → cache miss
        let cache2 = ExtractionCache::load(project_dir, "prefix_v2", &HashMap::new(), None);
        assert!(
            cache2
                .get(&sql_file, project_dir, cache2.input_hash(sql))
                .is_none()
        );
    }

    #[test]
    fn test_cache_invalidated_by_same_size_file_change_without_sleep() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();
        let original_mtime =
            FileTime::from_last_modification_time(&fs::metadata(&sql_file).unwrap());

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        // Modify file without changing its size or waiting for its mtime.
        let changed_sql = b"SELECT 2";
        fs::write(&sql_file, changed_sql).unwrap();
        set_file_mtime(&sql_file, original_mtime).unwrap();
        let changed_mtime =
            FileTime::from_last_modification_time(&fs::metadata(&sql_file).unwrap());
        assert_eq!(sql.len(), changed_sql.len());
        assert_eq!(original_mtime.unix_seconds(), changed_mtime.unix_seconds());

        let cache2 = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(
            cache2
                .get(&sql_file, project_dir, cache2.input_hash(changed_sql))
                .is_none()
        );
    }

    #[test]
    fn test_cache_invalidated_by_size_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );

        // Tamper with the entry to simulate an invalid input hash.
        let key = relative_key(&sql_file, project_dir);
        if let Some(entry) = cache.entries.get_mut(&key) {
            entry.input_hash = entry.input_hash.wrapping_add(1);
        }

        assert!(
            cache
                .get(&sql_file, project_dir, cache.input_hash(sql))
                .is_none()
        );
    }

    #[test]
    fn test_shared_cache_dir_does_not_reuse_different_project_content() {
        let tmp = tempdir().unwrap();
        let project_a = tmp.path().join("project-a");
        let project_b = tmp.path().join("project-b");
        let cache_dir = tmp.path().join("shared-cache");
        fs::create_dir_all(&project_a).unwrap();
        fs::create_dir_all(&project_b).unwrap();
        let sql_a = b"SELECT 1";
        let sql_b = b"SELECT 2";
        let file_a = project_a.join("models/orders.sql");
        let file_b = project_b.join("models/orders.sql");
        fs::create_dir_all(file_a.parent().unwrap()).unwrap();
        fs::create_dir_all(file_b.parent().unwrap()).unwrap();
        fs::write(&file_a, sql_a).unwrap();
        fs::write(&file_b, sql_b).unwrap();

        let mut cache_a =
            ExtractionCache::load(&project_a, "prefix", &HashMap::new(), Some(&cache_dir));
        let input_hash = cache_a.input_hash(sql_a);
        cache_a.insert(&file_a, &project_a, input_hash, &JinjaExtraction::default());
        cache_a.save();

        let mut cache_b =
            ExtractionCache::load(&project_b, "prefix", &HashMap::new(), Some(&cache_dir));
        assert!(
            cache_b
                .get(&file_b, &project_b, cache_b.input_hash(sql_b))
                .is_none()
        );
        let input_hash = cache_b.input_hash(sql_b);
        cache_b.insert(&file_b, &project_b, input_hash, &JinjaExtraction::default());
        cache_b.save();

        let cache_a_again =
            ExtractionCache::load(&project_a, "prefix", &HashMap::new(), Some(&cache_dir));
        assert!(
            cache_a_again
                .get(&file_a, &project_a, cache_a_again.input_hash(sql_a))
                .is_none()
        );
    }

    #[test]
    fn test_gitignore_created_on_save() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
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
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        // Pre-create .gitignore with custom content
        let dlin_dir = project_dir.join(".dlin_cache");
        fs::create_dir_all(&dlin_dir).unwrap();
        fs::write(dlin_dir.join(".gitignore"), "custom\n").unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
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
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache =
            ExtractionCache::load(project_dir, "prefix", &HashMap::new(), Some(&cache_dir));
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
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
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        // Tamper with version in the saved file
        let cache_path = project_dir.join(CACHE_DIR).join(CACHE_FILENAME);
        let content = fs::read_to_string(&cache_path).unwrap();
        let mut cf: CacheFile = serde_json::from_str(&content).unwrap();
        cf.version = "0.0.0-fake".to_string();
        fs::write(&cache_path, serde_json::to_string(&cf).unwrap()).unwrap();

        // Reload → entries should be discarded
        let cache2 = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(
            cache2
                .get(&sql_file, project_dir, cache2.input_hash(sql))
                .is_none()
        );
    }

    #[test]
    fn test_corrupt_cache_fails_open() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();
        let cache_path = project_dir.join(CACHE_DIR).join(CACHE_FILENAME);
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, "not valid json").unwrap();

        let cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        assert!(
            cache
                .get(&sql_file, project_dir, cache.input_hash(sql))
                .is_none()
        );
    }

    #[test]
    fn test_cache_invalidated_by_vars_change() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut vars = HashMap::new();
        vars.insert("schema".to_string(), serde_json::json!("staging"));

        let mut cache = ExtractionCache::load(project_dir, "prefix", &vars, None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        // Different vars → cache miss
        let mut vars2 = HashMap::new();
        vars2.insert("schema".to_string(), serde_json::json!("production"));
        let cache2 = ExtractionCache::load(project_dir, "prefix", &vars2, None);
        assert!(
            cache2
                .get(&sql_file, project_dir, cache2.input_hash(sql))
                .is_none()
        );
    }

    #[test]
    fn test_cache_valid_with_same_vars() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut vars = HashMap::new();
        vars.insert("schema".to_string(), serde_json::json!("staging"));

        let mut cache = ExtractionCache::load(project_dir, "prefix", &vars, None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        // Same vars → cache hit
        let cache2 = ExtractionCache::load(project_dir, "prefix", &vars, None);
        assert!(
            cache2
                .get(&sql_file, project_dir, cache2.input_hash(sql))
                .is_some()
        );
    }

    #[test]
    fn test_cache_valid_with_reordered_nested_vars() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        let mut nested_a = serde_json::Map::new();
        nested_a.insert("first".to_string(), serde_json::json!(1));
        nested_a.insert("second".to_string(), serde_json::json!(2));
        let mut vars_a = HashMap::new();
        vars_a.insert("config".to_string(), serde_json::Value::Object(nested_a));

        let mut cache = ExtractionCache::load(project_dir, "prefix", &vars_a, None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        let mut nested_b = serde_json::Map::new();
        nested_b.insert("second".to_string(), serde_json::json!(2));
        nested_b.insert("first".to_string(), serde_json::json!(1));
        let mut vars_b = HashMap::new();
        vars_b.insert("config".to_string(), serde_json::Value::Object(nested_b));
        let cache2 = ExtractionCache::load(project_dir, "prefix", &vars_b, None);
        assert!(
            cache2
                .get(&sql_file, project_dir, cache2.input_hash(sql))
                .is_some()
        );
    }

    #[test]
    fn test_fresh_ignores_existing_but_saves() {
        let tmp = tempdir().unwrap();
        let project_dir = tmp.path();
        let sql_file = project_dir.join("model.sql");
        let sql = b"SELECT 1";
        fs::write(&sql_file, sql).unwrap();

        // Populate cache
        let mut cache = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let input_hash = cache.input_hash(sql);
        cache.insert(
            &sql_file,
            project_dir,
            input_hash,
            &JinjaExtraction::default(),
        );
        cache.save();

        // Fresh cache ignores existing entries
        let fresh = ExtractionCache::fresh(project_dir, "prefix", &HashMap::new(), None);
        assert!(
            fresh
                .get(&sql_file, project_dir, fresh.input_hash(sql))
                .is_none()
        );

        // But can still save new entries
        let mut fresh = ExtractionCache::fresh(project_dir, "prefix", &HashMap::new(), None);
        let extraction = JinjaExtraction {
            refs: vec![RefCall {
                package: None,
                name: "fresh_ref".to_string(),
                version: None,
            }],
            sources: vec![],
            config: SqlConfig::default(),
        };
        let input_hash = fresh.input_hash(sql);
        fresh.insert(&sql_file, project_dir, input_hash, &extraction);
        fresh.save();

        // Reload → new entry is there
        let reloaded = ExtractionCache::load(project_dir, "prefix", &HashMap::new(), None);
        let hit = reloaded
            .get(&sql_file, project_dir, reloaded.input_hash(sql))
            .unwrap();
        assert_eq!(hit.refs[0].name, "fresh_ref");
    }
}
