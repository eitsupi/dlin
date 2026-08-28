use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use serde::Deserialize;

use crate::graph::types::*;

/// Metadata section of manifest.json
#[derive(Debug, Default, Deserialize)]
pub struct ManifestMetadata {
    pub project_name: Option<String>,
    /// dbt adapter type (e.g. "postgres", "bigquery", "snowflake") — present in dbt >=1.x manifests.
    pub adapter_type: Option<String>,
    /// The exact schema URI emitted by dbt (for example, `.../manifest/v12/manifest.json`).
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub dbt_schema_version: Option<String>,
    /// The exact dbt version string emitted by dbt.
    #[serde(default, deserialize_with = "deserialize_optional_string")]
    pub dbt_version: Option<String>,
    /// Numeric schema version extracted from [`dbt_schema_version`], when possible.
    #[serde(skip)]
    pub dbt_schema_version_number: Option<u32>,
}

fn deserialize_optional_string<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| value.as_str().map(ToOwned::to_owned)))
}

/// Top-level manifest.json structure
#[derive(Debug, Default, Deserialize)]
pub struct Manifest {
    /// Metadata about the manifest (dbt version, project name, etc.)
    #[serde(default)]
    pub metadata: ManifestMetadata,
    /// Nodes keyed by unique_id (models, seeds, snapshots, tests, analyses)
    #[serde(default)]
    pub nodes: HashMap<String, ManifestNode>,
    /// Sources keyed by unique_id
    #[serde(default)]
    pub sources: HashMap<String, ManifestSource>,
    /// Exposures keyed by unique_id
    #[serde(default)]
    pub exposures: HashMap<String, ManifestExposure>,
    /// Semantic models keyed by unique_id (dbt Semantic Layer)
    #[serde(default)]
    pub semantic_models: HashMap<String, ManifestSemanticModel>,
    /// Metrics keyed by unique_id (dbt Semantic Layer)
    #[serde(default)]
    pub metrics: HashMap<String, ManifestMetric>,
    /// Saved queries keyed by unique_id (dbt Semantic Layer)
    #[serde(default)]
    pub saved_queries: HashMap<String, ManifestSavedQuery>,
    /// Additional dbt resource maps retained without requiring a typed model.
    #[serde(default)]
    pub macros: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub docs: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub groups: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub group_map: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub selectors: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub parent_map: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub child_map: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub unit_tests: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub functions: HashMap<String, serde_json::Value>,
    /// Disabled nodes are grouped by resource type in dbt manifests.
    #[serde(default)]
    pub disabled: Option<HashMap<String, Vec<serde_json::Value>>>,
    /// Unknown top-level fields, flattened so forward-compatible data is not lost.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
    /// Capability observations populated by the report loader.
    #[serde(skip)]
    pub capabilities: report::ManifestCapabilities,
}

/// A node entry in the manifest (model, seed, snapshot, test, analysis)
#[derive(Debug, Deserialize)]
pub struct ManifestNode {
    pub unique_id: String,
    pub name: String,
    #[serde(default)]
    pub alias: Option<String>,
    pub resource_type: String,
    #[serde(default)]
    pub depends_on: DependsOn,
    #[serde(default)]
    pub config: ManifestConfig,
    pub description: Option<String>,
    pub path: Option<String>,
    pub original_file_path: Option<String>,
    #[serde(default)]
    pub columns: HashMap<String, ManifestColumn>,
    pub compiled_code: Option<String>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
}

/// A source entry in the manifest
#[derive(Debug, Deserialize)]
pub struct ManifestSource {
    pub unique_id: String,
    pub name: String,
    pub source_name: String,
    #[serde(default)]
    pub resource_type: String,
    pub description: Option<String>,
    pub path: Option<String>,
    pub original_file_path: Option<String>,
    #[serde(default)]
    pub columns: HashMap<String, ManifestColumn>,
    #[serde(default)]
    pub database: Option<String>,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub identifier: Option<String>,
}

/// A column entry in the manifest
#[derive(Debug, Deserialize)]
pub struct ManifestColumn {
    pub name: String,
}

