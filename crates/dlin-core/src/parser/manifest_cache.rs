use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::graph::types::LineageGraph;
use crate::parser::manifest::ManifestDiagnostic;

const CACHE_DIR: &str = ".dlin_cache";
const CACHE_FILENAME: &str = "manifest_graph_cache.json";

/// Compact model-level information persisted by the manifest cache.
///
/// The typed manifest is intentionally excluded. Consumers that need compiled
/// SQL can parse the current artifact lazily after restoring this analysis.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestAnalysis {
    graph: LineageGraph,
    diagnostics: Vec<ManifestDiagnostic>,
    project_name: Option<String>,
    referenced_file_paths: Vec<String>,
}

impl ManifestAnalysis {
    pub fn new(
        graph: LineageGraph,
        diagnostics: Vec<ManifestDiagnostic>,
        project_name: Option<String>,
        referenced_file_paths: Vec<String>,
    ) -> Self {
        Self {
            graph,
            diagnostics,
            project_name,
            referenced_file_paths,
        }
    }

    pub fn into_parts(
        self,
    ) -> (
        LineageGraph,
        Vec<ManifestDiagnostic>,
        Option<String>,
        Vec<String>,
    ) {
        (
            self.graph,
            self.diagnostics,
            self.project_name,
            self.referenced_file_paths,
        )
    }
}

#[derive(Debug, Deserialize)]
struct ManifestCacheFile {
    #[serde(default)]
    version: String,
    #[serde(default)]
    entry: Option<ManifestCacheEntry>,
}

#[derive(Debug, Deserialize)]
struct ManifestCacheEntry {
    input_hash: u64,
    analysis: ManifestAnalysis,
}

#[derive(Serialize)]
struct ManifestCacheFileRef<'a> {
    version: &'a str,
    entry: Option<ManifestCacheEntryRef<'a>>,
}

#[derive(Serialize)]
struct ManifestCacheEntryRef<'a> {
    input_hash: u64,
    analysis: &'a ManifestAnalysis,
}

/// Persistent cache for model-level manifest analysis.
pub struct ManifestAnalysisCache {
    version: String,
    entry: Option<ManifestCacheEntry>,
    cache_path: Option<PathBuf>,
    dirty: bool,
}

impl ManifestAnalysisCache {
    pub fn disabled() -> Self {
        Self {
            version: String::new(),
            entry: None,
            cache_path: None,
            dirty: false,
        }
    }

    pub fn load(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        let cache_path = cache_path(project_dir, cache_dir);
        let version = env!("CARGO_PKG_VERSION").to_string();
        let entry = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|content| serde_json::from_str::<ManifestCacheFile>(&content).ok())
            .filter(|file| file.version == version)
            .and_then(|file| file.entry);
        Self {
            version,
            entry,
            cache_path: Some(cache_path),
            dirty: false,
        }
    }

    pub fn fresh(project_dir: &Path, cache_dir: Option<&Path>) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            entry: None,
            cache_path: Some(cache_path(project_dir, cache_dir)),
            dirty: false,
        }
    }

    /// Take the candidate entry for this manifest. Since this cache stores a
    /// single slot, a miss also discards the obsolete candidate before a
    /// replacement is inserted. The ownership-taking API keeps warm command
    /// paths from cloning the graph and diagnostics.
    pub fn take_for_manifest(&mut self, manifest_bytes: &[u8]) -> Option<ManifestAnalysis> {
        self.cache_path.as_ref()?;
        let input_hash = hash_manifest_bytes(manifest_bytes);
        self.entry
            .take()
            .filter(|entry| entry.input_hash == input_hash)
            .map(|entry| entry.analysis)
    }

    pub fn insert_for_manifest(&mut self, manifest_bytes: &[u8], analysis: ManifestAnalysis) {
        let input_hash = hash_manifest_bytes(manifest_bytes);
        self.entry = Some(ManifestCacheEntry {
            input_hash,
            analysis,
        });
        self.dirty = true;
    }

    pub fn save(&self) {
        let Some(cache_path) = self.cache_path.as_ref().filter(|_| self.dirty) else {
            return;
        };
        let file = ManifestCacheFileRef {
            version: &self.version,
            entry: self.entry.as_ref().map(|entry| ManifestCacheEntryRef {
                input_hash: entry.input_hash,
                analysis: &entry.analysis,
            }),
        };
        let Some(parent) = cache_path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            crate::warn!(
                "could not create cache directory {}: {}",
                parent.display(),
                error
            );
            return;
        }
        let gitignore = parent.join(".gitignore");
        if !gitignore.exists()
            && let Err(error) = std::fs::write(&gitignore, "# Automatically created by dlin\n*\n")
        {
            crate::warn!("could not create {}: {}", gitignore.display(), error);
        }
        let Ok(json) = serde_json::to_string(&file) else {
            crate::warn!("could not serialize manifest analysis cache");
            return;
        };
        // Keep the active file valid if serialization is interrupted.
        let temporary = cache_path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&temporary, json).and_then(|()| {
            #[cfg(windows)]
            let _ = std::fs::remove_file(cache_path);
            std::fs::rename(&temporary, cache_path)
        }) {
            crate::warn!(
                "could not write cache file {}: {}",
                cache_path.display(),
                error
            );
            let _ = std::fs::remove_file(temporary);
        }
    }
}

