use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use petgraph::stable_graph::NodeIndex;
use serde::Deserialize;

use crate::graph::types::*;

/// Metadata section of manifest.json
#[derive(Debug, Default, Deserialize)]
pub struct ManifestMetadata {
    pub project_name: Option<String>,
    /// dbt adapter type (e.g. "postgres", "bigquery", "snowflake") — present in dbt >=1.x manifests.
    pub adapter_type: Option<String>,
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
}

/// A node entry in the manifest (model, seed, snapshot, test, analysis)
#[derive(Debug, Deserialize)]
pub struct ManifestNode {
    pub unique_id: String,
    pub name: String,
    pub resource_type: String,
    #[serde(default)]
    pub depends_on: DependsOn,
    #[serde(default)]
    pub config: ManifestConfig,
    pub description: Option<String>,
    pub path: Option<String>,
    /// Project-root-relative path (e.g. "models/staging/stg_orders.sql").
    /// Present in dbt >=1.x manifests; preferred over `path` for file matching.
    pub original_file_path: Option<String>,
    /// Column definitions keyed by column name
    #[serde(default)]
    pub columns: HashMap<String, ManifestColumn>,
    /// Compiled SQL code (Jinja resolved) — present after `dbt compile` or `dbt run`
    pub compiled_code: Option<String>,
    /// Database name (e.g., "jaffle_shop")
    #[serde(default)]
    pub database: Option<String>,
    /// Schema name (e.g., "main")
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
    /// Project-root-relative path; preferred over `path` for file matching.
    pub original_file_path: Option<String>,
    /// Column definitions keyed by column name
    #[serde(default)]
    pub columns: HashMap<String, ManifestColumn>,
    /// Physical database name (may differ from source_name)
    #[serde(default)]
    pub database: Option<String>,
    /// Physical schema name (may differ from source_name)
    #[serde(default)]
    pub schema: Option<String>,
    /// Physical table identifier (defaults to name when absent)
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
    pub materialized: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Map a manifest resource_type string to our NodeType enum
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

/// Simplify a dbt manifest unique_id (e.g. "model.my_project.stg_orders") to
/// the short form used in this tool's graph (e.g. "model.stg_orders").
/// For sources: "source.my_project.raw.orders" -> "source.raw.orders"
/// For tests:   "test.my_project.test_name.hash" -> "test.test_name"
fn simplify_unique_id(unique_id: &str, resource_type: &str) -> String {
    let parts: Vec<&str> = unique_id.split('.').collect();
    match resource_type {
        "source" => {
            // source.project.source_name.table_name -> source.source_name.table_name
            if parts.len() >= 4 {
                format!("{}.{}.{}", parts[0], parts[2], parts[3])
            } else {
                unique_id.to_string()
            }
        }
        "test" => {
            // test.project.test_name[.hash] -> test.test_name (skip trailing hash)
            if parts.len() >= 3 {
                format!("{}.{}", parts[0], parts[2])
            } else {
                unique_id.to_string()
            }
        }
        _ => {
            // model.project.name → model.name
            // model.project.name.v1 → model.name.v1  (versioned models)
            if parts.len() >= 3 {
                format!("{}.{}", parts[0], parts[2..].join("."))
            } else {
                unique_id.to_string()
            }
        }
    }
}

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
    let manifest: Manifest = serde_json::from_slice(content).map_err(|e| {
        crate::error::DbtLineageError::ArtifactParseError {
            path: manifest_path.to_path_buf(),
            source: e,
        }
    })?;

    Ok(manifest)
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
        for (orig_id, node) in &self.nodes {
            if let Some(ref code) = node.compiled_code {
                let simple_id = simplify_unique_id(orig_id, &node.resource_type);
                map.insert(simple_id, code.clone());
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
        paths
    }
}

/// Build a LineageGraph from a parsed manifest.json file.
pub fn build_graph_from_manifest(manifest_path: &Path) -> Result<LineageGraph> {
    let manifest = load_manifest(manifest_path)?;
    build_graph_from_parsed_manifest(&manifest)
}

/// Build a LineageGraph from an already-parsed Manifest struct.
/// This is separated for testability and reuse by the diff feature.
pub fn build_graph_from_parsed_manifest(manifest: &Manifest) -> Result<LineageGraph> {
    let mut graph = LineageGraph::new();
    // Map from original manifest unique_id to graph NodeIndex
    let mut node_map: HashMap<String, NodeIndex> = HashMap::new();

    // 1. Add source nodes
    add_source_nodes(&mut graph, &mut node_map, &manifest.sources);

    // 2. Add regular nodes (models, seeds, snapshots, tests, analyses)
    add_regular_nodes(&mut graph, &mut node_map, &manifest.nodes);

    // 3. Add exposure nodes
    add_exposure_nodes(&mut graph, &mut node_map, &manifest.exposures);

    // 4. Add semantic layer nodes
    add_semantic_model_nodes(&mut graph, &mut node_map, &manifest.semantic_models);
    add_metric_nodes(&mut graph, &mut node_map, &manifest.metrics);
    add_saved_query_nodes(&mut graph, &mut node_map, &manifest.saved_queries);

    // 5. Add edges from depends_on for regular nodes
    add_node_edges(&mut graph, &node_map, &manifest.nodes);

    // 6. Add edges from depends_on for exposures
    add_exposure_edges(&mut graph, &node_map, &manifest.exposures);

    // 7. Add edges from depends_on for semantic layer nodes
    add_depends_on_edges(
        &mut graph,
        &node_map,
        &manifest.semantic_models,
        EdgeType::Ref,
    );
    add_depends_on_edges(&mut graph, &node_map, &manifest.metrics, EdgeType::Ref);
    add_depends_on_edges(
        &mut graph,
        &node_map,
        &manifest.saved_queries,
        EdgeType::Ref,
    );

    Ok(graph)
}

fn add_source_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    sources: &HashMap<String, ManifestSource>,
) {
    for (orig_id, source) in sources {
        let simple_id = simplify_unique_id(orig_id, "source");
        let label = format!("{}.{}", source.source_name, source.name);

        let idx = graph.add_node(NodeData {
            unique_id: simple_id.clone(),
            label,
            node_type: NodeType::Source,
            file_path: source
                .original_file_path
                .as_ref()
                .or(source.path.as_ref())
                .map(|p| p.into()),
            description: non_empty_string(&source.description),
            materialization: None,
            tags: vec![],
            columns: {
                let mut cols: Vec<String> = source.columns.keys().cloned().collect();
                cols.sort();
                cols
            },
            exposure: None,
            aliases: vec![],
        });
        node_map.insert(orig_id.clone(), idx);
        // Also index by simplified id for edge resolution
        node_map.insert(simple_id, idx);
    }
}

