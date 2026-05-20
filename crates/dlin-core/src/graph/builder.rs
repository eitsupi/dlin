use anyhow::Result;
use petgraph::stable_graph::NodeIndex;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

use std::path::PathBuf;

use crate::graph::types::{ExposureInfo, OwnerInfo};
use crate::parser::cache;
use crate::parser::columns::extract_select_columns;
use crate::parser::discovery::DiscoveredFiles;
use crate::parser::jinja::JinjaExtraction;
use crate::parser::sql::{
    RefCall, SourceCall, extract_all_with_vars, extract_refs_and_sources_with_vars,
};
use crate::parser::yaml_schema::{ExposureDefinition, SchemaFile, parse_schema_file};

/// Read all macro SQL files, filter out unparseable ones, and return a
/// pre-built prefix string for prepending to model templates.
fn load_macro_prefix(files: &DiscoveredFiles) -> String {
    let sources: Vec<String> = files
        .macro_sql_files
        .iter()
        .filter_map(|path| match std::fs::read_to_string(path) {
            Ok(content) => Some(content),
            Err(e) => {
                crate::warn!("could not read macro file {}: {}", path.display(), e);
                None
            }
        })
        .collect();
    crate::parser::jinja::build_macro_prefix(&sources)
}

use super::types::*;

/// Shared state threaded through the build_graph helper functions
struct GraphBuilder {
    graph: LineageGraph,
    node_map: HashMap<String, NodeIndex>,
}

impl GraphBuilder {
    fn new() -> Self {
        Self {
            graph: LineageGraph::new(),
            node_map: HashMap::new(),
        }
    }

    /// Add a node and register it in the node map
    fn add_node(&mut self, data: NodeData) -> NodeIndex {
        let idx = self.graph.add_node(data);
        let unique_id = self.graph[idx].unique_id.clone();
        self.node_map.insert(unique_id, idx);
        idx
    }

    /// Get or create a phantom ref node, returning its index
    fn get_or_create_phantom_ref(&mut self, ref_name: &str, sql_path: &Path) -> NodeIndex {
        let dep_id = resolve_ref(ref_name, &self.node_map);
        if let Some(&idx) = self.node_map.get(&dep_id) {
            return idx;
        }
        crate::warn!("unresolved ref '{}' in {}", ref_name, sql_path.display());
        let phantom_id = format!("model.{}", ref_name);
        self.add_node(NodeData {
            unique_id: phantom_id,
            label: ref_name.to_string(),
            node_type: NodeType::Phantom,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        })
    }

    /// Get or create a phantom source node, returning its index
    fn get_or_create_phantom_source(
        &mut self,
        source_name: &str,
        table_name: &str,
        sql_path: &Path,
    ) -> NodeIndex {
        let source_id = format!("source.{}.{}", source_name, table_name);
        if let Some(&idx) = self.node_map.get(&source_id) {
            return idx;
        }
        crate::warn!(
            "unresolved source '{}.{}' in {}",
            source_name,
            table_name,
            sql_path.display()
        );
        let label = format!("{}.{}", source_name, table_name);
        self.add_node(NodeData {
            unique_id: source_id,
            label,
            node_type: NodeType::Phantom,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        })
    }
}

/// Read a file with a descriptive error
fn read_file(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| {
        crate::error::DbtLineageError::FileReadError {
            path: path.to_path_buf(),
            source: e,
        }
        .into()
    })
}

/// Extract the file stem as a string, defaulting to "unknown"
fn file_stem_str(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string()
}

/// Create source nodes from a single schema file's source definitions
fn add_source_nodes(
    gb: &mut GraphBuilder,
    schema: &crate::parser::yaml_schema::SchemaFile,
    yaml_path: &Path,
    project_dir: &Path,
) {
    let relative_path = yaml_path
        .strip_prefix(project_dir)
        .unwrap_or(yaml_path)
        .to_path_buf();
    for source_def in &schema.sources {
        for table in &source_def.tables {
            let unique_id = format!("source.{}.{}", source_def.name, table.name);
            let label = format!("{}.{}", source_def.name, table.name);
            gb.add_node(NodeData {
                unique_id,
                label,
                node_type: NodeType::Source,
                file_path: Some(relative_path.clone()),
                description: table
                    .description
                    .clone()
                    .or_else(|| source_def.description.clone()),
                materialization: None,
                tags: vec![],
                columns: vec![],
                exposure: None,
            });
        }
    }
}

/// Metadata collected from YAML for a model
#[derive(Clone, Default)]
struct YamlModelMeta {
    description: Option<String>,
    materialization: Option<String>,
    tags: Vec<String>,
    columns: Vec<String>,
}

/// Result of parsing YAML schema files.
/// The third element pairs each `SchemaFile` with the relative path of the
/// YAML file it was parsed from (used to populate `file_path` on test nodes).
type YamlResult = (
    HashMap<String, YamlModelMeta>,
    Vec<ExposureDefinition>,
    Vec<(SchemaFile, PathBuf)>,
);