fn cache_path(project_dir: &Path, cache_dir: Option<&Path>) -> PathBuf {
    cache_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project_dir.join(CACHE_DIR))
        .join(CACHE_FILENAME)
}

/// Deterministic content hash used to validate the current manifest bytes.
/// The package version remains the sole cache compatibility boundary.
fn hash_manifest_bytes(bytes: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut hash = FNV_OFFSET_BASIS;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::manifest::{ManifestDiagnosticKind, ManifestDiagnosticSeverity};

    fn analysis() -> ManifestAnalysis {
        ManifestAnalysis::new(
            LineageGraph::new(),
            vec![ManifestDiagnostic {
                kind: ManifestDiagnosticKind::MissingSchemaVersion,
                severity: ManifestDiagnosticSeverity::Warning,
                message: "missing".to_string(),
                hint: None,
                raw_resource: None,
                raw_type: None,
                schema: None,
            }],
            Some("project".to_string()),
            vec!["models/orders.sql".to_string()],
        )
    }

    #[test]
    fn content_hash_lookup_does_not_use_path_or_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = ManifestAnalysisCache::fresh(temp.path(), None);
        cache.insert_for_manifest(b"a", analysis());
        assert!(cache.take_for_manifest(b"a").is_some());
    }

    #[test]
    fn mismatched_slot_is_a_miss() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = ManifestAnalysisCache::fresh(temp.path(), None);
        cache.insert_for_manifest(b"a", analysis());
        assert!(cache.take_for_manifest(b"b").is_none());
    }

    #[test]
    fn compact_analysis_round_trips_through_persistence() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = ManifestAnalysisCache::fresh(temp.path(), None);
        cache.insert_for_manifest(b"manifest", analysis());
        cache.save();

        let mut loaded = ManifestAnalysisCache::load(temp.path(), None);
        let restored = loaded.take_for_manifest(b"manifest").unwrap();
        let (_, diagnostics, project_name, paths) = restored.into_parts();
        assert_eq!(project_name.as_deref(), Some("project"));
        assert_eq!(paths, ["models/orders.sql"]);
        assert_eq!(diagnostics.len(), 1);
    }

    #[test]
    fn corrupt_cache_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(CACHE_DIR).join(CACHE_FILENAME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"not json").unwrap();
        let mut loaded = ManifestAnalysisCache::load(temp.path(), None);
        assert!(loaded.take_for_manifest(b"manifest").is_none());
    }

    #[test]
    fn disabled_cache_skips_lookup_and_persistent_io() {
        let mut cache = ManifestAnalysisCache::disabled();
        assert!(cache.take_for_manifest(b"manifest").is_none());
        cache.insert_for_manifest(b"manifest", analysis());
        cache.save();
    }

    #[test]
    fn package_version_mismatch_fails_open() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = ManifestAnalysisCache::fresh(temp.path(), None);
        cache.insert_for_manifest(b"manifest", analysis());
        cache.save();

        let path = temp.path().join(CACHE_DIR).join(CACHE_FILENAME);
        let mut file: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        file["version"] = serde_json::Value::String("different-release".to_string());
        std::fs::write(
            temp.path().join(CACHE_DIR).join(CACHE_FILENAME),
            serde_json::to_vec(&file).unwrap(),
        )
        .unwrap();

        let mut loaded = ManifestAnalysisCache::load(temp.path(), None);
        assert!(loaded.take_for_manifest(b"manifest").is_none());
    }
}
