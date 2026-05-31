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
        let identity = manifest_identity(manifest_path);
        if entry.manifest_identity == identity
            && entry.mtime_secs == stat.mtime_secs
            && entry.mtime_nanos == stat.mtime_nanos
            && entry.file_size == stat.file_size
            && entry.content_hash == stat.content_hash
        {
            Some(&entry.graph)
        } else {
            None
        }
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
