use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use petgraph::stable_graph::NodeIndex;

use crate::graph::types::*;

use super::{
    Manifest, ManifestExposure, ManifestGraphIdentity, ManifestMetric, ManifestNode,
    ManifestResourceType, ManifestSavedQuery, ManifestSemanticModel, ManifestSource,
    classify_resource_type, load_manifest,
};

/// A manifest graph together with diagnostics observed while loading the
/// artifact. Keeping these values together prevents callers from reparsing a
/// manifest merely to display forward-compatibility warnings.
#[derive(Debug)]
pub struct ManifestGraphReport {
    pub graph: LineageGraph,
    pub diagnostics: Vec<super::ManifestDiagnostic>,
    pub manifest: Manifest,
}

/// Build a LineageGraph from a parsed manifest.json file.
pub fn build_graph_from_manifest(manifest_path: &Path) -> Result<LineageGraph> {
    let manifest = load_manifest(manifest_path)?;
    build_graph_from_parsed_manifest(&manifest)
}

/// Load a manifest once and build its graph while retaining load diagnostics.
pub fn build_graph_from_manifest_report(manifest_path: &Path) -> Result<ManifestGraphReport> {
    let report = super::load_manifest_report(manifest_path)?;
    build_graph_from_load_report(report)
}

/// Build a graph from an already loaded report without reparsing it.
pub fn build_graph_from_load_report(
    report: super::ManifestLoadReport,
) -> Result<ManifestGraphReport> {
    let diagnostics = report.diagnostics;
    let manifest = report.manifest.ok_or_else(|| {
        let message = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == super::ManifestDiagnosticSeverity::Error)
            .map(|diagnostic| diagnostic.message.clone())
            .unwrap_or_else(|| "manifest could not be loaded".to_string());
        anyhow::anyhow!(message)
    })?;
    let graph = build_graph_from_parsed_manifest(&manifest)?;
    Ok(ManifestGraphReport {
        graph,
        diagnostics,
        manifest,
    })
}

/// Strict graph-building wrapper. Unknown resource kinds are rejected instead
/// of being omitted, while [`build_graph_from_manifest`] keeps its historical
/// permissive behavior.
pub fn build_graph_from_manifest_strict(manifest_path: &Path) -> Result<LineageGraph> {
    let report = super::load_manifest_report(manifest_path)?;
    let manifest = report.into_manifest_strict()?;
    build_graph_from_parsed_manifest_strict(&manifest)
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
    add_semantic_layer_nodes(&mut graph, &mut node_map, &manifest.semantic_models);
    add_semantic_layer_nodes(&mut graph, &mut node_map, &manifest.metrics);
    add_semantic_layer_nodes(&mut graph, &mut node_map, &manifest.saved_queries);

    // 5. Add edges from depends_on for regular nodes
    add_node_edges(&mut graph, &node_map, &manifest.nodes);

    // 6. Add edges from depends_on for exposures
    add_exposure_edges(&mut graph, &node_map, &manifest.exposures);

    // 7. Add edges from depends_on for semantic layer nodes
    add_depends_on_edges(&mut graph, &node_map, &manifest.semantic_models);
    add_depends_on_edges(&mut graph, &node_map, &manifest.metrics);
    add_depends_on_edges(&mut graph, &node_map, &manifest.saved_queries);

    Ok(graph)
}

/// Strict counterpart to [`build_graph_from_parsed_manifest`].
pub fn build_graph_from_parsed_manifest_strict(manifest: &Manifest) -> Result<LineageGraph> {
    if manifest.capabilities.future_schema {
        anyhow::bail!("manifest uses a future dbt schema version");
    }
    if let Some((unique_id, raw_type)) = find_unknown_resource(manifest) {
        anyhow::bail!("manifest resource '{unique_id}' uses unknown resource type '{raw_type}'");
    }
    build_graph_from_parsed_manifest(manifest)
}