fn add_regular_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    nodes: &HashMap<String, ManifestNode>,
) {
    for (orig_id, node) in nodes {
        let node_type = resource_type_to_node_type(&node.resource_type);
        let simple_id = simplify_unique_id(orig_id, &node.resource_type);

        let idx = graph.add_node(NodeData {
            unique_id: simple_id.clone(),
            label: node.name.clone(),
            node_type,
            file_path: node
                .original_file_path
                .as_ref()
                .or(node.path.as_ref())
                .map(|p| p.into()),
            description: non_empty_string(&node.description),
            materialization: node.config.materialized.clone(),
            tags: node.config.tags.clone(),
            columns: {
                let mut cols: Vec<String> = node.columns.keys().cloned().collect();
                cols.sort();
                cols
            },
            exposure: None,
            aliases: vec![],
        });
        node_map.insert(orig_id.clone(), idx);
        node_map.insert(simple_id, idx);
    }
}

fn add_exposure_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    exposures: &HashMap<String, ManifestExposure>,
) {
    for (orig_id, exposure) in exposures {
        let simple_id = simplify_unique_id(orig_id, "exposure");

        let idx = graph.add_node(NodeData {
            unique_id: simple_id.clone(),
            label: exposure.name.clone(),
            node_type: NodeType::Exposure,
            file_path: None,
            description: non_empty_string(&exposure.description),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: Some(ExposureInfo {
                label: non_empty_string(&exposure.label),
                exposure_type: non_empty_string(&exposure.exposure_type),
                url: non_empty_string(&exposure.url),
                maturity: non_empty_string(&exposure.maturity),
                owner: exposure.owner.as_ref().map(|o| OwnerInfo {
                    name: non_empty_string(&o.name),
                    email: non_empty_string(&o.email),
                }),
            }),
            aliases: vec![],
        });
        node_map.insert(orig_id.clone(), idx);
        node_map.insert(simple_id, idx);
    }
}

fn add_node_edges(
    graph: &mut LineageGraph,
    node_map: &HashMap<String, NodeIndex>,
    nodes: &HashMap<String, ManifestNode>,
) {
    for (orig_id, node) in nodes {
        let current_idx = match node_map.get(orig_id) {
            Some(&idx) => idx,
            None => continue,
        };

        // Use EdgeType::Test when the target node is a test, regardless of
        // the dependency's type prefix, so all test relationships are consistent.
        let current_is_test = graph[current_idx].node_type == NodeType::Test;

        for dep_id in &node.depends_on.nodes {
            if let Some(&dep_idx) = node_map.get(dep_id) {
                let edge_type = if current_is_test {
                    EdgeType::Test
                } else {
                    infer_edge_type(dep_id)
                };
                graph.add_edge(dep_idx, current_idx, EdgeData::direct(edge_type));
            }
        }
    }
}

fn add_exposure_edges(
    graph: &mut LineageGraph,
    node_map: &HashMap<String, NodeIndex>,
    exposures: &HashMap<String, ManifestExposure>,
) {
    for (orig_id, exposure) in exposures {
        let current_idx = match node_map.get(orig_id) {
            Some(&idx) => idx,
            None => continue,
        };

        for dep_id in &exposure.depends_on.nodes {
            if let Some(&dep_idx) = node_map.get(dep_id) {
                graph.add_edge(dep_idx, current_idx, EdgeData::direct(EdgeType::Exposure));
            }
        }
    }
}

trait HasSemanticLayerFields {
    fn name(&self) -> &str;
    fn depends_on_nodes(&self) -> &[String];
    fn description(&self) -> Option<&str>;
    fn original_file_path(&self) -> Option<&str>;
    fn path(&self) -> Option<&str>;
    fn node_type(&self) -> NodeType;
}