/// Parse YAML schema files: create source nodes, collect model metadata, exposures,
/// and parsed schemas (for generic test extraction).
fn process_yaml_files(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
) -> Result<YamlResult> {
    let mut model_meta: HashMap<String, YamlModelMeta> = HashMap::new();
    let mut exposures: Vec<ExposureDefinition> = Vec::new();
    let mut schemas: Vec<(SchemaFile, PathBuf)> = Vec::new();

    // Sort YAML paths so that duplicate-test-ID suffixes (_2, _3, …) are
    // deterministic across filesystems/OSes.
    let mut sorted_yaml_files = files.yaml_files.clone();
    sorted_yaml_files.sort();

    for yaml_path in &sorted_yaml_files {
        let content = read_file(yaml_path)?;
        let schema = match parse_schema_file(&content, Some(yaml_path.as_path())) {
            Ok(s) => s,
            Err(_) => continue,
        };

        add_source_nodes(gb, &schema, yaml_path, project_dir);

        for model_def in &schema.models {
            let mut meta = YamlModelMeta {
                description: model_def.description.clone(),
                columns: model_def.columns.iter().map(|c| c.name.clone()).collect(),
                ..Default::default()
            };
            // Merge tags from model-level and config-level
            let mut tags = model_def.tags.clone();
            if let Some(cfg) = &model_def.config {
                meta.materialization = cfg.materialized.clone();
                tags.extend(cfg.tags.clone());
            }
            tags.sort();
            tags.dedup();
            meta.tags = tags;
            model_meta.insert(model_def.name.clone(), meta);
        }

        exposures.extend(schema.exposures.iter().cloned());
        let relative_path = yaml_path
            .strip_prefix(project_dir)
            .unwrap_or(yaml_path)
            .to_path_buf();
        schemas.push((schema, relative_path));
    }

    Ok((model_meta, exposures, schemas))
}

/// Cached extraction result for a model SQL file (refs and sources).
/// Avoids re-running minijinja in `process_sql_edges`.
type ExtractionCache = HashMap<PathBuf, (Vec<RefCall>, Vec<SourceCall>)>;

/// Result of parallel extraction for a single model SQL file
struct ModelExtraction {
    sql_path: PathBuf,
    model_name: String,
    extraction: Option<JinjaExtraction>,
    columns: Vec<String>,
    /// Whether this extraction came from the disk cache (no need to re-save)
    from_cache: bool,
}

/// Create nodes for model SQL files (with duplicate detection).
/// Returns an in-memory cache of refs/sources (for `process_sql_edges`)
/// and updates the disk cache with newly extracted results.
fn process_model_files(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
    model_meta: &HashMap<String, YamlModelMeta>,
    macro_prefix: &str,
    disk_cache: &mut cache::ExtractionCache,
    vars: &HashMap<String, serde_json::Value>,
) -> ExtractionCache {
    // Parallel phase: read files and run minijinja extraction concurrently.
    // Uses disk cache (immutable borrow) to skip rendering for unchanged files.
    let cache_ref = &*disk_cache;
    let extractions: Vec<ModelExtraction> = files
        .model_sql_files
        .par_iter()
        .map(|sql_path| {
            let model_name = file_stem_str(sql_path);

            // Check disk cache first
            if let Some(cached) = cache_ref.get(sql_path, project_dir) {
                let sql_content = std::fs::read_to_string(sql_path).ok();
                let columns = sql_content
                    .as_ref()
                    .map(|content| extract_select_columns(content))
                    .unwrap_or_default();
                return ModelExtraction {
                    sql_path: sql_path.clone(),
                    model_name,
                    extraction: Some(cached.clone()),
                    columns,
                    from_cache: true,
                };
            }

            let sql_content = std::fs::read_to_string(sql_path).ok();

            let extraction = sql_content
                .as_ref()
                .map(|content| extract_all_with_vars(content, macro_prefix, vars));

            let columns = sql_content
                .as_ref()
                .map(|content| extract_select_columns(content))
                .unwrap_or_default();

            ModelExtraction {
                sql_path: sql_path.clone(),
                model_name,
                extraction,
                columns,
                from_cache: false,
            }
        })
        .collect();

    // Sequential phase: insert nodes into the graph and update disk cache
    let mut model_name_paths: HashMap<String, std::path::PathBuf> = HashMap::new();
    let mut mem_cache: ExtractionCache = HashMap::new();

    for me in extractions {
        if let Some(existing_path) = model_name_paths.get(&me.model_name) {
            crate::warn!(
                "duplicate model name '{}' in {} and {}",
                me.model_name,
                existing_path.display(),
                me.sql_path.display()
            );
        }
        model_name_paths.insert(me.model_name.clone(), me.sql_path.clone());

        let from_cache = me.from_cache;
        let (sql_config, cached_refs_sources) = match me.extraction {
            Some(ext) => {
                // Save newly extracted results to disk cache
                if !from_cache {
                    disk_cache.insert(&me.sql_path, project_dir, &ext);
                }
                (ext.config, Some((ext.refs, ext.sources)))
            }
            None => (Default::default(), None),
        };

        if let Some(rs) = cached_refs_sources {
            mem_cache.insert(me.sql_path.clone(), rs);
        }

        let yaml_meta = model_meta.get(&me.model_name);

        let materialization = sql_config
            .materialized
            .or_else(|| yaml_meta.and_then(|m| m.materialization.clone()));

        let mut tags = sql_config.tags;
        if let Some(meta) = yaml_meta {
            tags.extend(meta.tags.clone());
        }
        tags.sort();
        tags.dedup();

        let unique_id = format!("model.{}", me.model_name);
        let relative_path = me
            .sql_path
            .strip_prefix(project_dir)
            .unwrap_or(&me.sql_path)
            .to_path_buf();

        // Prefer YAML-defined columns; fall back to SQL extraction (best-effort)
        let columns = match yaml_meta {
            Some(m) if !m.columns.is_empty() => m.columns.clone(),
            _ => me.columns,
        };

        gb.add_node(NodeData {
            unique_id,
            label: me.model_name,
            node_type: NodeType::Model,
            file_path: Some(relative_path),
            description: yaml_meta.and_then(|m| m.description.clone()),
            materialization,
            tags,
            columns,
            exposure: None,
        });
    }

    mem_cache
}