/// Find resources that the graph builder cannot represent. This mirrors the
/// report loader's policy for the typed resource containers, while allowing a
/// parsed manifest to be checked directly without reparsing its JSON.
fn find_unknown_resource(manifest: &Manifest) -> Option<(String, String)> {
    let node = manifest.nodes.iter().find_map(|(unique_id, node)| {
        match classify_resource_type(&node.resource_type) {
            ManifestResourceType::Known(_) => None,
            ManifestResourceType::Unknown(raw_type) => Some((unique_id.clone(), raw_type)),
        }
    });
    if node.is_some() {
        return node;
    }

    let function = manifest.functions.iter().find_map(|(unique_id, value)| {
        let raw_type = value
            .get("resource_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("function");
        match classify_resource_type(raw_type) {
            ManifestResourceType::Known(_) => None,
            ManifestResourceType::Unknown(raw_type) => Some((unique_id.clone(), raw_type)),
        }
    });
    if function.is_some() {
        return function;
    }

    // `extra` is the parsed representation of the same unknown top-level
    // keys tracked by `ManifestCapabilities`; reading it also keeps direct
    // callers that construct a Manifest without observations safe.
    manifest.extra.iter().find_map(|(_map_name, value)| {
        let entries = value.as_object()?;
        entries.iter().find_map(|(unique_id, value)| {
            let raw_type = value.get("resource_type")?.as_str()?;
            match classify_resource_type(raw_type) {
                ManifestResourceType::Known(_) => None,
                ManifestResourceType::Unknown(raw_type) => Some((unique_id.clone(), raw_type)),
            }
        })
    })
}

fn index_node(node_map: &mut HashMap<String, NodeIndex>, orig_id: &str, idx: NodeIndex) {
    node_map.insert(orig_id.to_string(), idx);
}

fn add_source_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    sources: &HashMap<String, ManifestSource>,
) {
    for (orig_id, source) in sources {
        let identity = ManifestGraphIdentity::from_resource(orig_id, "source");
        let label = format!("{}.{}", source.source_name, source.name);

        let idx = graph.add_node(NodeData {
            unique_id: identity.canonical_id,
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
            aliases: identity.alias.into_iter().collect(),
        });
        // Dependencies in manifests use canonical original IDs.
        index_node(node_map, orig_id, idx);
    }
}

fn add_regular_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    nodes: &HashMap<String, ManifestNode>,
) {
    for (orig_id, node) in nodes {
        let ManifestResourceType::Known(node_type) = classify_resource_type(&node.resource_type)
        else {
            // Unsupported resource kinds are retained by Manifest and
            // diagnosed by the report loader, but must not masquerade as
            // models in a graph.
            continue;
        };
        let identity = ManifestGraphIdentity::from_resource(orig_id, &node.resource_type);

        let idx = graph.add_node(NodeData {
            unique_id: identity.canonical_id,
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
            aliases: identity.alias.into_iter().collect(),
        });
        index_node(node_map, orig_id, idx);
    }
}

fn add_exposure_nodes(
    graph: &mut LineageGraph,
    node_map: &mut HashMap<String, NodeIndex>,
    exposures: &HashMap<String, ManifestExposure>,
) {
    for (orig_id, exposure) in exposures {
        let identity = ManifestGraphIdentity::from_resource(orig_id, "exposure");

        let idx = graph.add_node(NodeData {
            unique_id: identity.canonical_id,
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
            aliases: identity.alias.into_iter().collect(),
        });
        index_node(node_map, orig_id, idx);
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
    fn label(&self) -> Option<&str>;
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
    fn label(&self) -> Option<&str> {
        self.label.as_deref()
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
    fn label(&self) -> Option<&str> {
        self.label.as_deref()
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
    fn label(&self) -> Option<&str> {
        self.label.as_deref()
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
        let identity = ManifestGraphIdentity::from_resource(orig_id, resource_type);
        let idx = graph.add_node(NodeData {
            unique_id: identity.canonical_id,
            label: item.label().unwrap_or_else(|| item.name()).to_string(),
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
            aliases: identity.alias.into_iter().collect(),
        });
        index_node(node_map, orig_id, idx);
    }
}

fn add_depends_on_edges<T: HasSemanticLayerFields>(
    graph: &mut LineageGraph,
    node_map: &HashMap<String, NodeIndex>,
    items: &HashMap<String, T>,
) {
    for (orig_id, item) in items {
        let Some(&current_idx) = node_map.get(orig_id) else {
            continue;
        };
        for dep_id in item.depends_on_nodes() {
            if let Some(&dep_idx) = node_map.get(dep_id) {
                graph.add_edge(
                    dep_idx,
                    current_idx,
                    EdgeData::direct(infer_edge_type(dep_id)),
                );
            }
        }
    }
}

/// Infer the edge type from a dependency unique_id
pub(crate) fn infer_edge_type(dep_unique_id: &str) -> EdgeType {
    if dep_unique_id.starts_with("source.") {
        EdgeType::Source
    } else if dep_unique_id.starts_with("test.") {
        EdgeType::Test
    } else {
        EdgeType::Ref
    }
}

/// Return None for empty or whitespace-only strings
pub(crate) fn non_empty_string(s: &Option<String>) -> Option<String> {
    s.as_ref().filter(|v| !v.trim().is_empty()).cloned()
}