impl HasSemanticLayerFields for ManifestSemanticModel {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends_on_nodes(&self) -> &[String] {
        &self.depends_on.nodes
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn original_file_path(&self) -> Option<&str> {
        self.original_file_path.as_deref()
    }
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
    fn node_type(&self) -> NodeType {
        NodeType::SemanticModel
    }
}

impl HasSemanticLayerFields for ManifestMetric {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends_on_nodes(&self) -> &[String] {
        &self.depends_on.nodes
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn original_file_path(&self) -> Option<&str> {
        self.original_file_path.as_deref()
    }
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
    fn node_type(&self) -> NodeType {
        NodeType::Metric
    }
}

impl HasSemanticLayerFields for ManifestSavedQuery {
    fn name(&self) -> &str {
        &self.name
    }
    fn depends_on_nodes(&self) -> &[String] {
        &self.depends_on.nodes
    }
    fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }
    fn original_file_path(&self) -> Option<&str> {
        self.original_file_path.as_deref()
    }
    fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }
    fn node_type(&self) -> NodeType {
        NodeType::SavedQuery
    }
}

fn add_semantic_layer_nodes<T: HasSemanticLayerFields>(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    items: &HashMap<String, T>,
) {
    for (orig_id, item) in items {
        let resource_type = item.node_type().label();
        let simple_id = simplify_unique_id(orig_id, resource_type);
        let idx = graph.add_node(NodeData {
            unique_id: simple_id.clone(),
            label: item.name().to_string(),
            node_type: item.node_type(),
            file_path: item
                .original_file_path()
                .or_else(|| item.path())
                .map(|p| p.into()),
            description: item
                .description()
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
        node_map.insert(orig_id.clone(), idx);
        node_map.insert(simple_id, idx);
    }
}

fn add_semantic_model_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    items: &HashMap<String, ManifestSemanticModel>,
) {
    add_semantic_layer_nodes(graph, node_map, items);
}

fn add_metric_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    items: &HashMap<String, ManifestMetric>,
) {
    add_semantic_layer_nodes(graph, node_map, items);
}

fn add_saved_query_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    items: &HashMap<String, ManifestSavedQuery>,
) {
    add_semantic_layer_nodes(graph, node_map, items);
}

fn add_depends_on_edges<T: HasSemanticLayerFields>(
    graph: &mut LineageGraph,
    node_map: &HashMap<String, NodeIndex>,
    items: &HashMap<String, T>,
    edge_type: EdgeType,
) {
    for (orig_id, item) in items {
        let Some(&current_idx) = node_map.get(orig_id) else {
            continue;
        };
        for dep_id in item.depends_on_nodes() {
            if let Some(&dep_idx) = node_map.get(dep_id) {
                graph.add_edge(dep_idx, current_idx, EdgeData::direct(edge_type));
            }
        }
    }
}

/// Infer the edge type from a dependency unique_id
fn infer_edge_type(dep_unique_id: &str) -> EdgeType {
    if dep_unique_id.starts_with("source.") {
        EdgeType::Source
    } else if dep_unique_id.starts_with("test.") {
        EdgeType::Test
    } else {
        EdgeType::Ref
    }
}