/// Create nodes for simple file-based resources (seeds, snapshots)
fn process_simple_nodes(
    gb: &mut GraphBuilder,
    paths: &[std::path::PathBuf],
    project_dir: &Path,
    prefix: &str,
    node_type: NodeType,
) {
    for path in paths {
        let name = file_stem_str(path);
        let unique_id = format!("{}.{}", prefix, name);
        let relative_path = path.strip_prefix(project_dir).unwrap_or(path).to_path_buf();

        gb.add_node(NodeData {
            unique_id,
            label: name,
            node_type,
            file_path: Some(relative_path),
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        });
    }
}

/// Parse SQL files for ref()/source() calls and add edges.
/// `extraction_cache` contains pre-extracted refs/sources for model files
/// (from `process_model_files`) to avoid redundant minijinja renders.
fn process_sql_edges(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
    macro_prefix: &str,
    extraction_cache: &ExtractionCache,
    vars: &HashMap<String, serde_json::Value>,
) -> Result<()> {
    let all_sql_files: Vec<(&std::path::PathBuf, &str)> = files
        .model_sql_files
        .iter()
        .map(|p| (p, "model"))
        .chain(files.snapshot_sql_files.iter().map(|p| (p, "snapshot")))
        .chain(files.test_sql_files.iter().map(|p| (p, "test")))
        .collect();

    for (sql_path, file_type) in &all_sql_files {
        let node_name = file_stem_str(sql_path);
        let node_unique_id = format!("{}.{}", file_type, node_name);

        // Create test nodes on the fly
        if *file_type == "test" {
            let relative_path = sql_path
                .strip_prefix(project_dir)
                .unwrap_or(sql_path)
                .to_path_buf();
            gb.add_node(NodeData {
                unique_id: node_unique_id.clone(),
                label: node_name,
                node_type: NodeType::Test,
                file_path: Some(relative_path),
                description: None,
                materialization: None,
                tags: vec![],
                columns: vec![],
                exposure: None,
            });
        }

        let current_idx = match gb.node_map.get(&node_unique_id) {
            Some(&idx) => idx,
            None => continue,
        };

        // Use cached extraction for model files; extract fresh for others
        let owned;
        let (refs, sources) = if let Some(cached) = extraction_cache.get(*sql_path) {
            (&cached.0, &cached.1)
        } else {
            let content = read_file(sql_path)?;
            owned = extract_refs_and_sources_with_vars(&content, macro_prefix, vars);
            (&owned.0, &owned.1)
        };

        // Use EdgeType::Test when the target node is a test, so all test
        // relationships render with consistent edge labels/styles.
        let is_test = *file_type == "test";

        for ref_call in refs {
            let dep_idx = gb.get_or_create_phantom_ref(&ref_call.name, sql_path);
            let edge_type = if is_test {
                EdgeType::Test
            } else {
                EdgeType::Ref
            };
            gb.graph
                .add_edge(dep_idx, current_idx, EdgeData::direct(edge_type));
        }

        for source_call in sources {
            let source_idx = gb.get_or_create_phantom_source(
                &source_call.source_name,
                &source_call.table_name,
                sql_path,
            );
            let edge_type = if is_test {
                EdgeType::Test
            } else {
                EdgeType::Source
            };
            gb.graph
                .add_edge(source_idx, current_idx, EdgeData::direct(edge_type));
        }
    }

    Ok(())
}

/// Create exposure nodes and edges to their dependencies
fn process_exposures(gb: &mut GraphBuilder, exposures: &[ExposureDefinition]) {
    for exposure in exposures {
        let unique_id = format!("exposure.{}", exposure.name);
        let idx = gb.add_node(NodeData {
            unique_id,
            label: exposure.name.clone(),
            node_type: NodeType::Exposure,
            file_path: None,
            description: exposure.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: Some(ExposureInfo {
                label: exposure.label.clone(),
                exposure_type: exposure.exposure_type.clone(),
                url: exposure.url.clone(),
                maturity: exposure.maturity.clone(),
                owner: exposure.owner.as_ref().map(|o| OwnerInfo {
                    name: o.name.as_ref().filter(|s| !s.trim().is_empty()).cloned(),
                    email: o.email.as_ref().filter(|s| !s.trim().is_empty()).cloned(),
                }),
            }),
        });

        for dep in &exposure.depends_on {
            if let Some(model_name) = parse_exposure_ref(dep) {
                let dep_id = resolve_ref(&model_name, &gb.node_map);
                if let Some(&dep_idx) = gb.node_map.get(&dep_id) {
                    gb.graph
                        .add_edge(dep_idx, idx, EdgeData::direct(EdgeType::Exposure));
                }
            }
        }
    }
}

/// Deduplicate a candidate unique_id by appending `_2`, `_3`, … if it already
/// exists in the node map.  Returns `(unique_id, suffix)` where `suffix` is
/// `None` when no deduplication was needed, or `Some("_2")` etc. when it was.
/// Callers can append the suffix to labels so they stay distinct too.
fn dedup_unique_id(
    candidate: &str,
    node_map: &HashMap<String, NodeIndex>,
) -> (String, Option<String>) {
    if !node_map.contains_key(candidate) {
        return (candidate.to_string(), None);
    }
    let mut n = 2u32;
    loop {
        let suffix = format!("_{}", n);
        let suffixed = format!("{}{}", candidate, suffix);
        if !node_map.contains_key(&suffixed) {
            return (suffixed, Some(suffix));
        }
        n += 1;
    }
}

/// Add a generic test node to the graph and connect it to the parent.
fn add_generic_test_node(
    gb: &mut GraphBuilder,
    parent_idx: NodeIndex,
    unique_id: String,
    label: String,
    file_path: Option<PathBuf>,
) {
    let idx = gb.add_node(NodeData {
        unique_id,
        label,
        node_type: NodeType::Test,
        file_path,
        description: None,
        materialization: None,
        tags: vec![],
        columns: vec![],
        exposure: None,
    });
    gb.graph
        .add_edge(parent_idx, idx, EdgeData::direct(EdgeType::Test));
}

