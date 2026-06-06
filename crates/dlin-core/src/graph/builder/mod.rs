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
    RefCall, SourceCall, extract_all_with_vars, extract_refs_and_sources_with_vars, extract_sources,
};
use crate::parser::yaml_schema::{
    ExposureDefinition, MetricDefinition, ModelDefinition, SavedQueryDefinition, SchemaFile,
    SemanticModelDefinition, SnapshotDefinition, parse_schema_file,
};

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

    /// Register a node_map alias: `from` → same NodeIndex as `to`.
    /// When `to` is not yet in the map (e.g. its SQL file is missing), a Phantom
    /// node is created for `to` so that unversioned lookups still resolve to the
    /// intended versioned unique_id rather than falling back to a generic phantom.
    /// Also records `from` in the target node's `aliases` list so that the alias
    /// survives after `build_graph` discards the node_map.
    fn add_alias(&mut self, from: String, to: &str) {
        let idx = if let Some(&existing) = self.node_map.get(to) {
            existing
        } else {
            let label = to.strip_prefix("model.").unwrap_or(to).to_string();
            self.add_node(NodeData {
                unique_id: to.to_string(),
                label,
                node_type: NodeType::Phantom,
                file_path: None,
                description: None,
                materialization: None,
                tags: vec![],
                columns: vec![],
                exposure: None,
                aliases: vec![],
            })
        };
        if let std::collections::hash_map::Entry::Vacant(e) = self.node_map.entry(from.clone()) {
            e.insert(idx);
            self.graph[idx].aliases.push(from);
        }
    }

    /// Get or create a phantom ref node, returning its index.
    /// When `version` is `Some(N)`, resolves only `model.{name}.v{N}` — never
    /// falls back to the unversioned alias so that version-pinned refs don't
    /// silently link to the wrong version.
    fn get_or_create_phantom_ref(
        &mut self,
        ref_name: &str,
        version: Option<String>,
        sql_path: &Path,
    ) -> NodeIndex {
        let dep_id = if let Some(ref v) = version {
            format!("model.{}.v{}", ref_name, v)
        } else {
            resolve_ref(ref_name, &self.node_map)
        };
        if let Some(&idx) = self.node_map.get(&dep_id) {
            return idx;
        }
        let display_name = match version.as_deref() {
            Some(v) => format!("{}.v{}", ref_name, v),
            None => ref_name.to_string(),
        };
        crate::warn!(
            "unresolved ref '{}' in {}",
            display_name,
            sql_path.display()
        );
        let phantom_id = match version.as_deref() {
            Some(v) => format!("model.{}.v{}", ref_name, v),
            None => format!("model.{}", ref_name),
        };
        self.add_node(NodeData {
            unique_id: phantom_id,
            label: display_name,
            node_type: NodeType::Phantom,
            file_path: None,
            description: None,
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
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
            aliases: vec![],
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
                aliases: vec![],
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
struct YamlParseResult {
    model_meta: HashMap<String, YamlModelMeta>,
    exposures: Vec<ExposureDefinition>,
    /// Each SchemaFile paired with its YAML file relative path (for test node file_path).
    schemas: Vec<(SchemaFile, PathBuf)>,
    /// Maps SQL file stems to (versioned_unique_id, base_model_name).
    stem_to_versioned: HashMap<String, (String, String)>,
    /// Maps unversioned model IDs to the latest-version unique ID.
    version_aliases: HashMap<String, String>,
    /// YAML-only snapshot defs with their yaml file (relative) path.
    snapshot_defs: Vec<(SnapshotDefinition, PathBuf)>,
    semantic_models: Vec<SemanticModelDefinition>,
    metrics: Vec<MetricDefinition>,
    saved_queries: Vec<SavedQueryDefinition>,
}

/// Build version maps for a single versioned model definition.
/// Returns entries to add to `stem_to_versioned` and `version_aliases`.
#[allow(clippy::type_complexity)]
fn build_version_maps(
    model_def: &ModelDefinition,
) -> (Vec<(String, (String, String))>, Option<(String, String)>) {
    if model_def.versions.is_empty() {
        return (vec![], None);
    }
    let name = &model_def.name;
    let mut stem_entries: Vec<(String, (String, String))> = Vec::new();
    for vspec in &model_def.versions {
        let v_str = vspec.v_str();
        let stem = vspec.sql_stem(name);
        let unique_id = format!("model.{}.v{}", name, v_str);
        stem_entries.push((stem, (unique_id, name.clone())));
    }
    let alias = model_def.resolved_latest_version_str().map(|lv_str| {
        let unversioned_id = format!("model.{}", name);
        let latest_versioned_id = format!("model.{}.v{}", name, lv_str);
        (unversioned_id, latest_versioned_id)
    });
    (stem_entries, alias)
}

/// Parse YAML schema files: create source nodes, collect model metadata, exposures,
/// and parsed schemas (for generic test extraction).
fn process_yaml_files(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
) -> Result<YamlParseResult> {
    let mut model_meta: HashMap<String, YamlModelMeta> = HashMap::new();
    let mut exposures: Vec<ExposureDefinition> = Vec::new();
    let mut schemas: Vec<(SchemaFile, PathBuf)> = Vec::new();
    let mut stem_to_versioned: HashMap<String, (String, String)> = HashMap::new();
    let mut version_aliases: HashMap<String, String> = HashMap::new();
    let mut snapshot_defs: Vec<(SnapshotDefinition, PathBuf)> = Vec::new();
    let mut semantic_models: Vec<SemanticModelDefinition> = Vec::new();
    let mut metrics: Vec<MetricDefinition> = Vec::new();
    let mut saved_queries: Vec<SavedQueryDefinition> = Vec::new();

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

            // Collect versioned model maps
            let (stem_entries, alias) = build_version_maps(model_def);
            for (stem, entry) in stem_entries {
                stem_to_versioned.entry(stem).or_insert(entry);
            }
            if let Some((unversioned_id, latest_versioned_id)) = alias {
                version_aliases
                    .entry(unversioned_id)
                    .or_insert(latest_versioned_id);
            }
        }

        exposures.extend(schema.exposures.iter().cloned());
        semantic_models.extend(schema.semantic_models.iter().cloned());
        metrics.extend(schema.metrics.iter().cloned());
        saved_queries.extend(schema.saved_queries.iter().cloned());

        let relative_path = yaml_path
            .strip_prefix(project_dir)
            .unwrap_or(yaml_path)
            .to_path_buf();
        for snap_def in &schema.snapshots {
            snapshot_defs.push((snap_def.clone(), relative_path.clone()));
        }
        schemas.push((schema, relative_path));
    }

    Ok(YamlParseResult {
        model_meta,
        exposures,
        schemas,
        stem_to_versioned,
        version_aliases,
        snapshot_defs,
        semantic_models,
        metrics,
        saved_queries,
    })
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
///
/// `stem_to_versioned` maps SQL file stems to `(versioned_unique_id, base_model_name)`.
/// When a file stem is present in this map, the node is registered under the
/// versioned unique_id (e.g. `model.my_model.v2`) and model metadata is looked up
/// under the base name.
#[allow(clippy::too_many_arguments)]
fn process_model_files(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
    model_meta: &HashMap<String, YamlModelMeta>,
    macro_prefix: &str,
    disk_cache: &mut cache::ExtractionCache,
    vars: &HashMap<String, serde_json::Value>,
    stem_to_versioned: &HashMap<String, (String, String)>,
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

        // Resolve versioned unique_id and base model name for YAML metadata lookup.
        let (unique_id, label, meta_key) =
            if let Some((versioned_id, base_name)) = stem_to_versioned.get(&me.model_name) {
                (
                    versioned_id.clone(),
                    // label: e.g. "my_model.v2"
                    versioned_id
                        .strip_prefix("model.")
                        .unwrap_or(versioned_id)
                        .to_string(),
                    base_name.as_str(),
                )
            } else {
                let uid = format!("model.{}", me.model_name);
                (uid, me.model_name.clone(), me.model_name.as_str())
            };

        let yaml_meta = model_meta.get(meta_key);

        let materialization = sql_config
            .materialized
            .or_else(|| yaml_meta.and_then(|m| m.materialization.clone()));

        let mut tags = sql_config.tags;
        if let Some(meta) = yaml_meta {
            tags.extend(meta.tags.clone());
        }
        tags.sort();
        tags.dedup();

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
            label,
            node_type: NodeType::Model,
            file_path: Some(relative_path),
            description: yaml_meta.and_then(|m| m.description.clone()),
            materialization,
            tags,
            columns,
            exposure: None,
            aliases: vec![],
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
            aliases: vec![],
        });
    }
}

