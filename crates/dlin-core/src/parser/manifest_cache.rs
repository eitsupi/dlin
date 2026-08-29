use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::graph::types::LineageGraph;

const CACHE_DIR: &str = ".dlin_cache";
const CACHE_FILENAME: &str = "manifest_graph_cache.json";

#[derive(Debug, Serialize, Deserialize)]
struct ManifestCacheFile {
    #[serde(default)]
    version: String,
    entry: Option<ManifestCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestCacheEntry {
    #[serde(default)]
    manifest_identity: String,
    mtime_secs: u64,
    #[serde(default)]
    mtime_nanos: u32,
    file_size: u64,
    #[serde(default)]
    content_hash: u64,
    graph: LineageGraph,
}

pub struct ManifestGraphCache {
    version: String,
    entry: Option<ManifestCacheEntry>,
    cache_path: Option<PathBuf>,
    dirty: bool,
}

// `CARGO_PKG_VERSION`, used by `load` and `fresh`, is the compatibility and
// invalidation boundary for both cache format and graph semantics. We avoid a
// second cache schema/semantics version: a release/version bump automatically
// discards older caches. Compatibility between development builds sharing a
// package version is not guaranteed; use `--refresh-cache` when needed.
impl ManifestGraphCache {
    pub fn disabled() -> Self {
        Self {
            version: String::new(),
            entry: None,
            cache_path: None,
            dirty: false,
        }
    }

    pub fn load(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(CACHE_FILENAME),
            None => project_dir.join(CACHE_DIR).join(CACHE_FILENAME),
        };
        let version = env!("CARGO_PKG_VERSION").to_string();
        let entry = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<ManifestCacheFile>(&content).ok())
            .filter(|cf| cf.version == version)
            .and_then(|cf| cf.entry);

        Self {
            version,
            entry,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    pub fn fresh(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = match cache_dir {
            Some(dir) => dir.join(CACHE_FILENAME),
            None => project_dir.join(CACHE_DIR).join(CACHE_FILENAME),
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            entry: None,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    pub fn get(&self, manifest_path: &Path) -> Option<&LineageGraph> {
        let entry = self.entry.as_ref()?;
        let stat = file_stat(manifest_path)?;
        if entry_matches_fingerprint(
            entry,
            manifest_path,
            (
                stat.mtime_secs,
                stat.mtime_nanos,
                stat.file_size,
                stat.content_hash,
            ),
        ) {
            Some(&entry.graph)
        } else {
            None
        }
    }

    /// Look up a cached graph using metadata and content hash computed by the
    /// caller. This avoids rereading the manifest when its bytes were already
    /// loaded for parsing and fingerprinting.
    pub fn get_with_fingerprint(
        &self,
        manifest_path: &Path,
        fingerprint: (u64, u32, u64, u64),
    ) -> Option<&LineageGraph> {
        let entry = self.entry.as_ref()?;
        entry_matches_fingerprint(entry, manifest_path, fingerprint).then_some(&entry.graph)
    }

    pub fn insert_if_fingerprint_matches(
        &mut self,
        manifest_path: &Path,
        graph: &LineageGraph,
        expected: (u64, u32, u64, u64),
    ) -> bool {
        let Some(stat) = file_stat(manifest_path) else {
            return false;
        };
        if (
            stat.mtime_secs,
            stat.mtime_nanos,
            stat.file_size,
            stat.content_hash,
        ) != expected
        {
            return false;
        }
        self.entry = Some(ManifestCacheEntry {
            manifest_identity: manifest_identity(manifest_path),
            mtime_secs: stat.mtime_secs,
            mtime_nanos: stat.mtime_nanos,
            file_size: stat.file_size,
            content_hash: stat.content_hash,
            graph: graph.clone(),
        });
        self.dirty = true;
        true
    }

    pub fn save(&self) {
        let cache_path = match (&self.cache_path, self.dirty) {
            (Some(p), true) => p,
            _ => return,
        };
        let cf = ManifestCacheFile {
            version: self.version.clone(),
            entry: self.entry.clone(),
        };
        if let Some(parent) = cache_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                crate::warn!("could not create cache directory: {}", parent.display());
                return;
            }
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
                crate::warn!("could not serialize manifest graph cache: {}", e);
            }
        }
    }
}

struct FileStat {
    mtime_secs: u64,
    mtime_nanos: u32,
    file_size: u64,
    content_hash: u64,
}

fn file_stat(path: &Path) -> Option<FileStat> {
    let meta = std::fs::metadata(path).ok()?;
    let content = std::fs::read(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let mtime_nanos = meta
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .subsec_nanos();
    Some(FileStat {
        mtime_secs,
        mtime_nanos,
        file_size: meta.len(),
        content_hash: hash_bytes(&content),
    })
}

fn entry_matches_fingerprint(
    entry: &ManifestCacheEntry,
    manifest_path: &Path,
    fingerprint: (u64, u32, u64, u64),
) -> bool {
    entry.manifest_identity == manifest_identity(manifest_path)
        && (
            entry.mtime_secs,
            entry.mtime_nanos,
            entry.file_size,
            entry.content_hash,
        ) == fingerprint
}

fn manifest_identity(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    #[test]
    fn fingerprint_lookup_uses_caller_fingerprint_without_reread() {
        let temp = tempfile::tempdir().unwrap();
        let manifest_path = temp.path().join("manifest.json");
        let bytes = br#"{}"#;
        std::fs::write(&manifest_path, bytes).unwrap();
        let metadata = std::fs::metadata(&manifest_path).unwrap();
        let modified = metadata
            .modified()
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let fingerprint = (
            modified.as_secs(),
            modified.subsec_nanos(),
            metadata.len(),
            hash_bytes(bytes),
        );

        let mut cache = ManifestGraphCache::fresh(temp.path(), None);
        let graph = LineageGraph::new();
        assert!(cache.insert_if_fingerprint_matches(&manifest_path, &graph, fingerprint));
        // Keep the path identity stable while changing the on-disk content.
        std::fs::write(&manifest_path, br#"{"changed":true}"#).unwrap();

        // The caller-computed old fingerprint is trusted and does not reread
        // the changed manifest.
        assert!(
            cache
                .get_with_fingerprint(&manifest_path, fingerprint)
                .is_some()
        );
        // The compatibility API reads and hashes the current content, so it
        // correctly misses the stale entry.
        assert!(cache.get(&manifest_path).is_none());
    }
}