/// Return None for empty or whitespace-only strings
fn non_empty_string(s: &Option<String>) -> Option<String> {
    s.as_ref().filter(|v| !v.trim().is_empty()).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_resource_type_to_node_type() {
        assert_eq!(resource_type_to_node_type("model"), NodeType::Model);
        assert_eq!(resource_type_to_node_type("source"), NodeType::Source);
        assert_eq!(resource_type_to_node_type("seed"), NodeType::Seed);
        assert_eq!(resource_type_to_node_type("snapshot"), NodeType::Snapshot);
        assert_eq!(resource_type_to_node_type("test"), NodeType::Test);
        assert_eq!(resource_type_to_node_type("analysis"), NodeType::Model);
        assert_eq!(resource_type_to_node_type("exposure"), NodeType::Exposure);
        assert_eq!(resource_type_to_node_type("unknown"), NodeType::Model);
    }

    #[test]
    fn test_simplify_unique_id_model() {
        assert_eq!(
            simplify_unique_id("model.my_project.stg_orders", "model"),
            "model.stg_orders"
        );
    }

    #[test]
    fn test_simplify_unique_id_source() {
        assert_eq!(
            simplify_unique_id("source.my_project.raw.orders", "source"),
            "source.raw.orders"
        );
    }

    #[test]
    fn test_simplify_unique_id_short() {
        assert_eq!(
            simplify_unique_id("model.stg_orders", "model"),
            "model.stg_orders"
        );
    }

    #[test]
    fn test_simplify_unique_id_source_short() {
        assert_eq!(
            simplify_unique_id("source.raw.orders", "source"),
            "source.raw.orders"
        );
    }

    #[test]
    fn test_simplify_unique_id_test() {
        // test.project.test_name.hash -> test.test_name
        assert_eq!(
            simplify_unique_id(
                "test.jaffle_shop.not_null_orders_order_id.cf6c17daed",
                "test"
            ),
            "test.not_null_orders_order_id"
        );
    }

    #[test]
    fn test_simplify_unique_id_test_short() {
        assert_eq!(
            simplify_unique_id("test.not_null_orders_order_id", "test"),
            "test.not_null_orders_order_id"
        );
    }

    #[test]
    fn test_simplify_unique_id_versioned_model() {
        // dbt versioned model unique_ids: model.project.name.v{N} → model.name.v{N}
        assert_eq!(
            simplify_unique_id("model.my_project.my_model.v1", "model"),
            "model.my_model.v1"
        );
        assert_eq!(
            simplify_unique_id("model.my_project.my_model.v2", "model"),
            "model.my_model.v2"
        );
        // Unversioned model must still work
        assert_eq!(
            simplify_unique_id("model.my_project.stg_orders", "model"),
            "model.stg_orders"
        );
    }

    #[test]
    fn test_infer_edge_type() {
        assert_eq!(
            infer_edge_type("source.my_project.raw.orders"),
            EdgeType::Source
        );
        assert_eq!(
            infer_edge_type("model.my_project.stg_orders"),
            EdgeType::Ref
        );
        assert_eq!(infer_edge_type("test.my_project.some_test"), EdgeType::Test);
        assert_eq!(infer_edge_type("seed.my_project.countries"), EdgeType::Ref);
    }

    #[test]
    fn test_non_empty_string() {
        assert_eq!(non_empty_string(&None), None);
        assert_eq!(non_empty_string(&Some("".to_string())), None);
        assert_eq!(non_empty_string(&Some("  ".to_string())), None);
        assert_eq!(
            non_empty_string(&Some("hello".to_string())),
            Some("hello".to_string())
        );
    }

    #[test]
    fn test_build_graph_from_minimal_manifest() {
        let manifest = Manifest {
            nodes: HashMap::from([(
                "model.proj.stg_orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.stg_orders".to_string(),
                    name: "stg_orders".to_string(),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["source.proj.raw.orders".to_string()],
                    },
                    config: ManifestConfig {
                        materialized: Some("view".to_string()),
                        tags: vec!["staging".to_string()],
                    },
                    description: Some("Staged orders".to_string()),
                    path: Some("models/staging/stg_orders.sql".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::from([(
                "source.proj.raw.orders".to_string(),
                ManifestSource {
                    unique_id: "source.proj.raw.orders".to_string(),
                    name: "orders".to_string(),
                    source_name: "raw".to_string(),
                    resource_type: "source".to_string(),
                    description: Some("Raw orders table".to_string()),
                    path: Some("models/staging/schema.yml".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    database: None,
                    schema: None,
                    identifier: None,
                },
            )]),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        // Find the model node
        let model = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Model)
            .expect("Should have a model node");
        assert_eq!(graph[model].label, "stg_orders");
        assert_eq!(graph[model].unique_id, "model.stg_orders");
        assert_eq!(graph[model].materialization.as_deref(), Some("view"));
        assert_eq!(graph[model].tags, vec!["staging"]);
        assert_eq!(graph[model].description.as_deref(), Some("Staged orders"));

        // Find the source node
        let source = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Source)
            .expect("Should have a source node");
        assert_eq!(graph[source].label, "raw.orders");
        assert_eq!(graph[source].unique_id, "source.raw.orders");
    }

    #[test]
    fn test_build_graph_with_exposures() {
        let manifest = Manifest {
            nodes: HashMap::from([(
                "model.proj.orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.orders".to_string(),
                    name: "orders".to_string(),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::new(),
            exposures: HashMap::from([(
                "exposure.proj.weekly_report".to_string(),
                ManifestExposure {
                    unique_id: "exposure.proj.weekly_report".to_string(),
                    name: "weekly_report".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["model.proj.orders".to_string()],
                    },
                    description: Some("Weekly dashboard".to_string()),
                    label: None,
                    exposure_type: None,
                    url: None,
                    maturity: None,
                    owner: None,
                },
            )]),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        let exposure = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Exposure)
            .expect("Should have an exposure node");
        assert_eq!(graph[exposure].label, "weekly_report");
        assert_eq!(
            graph[exposure].description.as_deref(),
            Some("Weekly dashboard")
        );
    }

    #[test]
    fn test_exposure_metadata_parsed() {
        let manifest = Manifest {
            nodes: HashMap::new(),
            sources: HashMap::new(),
            exposures: HashMap::from([(
                "exposure.proj.dashboard".to_string(),
                ManifestExposure {
                    unique_id: "exposure.proj.dashboard".to_string(),
                    name: "dashboard".to_string(),
                    depends_on: DependsOn { nodes: vec![] },
                    description: Some("Main dashboard".to_string()),
                    label: Some("Main Dashboard".to_string()),
                    exposure_type: Some("dashboard".to_string()),
                    url: Some("https://bi.example.com".to_string()),
                    maturity: Some("high".to_string()),
                    owner: Some(ManifestExposureOwner {
                        name: Some("Data Team".to_string()),
                        email: Some("data@example.com".to_string()),
                    }),
                },
            )]),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        let exp_idx = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Exposure)
            .expect("Should have an exposure node");
        let exp = &graph[exp_idx];

        let info = exp.exposure.as_ref().expect("Should have exposure info");
        assert_eq!(info.label.as_deref(), Some("Main Dashboard"));
        assert_eq!(info.exposure_type.as_deref(), Some("dashboard"));
        assert_eq!(info.url.as_deref(), Some("https://bi.example.com"));
        assert_eq!(info.maturity.as_deref(), Some("high"));

        let owner = info.owner.as_ref().expect("Should have owner");
        assert_eq!(owner.name.as_deref(), Some("Data Team"));
        assert_eq!(owner.email.as_deref(), Some("data@example.com"));
    }

    #[test]
    fn test_exposure_metadata_from_fixture() {
        let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/simple_project/target/manifest.json");
        let graph = build_graph_from_manifest(&manifest_path).unwrap();

        let exp_idx = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Exposure)
            .expect("Should have an exposure node from fixture");
        let exp = &graph[exp_idx];
        assert_eq!(exp.label, "weekly_report");

        let info = exp.exposure.as_ref().expect("Should have exposure info");
        assert_eq!(info.label.as_deref(), Some("Weekly Report"));
        assert_eq!(info.exposure_type.as_deref(), Some("dashboard"));
        assert_eq!(info.url.as_deref(), Some("https://bi.example.com/weekly"));
        assert_eq!(info.maturity.as_deref(), Some("high"));

        let owner = info.owner.as_ref().expect("Should have owner");
        assert_eq!(owner.name.as_deref(), Some("Data Team"));
        assert_eq!(owner.email.as_deref(), Some("data@example.com"));
    }

    #[test]
    fn test_build_graph_with_seeds_and_snapshots() {
        let manifest = Manifest {
            nodes: HashMap::from([
                (
                    "seed.proj.countries".to_string(),
                    ManifestNode {
                        unique_id: "seed.proj.countries".to_string(),
                        name: "countries".to_string(),
                        resource_type: "seed".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: Some("seeds/countries.csv".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "snapshot.proj.snap_orders".to_string(),
                    ManifestNode {
                        unique_id: "snapshot.proj.snap_orders".to_string(),
                        name: "snap_orders".to_string(),
                        resource_type: "snapshot".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig {
                            materialized: Some("snapshot".to_string()),
                            tags: vec![],
                        },
                        description: None,
                        path: Some("snapshots/snap_orders.sql".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
            ]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        assert_eq!(graph.node_count(), 2);

        let seed = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Seed)
            .expect("Should have a seed node");
        assert_eq!(graph[seed].label, "countries");

        let snap = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Snapshot)
            .expect("Should have a snapshot node");
        assert_eq!(graph[snap].label, "snap_orders");
    }

    #[test]
    fn test_build_graph_with_tests() {
        let manifest = Manifest {
            nodes: HashMap::from([
                (
                    "model.proj.orders".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.orders".to_string(),
                        name: "orders".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "test.proj.assert_positive".to_string(),
                    ManifestNode {
                        unique_id: "test.proj.assert_positive".to_string(),
                        name: "assert_positive".to_string(),
                        resource_type: "test".to_string(),
                        depends_on: DependsOn {
                            nodes: vec!["model.proj.orders".to_string()],
                        },
                        config: ManifestConfig::default(),
                        description: None,
                        path: Some("tests/assert_positive.sql".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
            ]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);

        let test_node = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Test)
            .expect("Should have a test node");
        assert_eq!(graph[test_node].label, "assert_positive");

        // Edge to test node should use EdgeType::Test, not EdgeType::Ref
        use petgraph::visit::IntoEdgeReferences;
        let edge = graph.edge_references().next().unwrap();
        assert_eq!(edge.weight().edge_type, EdgeType::Test);
    }

    #[test]
    fn test_build_graph_empty_manifest() {
        let manifest = Manifest {
            nodes: HashMap::new(),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_build_graph_missing_dependency() {
        // A node depends on something not in the manifest -- edge is skipped gracefully
        let manifest = Manifest {
            nodes: HashMap::from([(
                "model.proj.orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.orders".to_string(),
                    name: "orders".to_string(),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["model.proj.nonexistent".to_string()],
                    },
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.edge_count(), 0); // Edge to nonexistent node is skipped
    }

    #[test]
    fn test_build_graph_optional_fields() {
        let manifest = Manifest {
            nodes: HashMap::from([(
                "model.proj.bare".to_string(),
                ManifestNode {
                    unique_id: "model.proj.bare".to_string(),
                    name: "bare".to_string(),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig {
                        materialized: None,
                        tags: vec![],
                    },
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        let node = &graph[graph.node_indices().next().unwrap()];
        assert!(node.description.is_none());
        assert!(node.materialization.is_none());
        assert!(node.tags.is_empty());
        assert!(node.file_path.is_none());
    }

    #[test]
    fn test_build_graph_from_manifest_file() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.json");

        let manifest_json = r#"{
            "nodes": {
                "model.proj.stg_orders": {
                    "unique_id": "model.proj.stg_orders",
                    "name": "stg_orders",
                    "resource_type": "model",
                    "depends_on": { "nodes": ["source.proj.raw.orders"] },
                    "config": { "materialized": "view", "tags": [] },
                    "description": "Staged orders",
                    "path": "models/staging/stg_orders.sql"
                }
            },
            "sources": {
                "source.proj.raw.orders": {
                    "unique_id": "source.proj.raw.orders",
                    "name": "orders",
                    "source_name": "raw",
                    "resource_type": "source",
                    "description": "Raw orders",
                    "path": "models/staging/schema.yml"
                }
            },
            "exposures": {}
        }"#;

        fs::write(&manifest_path, manifest_json).unwrap();

        let graph = build_graph_from_manifest(&manifest_path).unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_build_graph_from_manifest_file_not_found() {
        let result = build_graph_from_manifest(Path::new("/nonexistent/manifest.json"));
        assert!(result.is_err());
    }

    #[test]
    fn test_build_graph_from_manifest_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest_path = tmp.path().join("manifest.json");
        fs::write(&manifest_path, "not valid json").unwrap();

        let result = build_graph_from_manifest(&manifest_path);
        assert!(result.is_err());
    }

    #[test]
    fn test_original_file_path_preferred_over_path() {
        // dbt >= 1.x sets path to the models-dir-relative path (e.g. "staging/stg_orders.sql")
        // and original_file_path to the project-root-relative path ("models/staging/stg_orders.sql").
        // resolve_sql_to_label strips the project root and compares against file_path, so
        // original_file_path must win when both are present.
        let manifest = Manifest {
            nodes: HashMap::from([(
                "model.proj.stg_orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.stg_orders".to_string(),
                    name: "stg_orders".to_string(),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: Some("staging/stg_orders.sql".to_string()),
                    original_file_path: Some("models/staging/stg_orders.sql".to_string()),
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        let node = &graph[graph.node_indices().next().unwrap()];
        assert_eq!(
            node.file_path.as_ref().map(|p| p.to_str().unwrap()),
            Some("models/staging/stg_orders.sql")
        );
    }

    #[test]
    fn test_build_graph_analysis_maps_to_model() {
        let manifest = Manifest {
            nodes: HashMap::from([(
                "analysis.proj.my_analysis".to_string(),
                ManifestNode {
                    unique_id: "analysis.proj.my_analysis".to_string(),
                    name: "my_analysis".to_string(),
                    resource_type: "analysis".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        let node = &graph[graph.node_indices().next().unwrap()];
        assert_eq!(node.node_type, NodeType::Model);
    }

    #[test]
    fn test_build_graph_complex_chain() {
        // source -> stg_orders -> orders (with multiple deps)
        let manifest = Manifest {
            nodes: HashMap::from([
                (
                    "model.proj.stg_orders".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.stg_orders".to_string(),
                        name: "stg_orders".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn {
                            nodes: vec!["source.proj.raw.orders".to_string()],
                        },
                        config: ManifestConfig {
                            materialized: Some("view".to_string()),
                            tags: vec![],
                        },
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "model.proj.stg_payments".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.stg_payments".to_string(),
                        name: "stg_payments".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn {
                            nodes: vec!["source.proj.raw.payments".to_string()],
                        },
                        config: ManifestConfig::default(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "model.proj.orders".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.orders".to_string(),
                        name: "orders".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn {
                            nodes: vec![
                                "model.proj.stg_orders".to_string(),
                                "model.proj.stg_payments".to_string(),
                            ],
                        },
                        config: ManifestConfig {
                            materialized: Some("table".to_string()),
                            tags: vec!["marts".to_string()],
                        },
                        description: Some("Order fact table".to_string()),
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
            ]),
            sources: HashMap::from([
                (
                    "source.proj.raw.orders".to_string(),
                    ManifestSource {
                        unique_id: "source.proj.raw.orders".to_string(),
                        name: "orders".to_string(),
                        source_name: "raw".to_string(),
                        resource_type: "source".to_string(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        database: None,
                        schema: None,
                        identifier: None,
                    },
                ),
                (
                    "source.proj.raw.payments".to_string(),
                    ManifestSource {
                        unique_id: "source.proj.raw.payments".to_string(),
                        name: "payments".to_string(),
                        source_name: "raw".to_string(),
                        resource_type: "source".to_string(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        database: None,
                        schema: None,
                        identifier: None,
                    },
                ),
            ]),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();
        // 2 sources + 3 models = 5 nodes
        assert_eq!(graph.node_count(), 5);
        // source.raw.orders -> stg_orders, source.raw.payments -> stg_payments,
        // stg_orders -> orders, stg_payments -> orders = 4 edges
        assert_eq!(graph.edge_count(), 4);
    }

    #[test]
    fn test_build_graph_from_fixture_manifest() {
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/simple_project/target/manifest.json");

        if !fixture_path.exists() {
            // Skip if fixture not yet created
            return;
        }

        let graph = build_graph_from_manifest(&fixture_path).unwrap();

        // The fixture has: 3 sources, 3 staging models, 2 mart models, 1 seed, 1 test, 1 exposure
        // = 11 nodes total
        assert!(
            graph.node_count() >= 10,
            "Expected at least 10 nodes, got {}",
            graph.node_count()
        );

        // Check we have all node types present
        let has_source = graph
            .node_indices()
            .any(|i| graph[i].node_type == NodeType::Source);
        let has_model = graph
            .node_indices()
            .any(|i| graph[i].node_type == NodeType::Model);
        let has_seed = graph
            .node_indices()
            .any(|i| graph[i].node_type == NodeType::Seed);
        let has_test = graph
            .node_indices()
            .any(|i| graph[i].node_type == NodeType::Test);
        let has_exposure = graph
            .node_indices()
            .any(|i| graph[i].node_type == NodeType::Exposure);

        assert!(has_source, "Should have source nodes");
        assert!(has_model, "Should have model nodes");
        assert!(has_seed, "Should have seed nodes");
        assert!(has_test, "Should have test nodes");
        assert!(has_exposure, "Should have exposure nodes");

        // Check edges exist
        assert!(graph.edge_count() > 0, "Should have edges");
    }

    #[test]
    fn test_collect_file_paths() {
        let manifest = Manifest {
            nodes: HashMap::from([
                (
                    "model.proj.stg_orders".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.stg_orders".to_string(),
                        name: "stg_orders".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: Some("models/staging/stg_orders.sql".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "model.proj.orders".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.orders".to_string(),
                        name: "orders".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: Some("models/marts/orders.sql".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "model.proj.bare".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.bare".to_string(),
                        name: "bare".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
            ]),
            sources: HashMap::from([(
                "source.proj.raw.orders".to_string(),
                ManifestSource {
                    unique_id: "source.proj.raw.orders".to_string(),
                    name: "orders".to_string(),
                    source_name: "raw".to_string(),
                    resource_type: "source".to_string(),
                    description: None,
                    path: Some("models/staging/schema.yml".to_string()),
                    original_file_path: None,
                    columns: HashMap::new(),
                    database: None,
                    schema: None,
                    identifier: None,
                },
            )]),
            ..Default::default()
        };

        let paths = manifest.collect_file_paths();
        assert_eq!(paths.len(), 3);
        assert!(paths.contains("models/staging/stg_orders.sql"));
        assert!(paths.contains("models/marts/orders.sql"));
        assert!(paths.contains("models/staging/schema.yml"));
        // bare has no path, should not appear
        assert!(!paths.iter().any(|p| p.contains("bare")));
    }

    #[test]
    fn test_collect_file_paths_deduplicates() {
        // Multiple sources can reference the same YAML file
        let manifest = Manifest {
            nodes: HashMap::new(),
            sources: HashMap::from([
                (
                    "source.proj.raw.orders".to_string(),
                    ManifestSource {
                        unique_id: "source.proj.raw.orders".to_string(),
                        name: "orders".to_string(),
                        source_name: "raw".to_string(),
                        resource_type: "source".to_string(),
                        description: None,
                        path: Some("models/staging/schema.yml".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        database: None,
                        schema: None,
                        identifier: None,
                    },
                ),
                (
                    "source.proj.raw.customers".to_string(),
                    ManifestSource {
                        unique_id: "source.proj.raw.customers".to_string(),
                        name: "customers".to_string(),
                        source_name: "raw".to_string(),
                        resource_type: "source".to_string(),
                        description: None,
                        path: Some("models/staging/schema.yml".to_string()),
                        original_file_path: None,
                        columns: HashMap::new(),
                        database: None,
                        schema: None,
                        identifier: None,
                    },
                ),
            ]),
            ..Default::default()
        };

        let paths = manifest.collect_file_paths();
        assert_eq!(paths.len(), 1, "Duplicate paths should be deduplicated");
    }

    #[test]
    fn test_load_manifest() {
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/simple_project/target/manifest.json");

        let manifest = load_manifest(&fixture_path).unwrap();
        assert!(!manifest.nodes.is_empty());
        assert!(!manifest.sources.is_empty());

        let paths = manifest.collect_file_paths();
        assert!(paths.contains("models/staging/stg_orders.sql"));
        assert!(paths.contains("models/staging/schema.yml"));
    }

    #[test]
    fn test_collect_sql_contents_from_manifest() {
        let manifest = Manifest {
            nodes: HashMap::from([
                (
                    "model.proj.stg_orders".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.stg_orders".to_string(),
                        name: "stg_orders".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: Some("select * from raw.orders".to_string()),
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "test.proj.not_null_orders_id.abc123".to_string(),
                    ManifestNode {
                        unique_id: "test.proj.not_null_orders_id.abc123".to_string(),
                        name: "not_null_orders_id".to_string(),
                        resource_type: "test".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: Some(
                            "select count(*) from orders where id is null".to_string(),
                        ),
                        database: None,
                        schema: None,
                    },
                ),
                (
                    "model.proj.no_compile".to_string(),
                    ManifestNode {
                        unique_id: "model.proj.no_compile".to_string(),
                        name: "no_compile".to_string(),
                        resource_type: "model".to_string(),
                        depends_on: DependsOn::default(),
                        config: ManifestConfig::default(),
                        description: None,
                        path: None,
                        original_file_path: None,
                        columns: HashMap::new(),
                        compiled_code: None,
                        database: None,
                        schema: None,
                    },
                ),
            ]),
            sources: HashMap::new(),
            ..Default::default()
        };

        let sql_contents = manifest.collect_sql_contents();

        // compiled_code present → included
        assert_eq!(
            sql_contents.get("model.stg_orders").map(|s| s.as_str()),
            Some("select * from raw.orders")
        );
        // test unique_id is simplified (test.proj.name.hash → test.name)
        assert_eq!(
            sql_contents
                .get("test.not_null_orders_id")
                .map(|s| s.as_str()),
            Some("select count(*) from orders where id is null")
        );
        // compiled_code absent → omitted
        assert!(!sql_contents.contains_key("model.no_compile"));
    }

    #[test]
    fn test_collect_sql_contents_from_fixture() {
        let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/simple_project/target/manifest.json");

        let manifest = load_manifest(&fixture_path).unwrap();
        let sql_contents = manifest.collect_sql_contents();

        // The fixture has compiled_code for stg_orders and the test node
        assert!(
            sql_contents.contains_key("model.stg_orders"),
            "stg_orders should have compiled_code"
        );
        assert!(
            sql_contents.contains_key("test.assert_orders_positive_amount"),
            "test node should have compiled_code"
        );
        // Nodes without compiled_code should not appear
        assert!(
            !sql_contents.contains_key("model.customers"),
            "customers has no compiled_code in fixture"
        );
    }

    #[test]
    fn test_build_graph_with_semantic_layer_nodes() {
        let manifest = Manifest {
            nodes: HashMap::from([(
                "model.proj.orders".to_string(),
                ManifestNode {
                    unique_id: "model.proj.orders".to_string(),
                    name: "orders".to_string(),
                    resource_type: "model".to_string(),
                    depends_on: DependsOn::default(),
                    config: ManifestConfig::default(),
                    description: None,
                    path: None,
                    original_file_path: None,
                    columns: HashMap::new(),
                    compiled_code: None,
                    database: None,
                    schema: None,
                },
            )]),
            sources: HashMap::new(),
            semantic_models: HashMap::from([(
                "semantic_model.proj.orders".to_string(),
                ManifestSemanticModel {
                    unique_id: "semantic_model.proj.orders".to_string(),
                    name: "orders".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["model.proj.orders".to_string()],
                    },
                    description: Some("Orders semantic model".to_string()),
                    path: None,
                    original_file_path: None,
                },
            )]),
            metrics: HashMap::from([(
                "metric.proj.order_count".to_string(),
                ManifestMetric {
                    unique_id: "metric.proj.order_count".to_string(),
                    name: "order_count".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["semantic_model.proj.orders".to_string()],
                    },
                    description: None,
                    path: None,
                    original_file_path: None,
                },
            )]),
            saved_queries: HashMap::from([(
                "saved_query.proj.order_metrics".to_string(),
                ManifestSavedQuery {
                    unique_id: "saved_query.proj.order_metrics".to_string(),
                    name: "order_metrics".to_string(),
                    depends_on: DependsOn {
                        nodes: vec!["metric.proj.order_count".to_string()],
                    },
                    description: None,
                    path: None,
                    original_file_path: None,
                },
            )]),
            ..Default::default()
        };

        let graph = build_graph_from_parsed_manifest(&manifest).unwrap();

        // 4 nodes: model + semantic_model + metric + saved_query
        assert_eq!(graph.node_count(), 4);
        // 3 edges: model->sem, sem->metric, metric->saved_query
        assert_eq!(graph.edge_count(), 3);

        let sem = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::SemanticModel)
            .expect("Should have a semantic_model node");
        assert_eq!(graph[sem].unique_id, "semantic_model.orders");
        assert_eq!(graph[sem].label, "orders");
        assert_eq!(
            graph[sem].description.as_deref(),
            Some("Orders semantic model")
        );

        let metric = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Metric)
            .expect("Should have a metric node");
        assert_eq!(graph[metric].unique_id, "metric.order_count");

        let sq = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::SavedQuery)
            .expect("Should have a saved_query node");
        assert_eq!(graph[sq].unique_id, "saved_query.order_metrics");
    }

    #[test]
    fn test_semantic_layer_nodes_from_jaffle_shop_manifest() {
        let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../refs/jaffle-shop/target/manifest.json");
        if !manifest_path.exists() {
            return; // Skip if ref fixture not available
        }
        let graph = build_graph_from_manifest(&manifest_path).unwrap();

        let sem_models: Vec<_> = graph
            .node_indices()
            .filter(|&i| graph[i].node_type == NodeType::SemanticModel)
            .collect();
        assert!(!sem_models.is_empty(), "Should have semantic_model nodes");

        let metrics: Vec<_> = graph
            .node_indices()
            .filter(|&i| graph[i].node_type == NodeType::Metric)
            .collect();
        assert!(!metrics.is_empty(), "Should have metric nodes");

        let saved_queries: Vec<_> = graph
            .node_indices()
            .filter(|&i| graph[i].node_type == NodeType::SavedQuery)
            .collect();
        assert!(!saved_queries.is_empty(), "Should have saved_query nodes");

        // Each semantic_model should have at least one upstream edge (to a model)
        let sem_idx = sem_models[0];
        let has_upstream = graph
            .edges_directed(sem_idx, petgraph::Direction::Incoming)
            .next()
            .is_some();
        assert!(
            has_upstream,
            "semantic_model should have upstream model edge"
        );
    }
}