/// Parse SQL files for ref()/source() calls and add edges.
/// `extraction_cache` contains pre-extracted refs/sources for model files
/// (from `process_model_files`) to avoid redundant minijinja renders.
/// `stem_to_versioned` is used to locate versioned model nodes by SQL file
/// stem without relying on node_map aliases (which may point to a different
/// version when `defined_in` uses a base-model name).
fn process_sql_edges(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
    macro_prefix: &str,
    extraction_cache: &ExtractionCache,
    vars: &HashMap<String, serde_json::Value>,
    stem_to_versioned: &HashMap<String, (String, String)>,
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
                label: node_name.clone(),
                node_type: NodeType::Test,
                file_path: Some(relative_path),
                description: None,
                materialization: None,
                tags: vec![],
                columns: vec![],
                exposure: None,
                aliases: vec![],
            });
        }

        // For model files, resolve via stem_to_versioned to get the exact versioned
        // node ID. This avoids the collision where node_map["model.my_model"] already
        // points to the latest-version alias rather than the file being processed.
        let current_idx = if *file_type == "model" {
            let lookup_id = stem_to_versioned
                .get(&node_name)
                .map(|(versioned_id, _)| versioned_id.as_str())
                .unwrap_or(&node_unique_id);
            match gb.node_map.get(lookup_id) {
                Some(&idx) => idx,
                None => continue,
            }
        } else {
            match gb.node_map.get(&node_unique_id) {
                Some(&idx) => idx,
                None => continue,
            }
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
            let dep_idx =
                gb.get_or_create_phantom_ref(&ref_call.name, ref_call.version.clone(), sql_path);
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
            aliases: vec![],
        });

        for dep in &exposure.depends_on {
            if let Some((model_name, version)) = parse_exposure_ref(dep) {
                let dep_id = if let Some(ref v) = version {
                    format!("model.{}.v{}", model_name, v)
                } else {
                    resolve_ref(&model_name, &gb.node_map)
                };
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
        aliases: vec![],
    });
    gb.graph
        .add_edge(parent_idx, idx, EdgeData::direct(EdgeType::Test));
}