/// Create test nodes for YAML-declared generic tests (not_null, unique, etc.)
/// and connect them to their parent model/source nodes.
fn process_generic_tests(gb: &mut GraphBuilder, schemas: &[(SchemaFile, PathBuf)]) {
    for (schema, yaml_path) in schemas {
        let file_path = Some(yaml_path.clone());

        // Model-level generic tests
        for model_def in &schema.models {
            let parent_id = format!("model.{}", model_def.name);
            let parent_idx = match gb.node_map.get(&parent_id) {
                Some(&idx) => idx,
                None => continue,
            };

            // Model-level tests (not attached to a column)
            for test_def in &model_def.tests {
                let test_name = match test_def.test_name() {
                    Some(name) => name,
                    None => continue,
                };
                let candidate = format!("test.{}.{}", test_name, model_def.name);
                let (unique_id, suffix) = dedup_unique_id(&candidate, &gb.node_map);
                let mut label = format!("{}_{}", test_name, model_def.name);
                if let Some(s) = suffix {
                    label.push_str(&s);
                }
                add_generic_test_node(gb, parent_idx, unique_id, label, file_path.clone());
            }

            // Column-level tests
            for col in &model_def.columns {
                for test_def in &col.tests {
                    let test_name = match test_def.test_name() {
                        Some(name) => name,
                        None => continue,
                    };
                    let candidate = format!("test.{}.{}.{}", test_name, model_def.name, col.name);
                    let (unique_id, suffix) = dedup_unique_id(&candidate, &gb.node_map);
                    let mut label = format!("{}_{}_{}", test_name, model_def.name, col.name);
                    if let Some(s) = suffix {
                        label.push_str(&s);
                    }
                    add_generic_test_node(gb, parent_idx, unique_id, label, file_path.clone());
                }
            }
        }

        // Source-level generic tests (column-level only)
        for source_def in &schema.sources {
            for table in &source_def.tables {
                let parent_id = format!("source.{}.{}", source_def.name, table.name);
                let parent_idx = match gb.node_map.get(&parent_id) {
                    Some(&idx) => idx,
                    None => continue,
                };
                for col in &table.columns {
                    for test_def in &col.tests {
                        let test_name = match test_def.test_name() {
                            Some(name) => name,
                            None => continue,
                        };
                        let candidate = format!(
                            "test.{}.{}.{}.{}",
                            test_name, source_def.name, table.name, col.name
                        );
                        let (unique_id, suffix) = dedup_unique_id(&candidate, &gb.node_map);
                        let mut label = format!(
                            "{}_{}_{}_{}",
                            test_name, source_def.name, table.name, col.name
                        );
                        if let Some(s) = suffix {
                            label.push_str(&s);
                        }
                        add_generic_test_node(gb, parent_idx, unique_id, label, file_path.clone());
                    }
                }
            }
        }
    }
}

/// Build the lineage graph from discovered files.
/// If `cache_dir` is provided, it is used as the cache directory;
/// otherwise the cache is stored under `<project_dir>/.dlin_cache/`.
/// If `no_cache` is true, the extraction cache is completely disabled.
/// If `refresh_cache` is true, the existing cache is ignored but new results
/// are written to disk.
pub fn build_graph(
    project_dir: &Path,
    files: &DiscoveredFiles,
    cache_dir: Option<&Path>,
    no_cache: bool,
    refresh_cache: bool,
    vars: &HashMap<String, serde_json::Value>,
) -> Result<LineageGraph> {
    let mut gb = GraphBuilder::new();
    let macro_prefix = load_macro_prefix(files);
    let mut disk_cache = if no_cache {
        cache::ExtractionCache::disabled()
    } else if refresh_cache {
        cache::ExtractionCache::fresh(project_dir, &macro_prefix, vars, cache_dir)
    } else {
        cache::ExtractionCache::load(project_dir, &macro_prefix, vars, cache_dir)
    };

    let (model_meta, exposures, schemas) = process_yaml_files(&mut gb, files, project_dir)?;
    let extraction_cache = process_model_files(
        &mut gb,
        files,
        project_dir,
        &model_meta,
        &macro_prefix,
        &mut disk_cache,
        vars,
    );
    process_simple_nodes(
        &mut gb,
        &files.seed_files,
        project_dir,
        "seed",
        NodeType::Seed,
    );
    process_simple_nodes(
        &mut gb,
        &files.snapshot_sql_files,
        project_dir,
        "snapshot",
        NodeType::Snapshot,
    );
    process_sql_edges(
        &mut gb,
        files,
        project_dir,
        &macro_prefix,
        &extraction_cache,
        vars,
    )?;
    process_exposures(&mut gb, &exposures);
    process_generic_tests(&mut gb, &schemas);

    disk_cache.save();

    Ok(gb.graph)
}

/// Try to resolve a ref name to a node unique_id
fn resolve_ref(name: &str, node_map: &HashMap<String, NodeIndex>) -> String {
    // Try model first, then seed, then snapshot
    let model_id = format!("model.{}", name);
    if node_map.contains_key(&model_id) {
        return model_id;
    }

    let seed_id = format!("seed.{}", name);
    if node_map.contains_key(&seed_id) {
        return seed_id;
    }

    let snapshot_id = format!("snapshot.{}", name);
    if node_map.contains_key(&snapshot_id) {
        return snapshot_id;
    }

    // Default to model
    model_id
}