/// An exposure entry in the manifest
#[derive(Debug, Deserialize)]
pub struct ManifestExposure {
    pub unique_id: String,
    pub name: String,
    #[serde(default)]
    pub depends_on: DependsOn,
    pub description: Option<String>,
    pub label: Option<String>,
    #[serde(rename = "type")]
    pub exposure_type: Option<String>,
    pub url: Option<String>,
    pub maturity: Option<String>,
    pub owner: Option<ManifestExposureOwner>,
}

/// Owner information in a manifest exposure entry
#[derive(Debug, Deserialize)]
pub struct ManifestExposureOwner {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// A semantic model entry in the manifest (dbt Semantic Layer)
#[derive(Debug, Deserialize)]
pub struct ManifestSemanticModel {
    pub unique_id: String,
    pub name: String,
    pub label: Option<String>,
    #[serde(default)]
    pub depends_on: DependsOn,
    pub description: Option<String>,
    pub path: Option<String>,
    pub original_file_path: Option<String>,
}

/// A metric entry in the manifest (dbt Semantic Layer)
#[derive(Debug, Deserialize)]
pub struct ManifestMetric {
    pub unique_id: String,
    pub name: String,
    pub label: Option<String>,
    #[serde(default)]
    pub depends_on: DependsOn,
    pub description: Option<String>,
    pub path: Option<String>,
    pub original_file_path: Option<String>,
}

/// A saved query entry in the manifest (dbt Semantic Layer)
#[derive(Debug, Deserialize)]
pub struct ManifestSavedQuery {
    pub unique_id: String,
    pub name: String,
    pub label: Option<String>,
    #[serde(default)]
    pub depends_on: DependsOn,
    pub description: Option<String>,
    pub path: Option<String>,
    pub original_file_path: Option<String>,
}

/// depends_on section with a list of node unique_ids
#[derive(Debug, Default, Deserialize)]
pub struct DependsOn {
    #[serde(default)]
    pub nodes: Vec<String>,
}

/// Config section for nodes
#[derive(Debug, Default, Deserialize)]
pub struct ManifestConfig {
    #[serde(default)]
    pub alias: Option<String>,
    pub materialized: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl ManifestNode {
    pub fn relation_name(&self) -> &str {
        self.alias
            .as_deref()
            .filter(|alias| !alias.is_empty())
            .or_else(|| {
                self.config
                    .alias
                    .as_deref()
                    .filter(|alias| !alias.is_empty())
            })
            .unwrap_or(&self.name)
    }
}

fn resource_type_to_node_type(resource_type: &str) -> NodeType {
    match resource_type {
        "model" => NodeType::Model,
        "source" => NodeType::Source,
        "seed" => NodeType::Seed,
        "snapshot" => NodeType::Snapshot,
        "test" => NodeType::Test,
        "analysis" => NodeType::Model,
        "exposure" => NodeType::Exposure,
        _ => NodeType::Model,
    }
}

fn simplify_unique_id(unique_id: &str, resource_type: &str) -> String {
    let parts: Vec<&str> = unique_id.split('.').collect();
    match resource_type {
        "source" => {
            if parts.len() >= 4 {
                format!("{}.{}.{}", parts[0], parts[2], parts[3])
            } else {
                unique_id.to_string()
            }
        }
        "test" => {
            if parts.len() >= 3 {
                format!("{}.{}", parts[0], parts[2])
            } else {
                unique_id.to_string()
            }
        }
        _ => {
            if parts.len() >= 3 {
                format!("{}.{}", parts[0], parts[2..].join("."))
            } else {
                unique_id.to_string()
            }
        }
    }
}

#[derive(Debug)]
struct ManifestGraphIdentity {
    graph_id: String,
    simple_alias: Option<String>,
}

#[derive(Debug, Default)]
struct ManifestGraphResolver {
    ambiguous_simplified_ids: HashSet<String>,
}

impl ManifestGraphResolver {
    fn new(manifest: &Manifest) -> Self {
        let mut counts = HashMap::<String, usize>::new();
        let mut count = |orig_id: &str, resource_type: &str| {
            *counts
                .entry(simplify_unique_id(orig_id, resource_type))
                .or_default() += 1;
        };

        for orig_id in manifest.sources.keys() {
            count(orig_id, "source");
        }
        for (orig_id, node) in &manifest.nodes {
            count(orig_id, &node.resource_type);
        }
        for orig_id in manifest.exposures.keys() {
            count(orig_id, "exposure");
        }
        for orig_id in manifest.semantic_models.keys() {
            count(orig_id, "semantic_model");
        }
        for orig_id in manifest.metrics.keys() {
            count(orig_id, "metric");
        }
        for orig_id in manifest.saved_queries.keys() {
            count(orig_id, "saved_query");
        }

        Self {
            ambiguous_simplified_ids: counts
                .into_iter()
                .filter_map(|(simple_id, count)| (count > 1).then_some(simple_id))
                .collect(),
        }
    }

