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
    mtime_secs: u64,
    file_size: u64,
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
        if entry.mtime_secs == stat.mtime_secs && entry.file_size == stat.file_size {
            Some(&entry.graph)
        } else {
            None
        }
    }

    pub fn insert(&mut self, manifest_path: &Path, graph: &LineageGraph) {
        if let Some(stat) = file_stat(manifest_path) {
            self.entry = Some(ManifestCacheEntry {
                mtime_secs: stat.mtime_secs,
                file_size: stat.file_size,
                graph: graph.clone(),
            });
            self.dirty = true;
        }
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
    file_size: u64,
}

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