/// Create test nodes for YAML-declared generic tests (not_null, unique, etc.)
/// and connect them to their parent model/source nodes.
fn process_generic_tests(gb: &mut GraphBuilder, schemas: &[(SchemaFile, PathBuf)]) {
    for (schema, yaml_path) in schemas {
        let file_path = Some(yaml_path.clone());

        // Model-level generic tests.
        // For versioned models, `model.{name}` is an alias to the latest version node,
        // so the lookup still works without special-casing.
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

/// Register YAML-only snapshot nodes (dbt v1.9+) and add their upstream edges.
/// Snapshots already registered from a SQL file are skipped; for those without a
/// matching SQL file the node is created here and linked to the upstream model via
/// the `relation: ref('...')` field.
///
/// Two-pass approach: first register all nodes so that forward references between
/// YAML-only snapshots resolve correctly, then add edges.
fn process_yaml_snapshot_nodes(
    gb: &mut GraphBuilder,
    snapshot_defs: &[(SnapshotDefinition, PathBuf)],
) {
    // Pass 1: register all YAML-only snapshot nodes.
    // `gb.node_map.contains_key` guards against both SQL-registered nodes (already
    // present before this pass) and duplicate YAML definitions (added earlier in
    // this same pass), so each unique_id is added at most once.
    let mut yaml_registered = std::collections::HashSet::<String>::new();
    for (snap_def, yaml_path) in snapshot_defs {
        let unique_id = format!("snapshot.{}", snap_def.name);
        if gb.node_map.contains_key(&unique_id) {
            continue;
        }
        gb.add_node(NodeData {
            unique_id: unique_id.clone(),
            label: snap_def.name.clone(),
            node_type: NodeType::Snapshot,
            file_path: Some(yaml_path.clone()),
            description: snap_def.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
        yaml_registered.insert(unique_id);
    }

    // Pass 2: resolve upstream edges now that all snapshot nodes exist.
    for (snap_def, yaml_path) in snapshot_defs {
        let unique_id = format!("snapshot.{}", snap_def.name);
        if !yaml_registered.remove(&unique_id) {
            continue;
        }
        let Some(&snap_idx) = gb.node_map.get(&unique_id) else {
            continue;
        };
        if let Some(relation) = &snap_def.relation {
            if let Some((source_name, table_name)) = parse_relation_source(relation) {
                let dep_idx =
                    gb.get_or_create_phantom_source(&source_name, &table_name, yaml_path.as_path());
                gb.graph
                    .add_edge(dep_idx, snap_idx, EdgeData::direct(EdgeType::Source));
            } else if let Some((model_name, version)) = parse_exposure_ref(relation) {
                let dep_idx =
                    gb.get_or_create_phantom_ref(&model_name, version, yaml_path.as_path());
                gb.graph
                    .add_edge(dep_idx, snap_idx, EdgeData::direct(EdgeType::Ref));
            }
        }
    }
}

/// Build and connect semantic layer nodes (semantic_models, metrics, saved_queries).
///
/// Pass ordering:
///   1. Register all semantic_model nodes (enables forward references between metrics).
///   2. Build measure → semantic_model_name map.
///   3. Add model → semantic_model edges via `model: ref('...')`.
///   4. Register all metric nodes.
///   5. Add semantic_model → metric edges (Simple metrics via measure lookup).
///   6. Add metric → metric edges (Ratio/Derived/Conversion/Cumulative).
///   7. Register all saved_query nodes and add metric → saved_query edges.
fn process_semantic_layer(
    gb: &mut GraphBuilder,
    semantic_models: &[SemanticModelDefinition],
    metrics: &[MetricDefinition],
    saved_queries: &[SavedQueryDefinition],
) {
    // Dummy path for phantom-node warnings from parse_exposure_ref
    let dummy_path = std::path::Path::new("<semantic_layer>");

    // Pass 1: register semantic_model nodes
    for sm in semantic_models {
        let unique_id = format!("semantic_model.{}", sm.name);
        if gb.node_map.contains_key(&unique_id) {
            continue;
        }
        gb.add_node(NodeData {
            unique_id,
            label: sm.name.clone(),
            node_type: NodeType::SemanticModel,
            file_path: None,
            description: sm.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
    }

    // Pass 2: build measure_name → semantic_model_name map and add model edges
    let mut measure_to_sem: HashMap<String, String> = HashMap::new();
    for sm in semantic_models {
        let sem_id = format!("semantic_model.{}", sm.name);
        let Some(&sem_idx) = gb.node_map.get(&sem_id) else {
            continue;
        };
        for measure in &sm.measures {
            measure_to_sem
                .entry(measure.name.clone())
                .or_insert_with(|| sm.name.clone());
        }
        // Add edge: model_node → semantic_model_node
        if let Some(model_ref) = &sm.model
            && let Some((model_name, version)) = parse_exposure_ref(model_ref)
        {
            let dep_id = if let Some(ref v) = version {
                format!("model.{}.v{}", model_name, v)
            } else {
                resolve_ref(&model_name, &gb.node_map)
            };
            if let Some(&dep_idx) = gb.node_map.get(&dep_id) {
                gb.graph
                    .add_edge(dep_idx, sem_idx, EdgeData::direct(EdgeType::Ref));
            }
        }
    }

    // Pass 3: register metric nodes
    for metric in metrics {
        let unique_id = format!("metric.{}", metric.name);
        if gb.node_map.contains_key(&unique_id) {
            continue;
        }
        gb.add_node(NodeData {
            unique_id,
            label: metric.name.clone(),
            node_type: NodeType::Metric,
            file_path: None,
            description: metric.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
    }

    // Pass 4: add semantic_model → metric and metric → metric edges
    for metric in metrics {
        let metric_id = format!("metric.{}", metric.name);
        let Some(&metric_idx) = gb.node_map.get(&metric_id) else {
            continue;
        };
        // Simple metric: link via measure lookup
        if let Some(measure_name) = metric.measure_ref()
            && let Some(sem_name) = measure_to_sem.get(measure_name)
        {
            let sem_id = format!("semantic_model.{}", sem_name);
            if let Some(&sem_idx) = gb.node_map.get(&sem_id) {
                gb.graph
                    .add_edge(sem_idx, metric_idx, EdgeData::direct(EdgeType::Ref));
            }
        }
        // Ratio/Derived/Conversion/Cumulative: link to upstream metrics
        for dep_metric_name in metric.metric_refs() {
            let dep_id = format!("metric.{}", dep_metric_name);
            let dep_idx = if let Some(&idx) = gb.node_map.get(&dep_id) {
                idx
            } else {
                crate::warn!(
                    "unresolved metric ref '{}' from metric '{}'",
                    dep_metric_name,
                    metric.name
                );
                gb.add_node(NodeData {
                    unique_id: dep_id,
                    label: dep_metric_name.to_string(),
                    node_type: NodeType::Phantom,
                    file_path: None,
                    description: None,
                    materialization: None,
                    tags: vec![],
                    columns: vec![],
                    exposure: None,
                    aliases: vec![],
                })
            };
            gb.graph
                .add_edge(dep_idx, metric_idx, EdgeData::direct(EdgeType::Ref));
        }
    }

    // Pass 5: register saved_query nodes and add metric → saved_query edges
    for sq in saved_queries {
        let sq_id = format!("saved_query.{}", sq.name);
        if gb.node_map.contains_key(&sq_id) {
            continue;
        }
        let sq_idx = gb.add_node(NodeData {
            unique_id: sq_id.clone(),
            label: sq.name.clone(),
            node_type: NodeType::SavedQuery,
            file_path: None,
            description: sq.description.clone(),
            materialization: None,
            tags: vec![],
            columns: vec![],
            exposure: None,
            aliases: vec![],
        });
        if let Some(qp) = &sq.query_params {
            for metric_name in &qp.metrics {
                let metric_dep_id = format!("metric.{}", metric_name);
                let dep_idx = if let Some(&idx) = gb.node_map.get(&metric_dep_id) {
                    idx
                } else {
                    crate::warn!(
                        "unresolved metric ref '{}' in saved_query '{}'",
                        metric_name,
                        sq.name
                    );
                    gb.add_node(NodeData {
                        unique_id: metric_dep_id,
                        label: metric_name.clone(),
                        node_type: NodeType::Phantom,
                        file_path: None,
                        description: None,
                        materialization: None,
                        tags: vec![],
                        columns: vec![],
                        exposure: None,
                        aliases: vec![],
                    })
                };
                gb.graph
                    .add_edge(dep_idx, sq_idx, EdgeData::direct(EdgeType::Ref));
            }
        }
    }

    // Suppress unused variable warning for dummy_path (it's a sentinel for future use)
    let _ = dummy_path;
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

    let yaml_result = process_yaml_files(&mut gb, files, project_dir)?;
    let extraction_cache = process_model_files(
        &mut gb,
        files,
        project_dir,
        &yaml_result.model_meta,
        &macro_prefix,
        &mut disk_cache,
        vars,
        &yaml_result.stem_to_versioned,
    );
    // Register "model.name" aliases to the latest versioned node so that
    // unversioned ref('name') calls resolve correctly.
    for (unversioned_id, latest_versioned_id) in &yaml_result.version_aliases {
        gb.add_alias(unversioned_id.clone(), latest_versioned_id);
    }
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
    process_yaml_snapshot_nodes(&mut gb, &yaml_result.snapshot_defs);
    process_sql_edges(
        &mut gb,
        files,
        project_dir,
        &macro_prefix,
        &extraction_cache,
        vars,
        &yaml_result.stem_to_versioned,
    )?;
    process_exposures(&mut gb, &yaml_result.exposures);
    process_generic_tests(&mut gb, &yaml_result.schemas);
    process_semantic_layer(
        &mut gb,
        &yaml_result.semantic_models,
        &yaml_result.metrics,
        &yaml_result.saved_queries,
    );

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

/// Parse a source('schema', 'table') string (no Jinja delimiters).
/// Returns (source_name, table_name), or None if the string is not a source() call.
fn parse_relation_source(relation: &str) -> Option<(String, String)> {
    let wrapped = format!("{{{{ {} }}}}", relation.trim());
    extract_sources(&wrapped)
        .into_iter()
        .next()
        .map(|s| (s.source_name, s.table_name))
}

/// Parse a ref('name') or ref('name', version=N) string from exposure depends_on.
/// Returns (model_name, optional_version). Source refs and package-qualified refs
/// (cross-package) return None — cross-package exposure links are not yet supported.
fn parse_exposure_ref(dep: &str) -> Option<(String, Option<String>)> {
    let dep = dep.trim();
    if dep.starts_with("ref(") {
        // Wrap in {{ }} so we can reuse the SQL regex extractor.
        let wrapped = format!("{{{{ {} }}}}", dep);
        let refs = crate::parser::sql::extract_refs(&wrapped);
        // Skip package-qualified refs — cross-package resolution is not supported here.
        refs.into_iter()
            .next()
            .filter(|r| r.package.is_none())
            .map(|r| (r.name, r.version))
    } else {
        // source() and other strings: no edge
        None
    }
}

#[cfg(test)]
mod tests;