    fn resolve(&self, orig_id: &str, resource_type: &str) -> ManifestGraphIdentity {
        let simple_id = simplify_unique_id(orig_id, resource_type);
        if self.ambiguous_simplified_ids.contains(&simple_id) {
            ManifestGraphIdentity {
                graph_id: orig_id.to_string(),
                simple_alias: None,
            }
        } else {
            ManifestGraphIdentity {
                graph_id: simple_id.clone(),
                simple_alias: Some(simple_id),
            }
        }
    }
}

mod report;
pub use report::*;
mod graph;
pub use graph::{build_graph_from_manifest, build_graph_from_parsed_manifest};
#[cfg(test)]
pub(crate) use graph::{infer_edge_type, non_empty_string};

/// Load and parse a manifest.json file without building a graph.
pub fn load_manifest(manifest_path: &Path) -> Result<Manifest> {
    let content =
        std::fs::read(manifest_path).map_err(|e| crate::error::DbtLineageError::FileReadError {
            path: manifest_path.to_path_buf(),
            source: e,
        })?;

    load_manifest_from_bytes(&content, manifest_path)
}

pub fn load_manifest_from_bytes(content: &[u8], manifest_path: &Path) -> Result<Manifest> {
    Ok(
        report::load_manifest_compat_from_bytes(content).map_err(|e| {
            crate::error::DbtLineageError::ArtifactParseError {
                path: manifest_path.to_path_buf(),
                source: e,
            }
        })?,
    )
}

impl Manifest {
    /// Collect `compiled_code` from manifest nodes as a mapping from simplified
    /// unique_id to SQL string.  Nodes without `compiled_code` are omitted.
    ///
    /// This is the manifest-mode counterpart of the file-based
    /// `collect_sql_contents` used in SQL-parse mode.  Users must run
    /// `dbt compile` (or `dbt run`) before invoking dlin so that the manifest
    /// contains compiled SQL.
    pub fn collect_sql_contents(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        let resolver = ManifestGraphResolver::new(self);
        for (orig_id, node) in &self.nodes {
            if let Some(ref code) = node.compiled_code {
                let identity = resolver.resolve(orig_id, &node.resource_type);
                map.insert(identity.graph_id, code.clone());
            }
        }
        map
    }

    /// Collect all unique file paths referenced by nodes and sources.
    /// Returns relative paths as stored in the manifest (e.g. "models/staging/stg_orders.sql").
    pub fn collect_file_paths(&self) -> BTreeSet<String> {
        let mut paths = BTreeSet::new();
        for node in self.nodes.values() {
            let p = node.original_file_path.as_ref().or(node.path.as_ref());
            if let Some(p) = p {
                paths.insert(p.clone());
            }
        }
        for source in self.sources.values() {
            let p = source.original_file_path.as_ref().or(source.path.as_ref());
            if let Some(p) = p {
                paths.insert(p.clone());
            }
        }
        for sm in self.semantic_models.values() {
            let p = sm.original_file_path.as_ref().or(sm.path.as_ref());
            if let Some(p) = p {
                paths.insert(p.clone());
            }
        }
        for metric in self.metrics.values() {
            let p = metric.original_file_path.as_ref().or(metric.path.as_ref());
            if let Some(p) = p {
                paths.insert(p.clone());
            }
        }
        for sq in self.saved_queries.values() {
            let p = sq.original_file_path.as_ref().or(sq.path.as_ref());
            if let Some(p) = p {
                paths.insert(p.clone());
            }
        }
        paths
    }
}

#[cfg(test)]
mod tests;