/// Parse a ref('name') or source('src', 'table') string from exposure depends_on
fn parse_exposure_ref(dep: &str) -> Option<String> {
    let dep = dep.trim();
    if dep.starts_with("ref(") {
        // Extract name from ref('name')
        let inner = dep.trim_start_matches("ref(").trim_end_matches(')');
        let name = inner.trim().trim_matches('\'').trim_matches('"');
        Some(name.to_string())
    } else if dep.starts_with("source(") {
        // For sources in exposures, we won't create edges here for simplicity
        None
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::discovery::DiscoveredFiles;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn test_resolve_ref_model() {
        let mut node_map = HashMap::new();
        let graph = &mut LineageGraph::new();
        let idx = graph.add_node(NodeData {
            unique_id: "model.orders".to_string(),
            label: "orders".to_string(),
            node_type: NodeType::Model,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        });
        node_map.insert("model.orders".to_string(), idx);

        assert_eq!(resolve_ref("orders", &node_map), "model.orders");
    }

    #[test]
    fn test_resolve_ref_seed() {
        let mut node_map = HashMap::new();
        let graph = &mut LineageGraph::new();
        let idx = graph.add_node(NodeData {
            unique_id: "seed.countries".to_string(),
            label: "countries".to_string(),
            node_type: NodeType::Seed,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        });
        node_map.insert("seed.countries".to_string(), idx);

        assert_eq!(resolve_ref("countries", &node_map), "seed.countries");
    }

    #[test]
    fn test_resolve_ref_snapshot() {
        let mut node_map = HashMap::new();
        let graph = &mut LineageGraph::new();
        let idx = graph.add_node(NodeData {
            unique_id: "snapshot.snap_orders".to_string(),
            label: "snap_orders".to_string(),
            node_type: NodeType::Snapshot,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
        });
        node_map.insert("snapshot.snap_orders".to_string(), idx);

        assert_eq!(
            resolve_ref("snap_orders", &node_map),
            "snapshot.snap_orders"
        );
    }

    #[test]
    fn test_resolve_ref_unknown_defaults_to_model() {
        let node_map = HashMap::new();
        assert_eq!(resolve_ref("unknown_ref", &node_map), "model.unknown_ref");
    }

    #[test]
    fn test_parse_exposure_ref() {
        assert_eq!(
            parse_exposure_ref("ref('orders')"),
            Some("orders".to_string())
        );
        assert_eq!(
            parse_exposure_ref("ref(\"orders\")"),
            Some("orders".to_string())
        );
        assert_eq!(parse_exposure_ref("source('raw', 'orders')"), None);
        assert_eq!(parse_exposure_ref("something_else"), None);
    }

    /// Helper to create a temporary dbt project for build_graph tests
    fn setup_temp_project() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();

        // Create model files
        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        fs::write(
            models_dir.join("stg_orders.sql"),
            "SELECT * FROM {{ source('raw', 'orders') }}",
        )
        .unwrap();

        fs::write(
            models_dir.join("orders.sql"),
            "SELECT * FROM {{ ref('stg_orders') }}",
        )
        .unwrap();

        // Create schema YAML with source definition
        fs::write(
            models_dir.join("schema.yml"),
            r#"
version: 2
sources:
  - name: raw
    tables:
      - name: orders
        description: "Raw orders table"
models:
  - name: stg_orders
    description: "Staged orders"
"#,
        )
        .unwrap();

        (tmp, project_dir)
    }

    #[test]
    fn test_build_graph_sources_and_models() {
        let (_tmp, project_dir) = setup_temp_project();

        let files = DiscoveredFiles {
            model_sql_files: vec![
                project_dir.join("models/stg_orders.sql"),
                project_dir.join("models/orders.sql"),
            ],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        // Use no_cache: false to exercise the cache-enabled path end-to-end
        let graph = build_graph(&project_dir, &files, None, false, false, &HashMap::new()).unwrap();

        // Should have source + 2 models = 3 nodes
        assert_eq!(graph.node_count(), 3);

        // Check node types
        let mut types: Vec<NodeType> = graph.node_indices().map(|i| graph[i].node_type).collect();
        types.sort_by_key(|t| format!("{:?}", t));
        assert!(types.contains(&NodeType::Source));
        assert!(types.iter().filter(|t| **t == NodeType::Model).count() == 2);

        // Should have 2 edges: source→stg_orders, stg_orders→orders
        assert_eq!(graph.edge_count(), 2);
    }

    #[test]
    fn test_build_graph_with_seeds() {
        let (_tmp, project_dir) = setup_temp_project();

        // Add a seed
        let seeds_dir = project_dir.join("seeds");
        fs::create_dir_all(&seeds_dir).unwrap();
        fs::write(seeds_dir.join("countries.csv"), "id,name\n1,US\n").unwrap();

        let files = DiscoveredFiles {
            seed_files: vec![project_dir.join("seeds/countries.csv")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        assert_eq!(graph.node_count(), 1);
        let node = &graph[graph.node_indices().next().unwrap()];
        assert_eq!(node.node_type, NodeType::Seed);
        assert_eq!(node.label, "countries");
    }

    #[test]
    fn test_build_graph_with_snapshots() {
        let (_tmp, project_dir) = setup_temp_project();

        let snap_dir = project_dir.join("snapshots");
        fs::create_dir_all(&snap_dir).unwrap();
        fs::write(snap_dir.join("snap_orders.sql"), "SELECT 1").unwrap();

        let files = DiscoveredFiles {
            snapshot_sql_files: vec![project_dir.join("snapshots/snap_orders.sql")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        assert_eq!(graph.node_count(), 1);
        let node = &graph[graph.node_indices().next().unwrap()];
        assert_eq!(node.node_type, NodeType::Snapshot);
        assert_eq!(node.label, "snap_orders");
    }

    #[test]
    fn test_build_graph_with_tests() {
        let (_tmp, project_dir) = setup_temp_project();

        let test_dir = project_dir.join("tests");
        fs::create_dir_all(&test_dir).unwrap();
        fs::write(
            test_dir.join("assert_positive.sql"),
            "SELECT * FROM {{ ref('stg_orders') }} WHERE amount < 0",
        )
        .unwrap();

        // Need the model that the test references
        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("stg_orders.sql"), "SELECT 1").unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
            test_sql_files: vec![project_dir.join("tests/assert_positive.sql")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // model + test = 2 nodes
        assert_eq!(graph.node_count(), 2);
        // test edge: stg_orders → assert_positive
        assert_eq!(graph.edge_count(), 1);

        // Singular SQL tests should use EdgeType::Test
        use petgraph::visit::IntoEdgeReferences;
        let edge = graph.edge_references().next().unwrap();
        assert_eq!(edge.weight().edge_type, EdgeType::Test);
    }

    #[test]
    fn test_build_graph_with_exposures() {
        let (_tmp, project_dir) = setup_temp_project();

        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(models_dir.join("orders.sql"), "SELECT 1").unwrap();

        fs::write(
            models_dir.join("schema.yml"),
            r#"
version: 2
sources: []
models: []
exposures:
  - name: weekly_report
    description: "Weekly report dashboard"
    depends_on:
      - ref('orders')
"#,
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/orders.sql")],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // model + exposure = 2 nodes
        assert_eq!(graph.node_count(), 2);
        // exposure edge: orders → weekly_report
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_build_graph_ref_resolves_to_seed() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();

        let models_dir = project_dir.join("models");
        let seeds_dir = project_dir.join("seeds");
        fs::create_dir_all(&models_dir).unwrap();
        fs::create_dir_all(&seeds_dir).unwrap();

        fs::write(seeds_dir.join("countries.csv"), "id,name\n1,US\n").unwrap();
        fs::write(
            models_dir.join("stg_countries.sql"),
            "SELECT * FROM {{ ref('countries') }}",
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/stg_countries.sql")],
            seed_files: vec![project_dir.join("seeds/countries.csv")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // seed + model = 2 nodes (no phantom)
        assert_eq!(graph.node_count(), 2);
        // ref edge: countries → stg_countries
        assert_eq!(graph.edge_count(), 1);

        // Verify the seed node is properly typed (not phantom)
        let seed_node = graph
            .node_indices()
            .find(|&i| graph[i].label == "countries")
            .unwrap();
        assert_eq!(graph[seed_node].node_type, NodeType::Seed);
    }

    #[test]
    fn test_build_graph_phantom_node_for_unresolved_ref() {
        let (_tmp, project_dir) = setup_temp_project();

        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(
            models_dir.join("orders.sql"),
            "SELECT * FROM {{ ref('nonexistent_model') }}",
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/orders.sql")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // model + phantom = 2 nodes
        assert_eq!(graph.node_count(), 2);
        let phantom = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Phantom)
            .expect("Should have a phantom node");
        assert_eq!(graph[phantom].label, "nonexistent_model");
    }

    #[test]
    fn test_build_graph_phantom_node_for_unresolved_source() {
        let (_tmp, project_dir) = setup_temp_project();

        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();
        fs::write(
            models_dir.join("orders.sql"),
            "SELECT * FROM {{ source('unknown_src', 'unknown_table') }}",
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/orders.sql")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // model + phantom source = 2 nodes
        assert_eq!(graph.node_count(), 2);
        let phantom = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Phantom)
            .expect("Should have a phantom source node");
        assert_eq!(graph[phantom].label, "unknown_src.unknown_table");
    }

    #[test]
    fn test_build_graph_model_descriptions() {
        let (_tmp, project_dir) = setup_temp_project();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        let stg = graph
            .node_indices()
            .find(|&i| graph[i].label == "stg_orders")
            .unwrap();
        assert_eq!(graph[stg].description.as_deref(), Some("Staged orders"));
    }

    #[test]
    fn test_build_graph_edge_types() {
        use petgraph::visit::IntoEdgeReferences;

        let (_tmp, project_dir) = setup_temp_project();

        let files = DiscoveredFiles {
            model_sql_files: vec![
                project_dir.join("models/stg_orders.sql"),
                project_dir.join("models/orders.sql"),
            ],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        let edge_types: Vec<EdgeType> = graph
            .edge_references()
            .map(|e| e.weight().edge_type)
            .collect();
        assert!(edge_types.contains(&EdgeType::Source));
        assert!(edge_types.contains(&EdgeType::Ref));
    }

    #[test]
    fn test_build_graph_empty_files() {
        let tmp = tempfile::tempdir().unwrap();
        let files = DiscoveredFiles::default();
        let graph = build_graph(tmp.path(), &files, None, true, false, &HashMap::new()).unwrap();
        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_build_graph_model_config_merge() {
        // Covers lines 168-170: YAML model config with materialization and tags
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();

        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        fs::write(models_dir.join("stg_orders.sql"), "SELECT 1").unwrap();

        fs::write(
            models_dir.join("schema.yml"),
            r#"
version: 2
sources: []
models:
  - name: stg_orders
    description: "Staged orders"
    tags:
      - staging
    config:
      materialized: table
      tags:
        - daily
"#,
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/stg_orders.sql")],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        let stg = graph
            .node_indices()
            .find(|&i| graph[i].label == "stg_orders")
            .unwrap();
        assert_eq!(graph[stg].materialization.as_deref(), Some("table"));
        assert!(graph[stg].tags.contains(&"staging".to_string()));
        assert!(graph[stg].tags.contains(&"daily".to_string()));
    }

    #[test]
    fn test_build_graph_duplicate_model_name() {
        // Covers line 197: duplicate model name warning
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();

        let models_dir = project_dir.join("models");
        let subdir = models_dir.join("subdir");
        fs::create_dir_all(&subdir).unwrap();

        fs::write(models_dir.join("orders.sql"), "SELECT 1").unwrap();
        fs::write(subdir.join("orders.sql"), "SELECT 2").unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![
                project_dir.join("models/orders.sql"),
                project_dir.join("models/subdir/orders.sql"),
            ],
            ..Default::default()
        };

        // Should not panic, just warn on stderr about the duplicate
        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // Both SQL files produce nodes (duplicate warning is informational)
        let order_nodes: Vec<_> = graph
            .node_indices()
            .filter(|&i| graph[i].label == "orders")
            .collect();
        assert_eq!(order_nodes.len(), 2);
    }

    #[test]
    fn test_build_graph_file_paths_are_relative() {
        let (_tmp, project_dir) = setup_temp_project();

        let files = DiscoveredFiles {
            model_sql_files: vec![
                project_dir.join("models/stg_orders.sql"),
                project_dir.join("models/orders.sql"),
            ],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

        for idx in graph.node_indices() {
            let node = &graph[idx];
            if let Some(ref fp) = node.file_path {
                assert!(
                    fp.is_relative(),
                    "file_path for node '{}' should be relative but got: {}",
                    node.label,
                    fp.display()
                );
                assert!(
                    !fp.starts_with(&project_dir),
                    "file_path for node '{}' should not start with project_dir: {}",
                    node.label,
                    fp.display()
                );
            }
        }

        // Verify source node specifically has relative path
        let source_node = graph
            .node_indices()
            .find(|&i| graph[i].node_type == NodeType::Source)
            .expect("should have a source node");
        assert_eq!(
            graph[source_node].file_path.as_deref(),
            Some(std::path::Path::new("models/schema.yml"))
        );

        // Verify model node has relative path
        let model_node = graph
            .node_indices()
            .find(|&i| graph[i].label == "stg_orders")
            .unwrap();
        assert_eq!(
            graph[model_node].file_path.as_deref(),
            Some(std::path::Path::new("models/stg_orders.sql"))
        );
    }

    #[test]
    fn test_build_graph_with_macros() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();

        let models_dir = project_dir.join("models");
        let macros_dir = project_dir.join("macros");
        fs::create_dir_all(&models_dir).unwrap();
        fs::create_dir_all(&macros_dir).unwrap();

        // Macro that references a model
        fs::write(
            macros_dir.join("my_macro.sql"),
            r#"
{% macro my_cte() %}
    SELECT * FROM {{ ref('base_table') }}
{% endmacro %}
"#,
        )
        .unwrap();

        // Model that uses the macro
        fs::write(models_dir.join("base_table.sql"), "SELECT 1 as id").unwrap();
        fs::write(
            models_dir.join("derived.sql"),
            "SELECT * FROM ({{ my_cte() }})",
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![
                project_dir.join("models/base_table.sql"),
                project_dir.join("models/derived.sql"),
            ],
            macro_sql_files: vec![project_dir.join("macros/my_macro.sql")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();
        // base_table + derived = 2 nodes
        assert_eq!(graph.node_count(), 2);
        // ref edge: base_table → derived
        assert_eq!(graph.edge_count(), 1);

        // Verify the edge direction
        let base = graph
            .node_indices()
            .find(|&i| graph[i].label == "base_table")
            .unwrap();
        let derived = graph
            .node_indices()
            .find(|&i| graph[i].label == "derived")
            .unwrap();
        assert!(graph.contains_edge(base, derived));
    }

    #[test]
    fn test_var_list_expansion_resolves_refs() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        // dbt_project.yml with vars
        fs::write(project_dir.join("dbt_project.yml"), "name: var_test\n").unwrap();

        // Model that uses var() to iterate over categories and ref dynamically
        fs::write(
            models_dir.join("combined.sql"),
            r#"
            {%- set categories = var("product_categories") -%}
            {%- for cat in categories -%}
                SELECT * FROM {{ ref('stg_' ~ cat ~ '_summary') }}
                {% if not loop.last %}UNION ALL{% endif %}
            {%- endfor -%}
            "#,
        )
        .unwrap();

        // Stub models that the refs point to
        fs::write(models_dir.join("stg_electronics_summary.sql"), "SELECT 1").unwrap();
        fs::write(models_dir.join("stg_clothing_summary.sql"), "SELECT 1").unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![
                project_dir.join("models/combined.sql"),
                project_dir.join("models/stg_electronics_summary.sql"),
                project_dir.join("models/stg_clothing_summary.sql"),
            ],
            ..Default::default()
        };

        // Provide project-level vars
        let mut vars = HashMap::new();
        vars.insert(
            "product_categories".to_string(),
            serde_json::json!(["electronics", "clothing"]),
        );

        let graph = build_graph(&project_dir, &files, None, true, false, &vars).unwrap();

        // 3 model nodes: combined + stg_electronics_summary + stg_clothing_summary
        assert_eq!(graph.node_count(), 3);

        // 2 edges: stg_electronics_summary → combined, stg_clothing_summary → combined
        assert_eq!(graph.edge_count(), 2);

        let combined = graph
            .node_indices()
            .find(|&i| graph[i].label == "combined")
            .unwrap();
        let electronics = graph
            .node_indices()
            .find(|&i| graph[i].label == "stg_electronics_summary")
            .unwrap();
        let clothing = graph
            .node_indices()
            .find(|&i| graph[i].label == "stg_clothing_summary")
            .unwrap();
        assert!(graph.contains_edge(electronics, combined));
        assert!(graph.contains_edge(clothing, combined));
    }

    #[test]
    fn test_generic_tests_from_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let models_dir = project_dir.join("models");
        fs::create_dir_all(&models_dir).unwrap();

        fs::write(project_dir.join("dbt_project.yml"), "name: test_proj\n").unwrap();
        fs::write(models_dir.join("orders.sql"), "SELECT 1 AS order_id").unwrap();

        // Schema with generic tests on columns
        fs::write(
            models_dir.join("schema.yml"),
            r#"
sources:
  - name: raw
    tables:
      - name: events
        columns:
          - name: event_id
            data_tests:
              - not_null
models:
  - name: orders
    data_tests:
      - dbt_utils.expression_is_true:
          expression: "a = b"
      - dbt_utils.expression_is_true:
          expression: "c = d"
    columns:
      - name: order_id
        data_tests:
          - not_null
          - unique
"#,
        )
        .unwrap();

        let files = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/orders.sql")],
            yaml_files: vec![project_dir.join("models/schema.yml")],
            ..Default::default()
        };

        let graph = build_graph(&project_dir, &files, None, true, false, &HashMap::new()).unwrap();

        // 1 model + 1 source + 2 column tests + 1 source test + 2 model-level tests = 7
        assert_eq!(graph.node_count(), 7);

        let test_nodes: Vec<_> = graph
            .node_indices()
            .filter(|&i| graph[i].node_type == NodeType::Test)
            .collect();
        assert_eq!(test_nodes.len(), 5);

        // Verify test unique_ids
        let mut test_ids: Vec<&str> = test_nodes
            .iter()
            .map(|&i| graph[i].unique_id.as_str())
            .collect();
        test_ids.sort();
        assert!(test_ids.contains(&"test.not_null.orders.order_id"));
        assert!(test_ids.contains(&"test.unique.orders.order_id"));
        assert!(test_ids.contains(&"test.not_null.raw.events.event_id"));
        // Model-level tests: first gets base ID, second gets _2 suffix
        assert!(test_ids.contains(&"test.dbt_utils.expression_is_true.orders"));
        assert!(test_ids.contains(&"test.dbt_utils.expression_is_true.orders_2"));

        // All test edges from model (2 column + 2 model-level = 4)
        let model_idx = graph
            .node_indices()
            .find(|&i| graph[i].unique_id == "model.orders")
            .unwrap();
        let source_idx = graph
            .node_indices()
            .find(|&i| graph[i].unique_id == "source.raw.events")
            .unwrap();

        let model_test_edges = graph
            .edges_directed(model_idx, petgraph::Direction::Outgoing)
            .filter(|e| e.weight().edge_type == EdgeType::Test)
            .count();
        assert_eq!(model_test_edges, 4);

        let source_test_edges = graph
            .edges_directed(source_idx, petgraph::Direction::Outgoing)
            .filter(|e| e.weight().edge_type == EdgeType::Test)
            .count();
        assert_eq!(source_test_edges, 1);

        // All generic test nodes should have file_path pointing to the YAML file
        for &ti in &test_nodes {
            assert_eq!(
                graph[ti].file_path.as_deref(),
                Some(std::path::Path::new("models/schema.yml")),
                "test node '{}' should have file_path",
                graph[ti].unique_id,
            );
        }

        // Deduped test labels must also be distinct (suffix applied to label)
        let mut test_labels: Vec<&str> = test_nodes
            .iter()
            .map(|&i| graph[i].label.as_str())
            .collect();
        test_labels.sort();
        let deduped_len = test_labels.len();
        test_labels.dedup();
        assert_eq!(
            test_labels.len(),
            deduped_len,
            "All test labels should be unique"
        );
        // Verify the deduped model-level test labels
        assert!(test_labels.contains(&"dbt_utils.expression_is_true_orders"));
        assert!(test_labels.contains(&"dbt_utils.expression_is_true_orders_2"));
    }

    #[test]
    fn test_generic_test_ids_deterministic_across_yaml_order() {
        // Duplicate test names across two YAML files should produce the same
        // suffixed IDs regardless of the order the files are passed in.
        let tmp = tempfile::tempdir().unwrap();
        let project_dir = tmp.path().to_path_buf();
        let models_dir = project_dir.join("models");
        let sub_dir = models_dir.join("sub");
        fs::create_dir_all(&sub_dir).unwrap();

        fs::write(models_dir.join("orders.sql"), "SELECT 1 AS order_id").unwrap();

        // Two YAML files that both declare a not_null test on orders.order_id
        let yaml_a = models_dir.join("a_schema.yml");
        let yaml_b = sub_dir.join("b_schema.yml");
        let yaml_content = r#"
models:
  - name: orders
    columns:
      - name: order_id
        data_tests:
          - not_null
"#;
        fs::write(&yaml_a, yaml_content).unwrap();
        fs::write(&yaml_b, yaml_content).unwrap();

        // Build with files in forward order
        let files_fwd = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/orders.sql")],
            yaml_files: vec![yaml_a.clone(), yaml_b.clone()],
            ..Default::default()
        };
        let graph_fwd =
            build_graph(&project_dir, &files_fwd, None, true, false, &HashMap::new()).unwrap();

        // Build with files in reverse order
        let files_rev = DiscoveredFiles {
            model_sql_files: vec![project_dir.join("models/orders.sql")],
            yaml_files: vec![yaml_b, yaml_a],
            ..Default::default()
        };
        let graph_rev =
            build_graph(&project_dir, &files_rev, None, true, false, &HashMap::new()).unwrap();

        // Both should produce the same set of test unique_ids
        let mut ids_fwd: Vec<String> = graph_fwd
            .node_indices()
            .filter(|&i| graph_fwd[i].node_type == NodeType::Test)
            .map(|i| graph_fwd[i].unique_id.clone())
            .collect();
        ids_fwd.sort();

        let mut ids_rev: Vec<String> = graph_rev
            .node_indices()
            .filter(|&i| graph_rev[i].node_type == NodeType::Test)
            .map(|i| graph_rev[i].unique_id.clone())
            .collect();
        ids_rev.sort();

        assert_eq!(ids_fwd, ids_rev);
        assert_eq!(ids_fwd.len(), 2);
        assert!(ids_fwd.contains(&"test.not_null.orders.order_id".to_string()));
        assert!(ids_fwd.contains(&"test.not_null.orders.order_id_2".to_string()));
    }
}
