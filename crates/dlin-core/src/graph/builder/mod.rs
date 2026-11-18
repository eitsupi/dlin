use anyhow::Result;
use petgraph::stable_graph::NodeIndex;
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

use std::path::PathBuf;

use path_slash::PathExt as _;

use crate::graph::dependency::{
    DependencyAnalysis, DependencyReason, ExtractionSite, InputFailure, InputKind, InputLocation,
    InputStage, NodeCallSite, NodeDependencyReason, NotAssessedScope, ProjectGap,
    relative_display_path,
};
use crate::graph::types::{ExposureInfo, OwnerInfo};
use crate::parser::columns::extract_select_columns;
use crate::parser::discovery::{DiscoveredFiles, SkippedInput};
use crate::parser::jinja::interp::{DependencyExtraction, MacroLibrary};
use crate::parser::project::sql_file_stem;
use crate::parser::sql::recovery::{dependency_call_count, literal_calls};
use crate::parser::sql::{RefCall, SourceCall};
use crate::parser::yaml_schema::{
    ExposureDefinition, MetricDefinition, ModelDefinition, SavedQueryDefinition, SchemaFile,
    SemanticModelDefinition, SnapshotDefinition, TestDefinition, parse_schema_file,
};

/// Parse the project's macro files, recording the files
/// that cannot be read or parsed.
fn load_macro_library(files: &DiscoveredFiles, skipped: &mut Vec<SkippedInput>) -> MacroLibrary {
    let mut sources: Vec<(PathBuf, String)> = Vec::new();
    for path in &files.macro_sql_files {
        let content = std::fs::read(path)
            .map_err(|e| InputFailure::from_io_error(&e))
            .and_then(|bytes| String::from_utf8(bytes).map_err(|_| InputFailure::InvalidUtf8));
        match content {
            Ok(content) => sources.push((path.clone(), content)),
            Err(failure) => skipped.push(SkippedInput {
                path: path.clone(),
                kind: InputKind::Macro,
                stage: InputStage::Read,
                failure,
            }),
        }
    }
    let library = MacroLibrary::new(&sources);
    skipped.extend(library.unparsed.iter().map(|path| SkippedInput {
        path: path.clone(),
        kind: InputKind::Macro,
        stage: InputStage::Parse,
        failure: InputFailure::ParseError,
    }));
    library
}

/// Extract the dependencies and config of one SQL file.
fn extract_sql(
    content: &str,
    library: &MacroLibrary,
    vars: &HashMap<String, serde_json::Value>,
) -> DependencyExtraction {
    crate::parser::jinja::interp::extract_dependencies(content, library, vars)
}

use super::types::*;

/// Shared state threaded through the build_graph helper functions
struct GraphBuilder {
    graph: LineageGraph,
    node_map: HashMap<String, NodeIndex>,
    /// The project's name, which a `ref()` may name as its package.
    project_name: String,
    /// The project directory that reported paths are relative to.
    project_dir: PathBuf,
}

impl GraphBuilder {
    fn new(project_dir: &Path, project_name: &str) -> Self {
        Self {
            graph: LineageGraph::new(),
            node_map: HashMap::new(),
            project_name: project_name.to_string(),
            project_dir: project_dir.to_path_buf(),
        }
    }

    /// `path` relative to the project directory, with `/` separators, as in
    /// the nodes' `file_path`. A path that is already relative is kept.
    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.project_dir)
            .unwrap_or(path)
            .to_slash_lossy()
            .into_owned()
    }

    /// Add reasons to a node's dependency analysis.
    fn add_reasons(
        &mut self,
        idx: NodeIndex,
        reasons: impl IntoIterator<Item = NodeDependencyReason>,
    ) {
        self.graph[idx].dependency_analysis.extend(reasons);
    }

    /// Resolve a `ref()` of the node at `owner` and add its edge, recording
    /// why the target is uncertain. A target that does not exist becomes a
    /// phantom node, as before.
    fn link_ref(
        &mut self,
        owner: NodeIndex,
        call: &RefCall,
        at: NodeCallSite,
        origin: &Path,
        edge_type: EdgeType,
    ) {
        let mut reasons = Vec::new();
        if let Some(package) = &call.package
            && *package != self.project_name
        {
            reasons.push(DependencyReason::UnsupportedRefPackage {
                package: package.clone(),
                name: call.name.clone(),
                at: at.clone(),
            });
        }
        if call.version.is_none() {
            let candidates = ref_candidates(&call.name, &self.node_map, &self.graph);
            if candidates.len() > 1 {
                reasons.push(DependencyReason::AmbiguousRef {
                    name: call.name.clone(),
                    candidates,
                    at: at.clone(),
                });
            }
        }
        let dep_idx = self.get_or_create_phantom_ref(&call.name, call.version.clone(), origin);
        if self.graph[dep_idx].node_type == NodeType::Phantom {
            reasons.push(DependencyReason::UnresolvedRef {
                name: call.name.clone(),
                package: call.package.clone(),
                version: call.version.clone(),
                at,
            });
        }
        self.graph
            .add_edge(dep_idx, owner, EdgeData::direct(edge_type));
        self.add_reasons(owner, reasons);
    }

    /// Resolve a `source()` of the node at `owner` and add its edge, like
    /// [`Self::link_ref`].
    fn link_source(
        &mut self,
        owner: NodeIndex,
        call: &SourceCall,
        at: NodeCallSite,
        origin: &Path,
        edge_type: EdgeType,
    ) {
        let dep_idx =
            self.get_or_create_phantom_source(&call.source_name, &call.table_name, origin);
        if self.graph[dep_idx].node_type == NodeType::Phantom {
            self.add_reasons(
                owner,
                [DependencyReason::UnresolvedSource {
                    source: call.source_name.clone(),
                    table: call.table_name.clone(),
                    at,
                }],
            );
        }
        self.graph
            .add_edge(dep_idx, owner, EdgeData::direct(edge_type));
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
                dependency_analysis: DependencyAnalysis::not_assessed(
                    NotAssessedScope::PhantomDependencies,
                ),
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
            dependency_analysis: DependencyAnalysis::not_assessed(
                NotAssessedScope::PhantomDependencies,
            ),
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
            dependency_analysis: DependencyAnalysis::not_assessed(
                NotAssessedScope::PhantomDependencies,
            ),
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

mod project_yaml;

/// Extract the file stem as a string, defaulting to "unknown"
fn file_stem_str(path: &Path) -> String {
    sql_file_stem(path)
}

/// Dependencies already extracted from model SQL files, so that
/// `process_sql_edges` does not extract them again.
type ModelDependencies = HashMap<PathBuf, DependencyExtraction>;

/// Result of parallel extraction for a single model SQL file
struct ModelExtraction {
    sql_path: PathBuf,
    model_name: String,
    extraction: Option<DependencyExtraction>,
    columns: Vec<String>,
}

/// Create nodes for model SQL files (with duplicate detection).
/// Returns the extracted refs/sources (for `process_sql_edges`).
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
    model_meta: &HashMap<String, project_yaml::YamlModelMeta>,
    vars: &HashMap<String, serde_json::Value>,
    stem_to_versioned: &HashMap<String, (String, String)>,
    library: &MacroLibrary,
) -> ModelDependencies {
    // Parallel phase: read and extract the files concurrently.
    let extractions: Vec<ModelExtraction> = files
        .model_sql_files
        .par_iter()
        .map(|sql_path| {
            let sql_bytes = std::fs::read(sql_path).ok();
            let sql_content = sql_bytes
                .as_deref()
                .and_then(|bytes| std::str::from_utf8(bytes).ok());
            ModelExtraction {
                sql_path: sql_path.clone(),
                model_name: file_stem_str(sql_path),
                extraction: sql_content.map(|content| extract_sql(content, library, vars)),
                columns: sql_content.map(extract_select_columns).unwrap_or_default(),
            }
        })
        .collect();

    // Sequential phase: insert nodes into the graph
    let mut model_name_paths: HashMap<String, std::path::PathBuf> = HashMap::new();
    let mut dependencies: ModelDependencies = HashMap::new();

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

        let sql_config = match me.extraction {
            Some(ext) => {
                let config = ext.config.clone();
                dependencies.insert(me.sql_path.clone(), ext);
                config
            }
            None => Default::default(),
        };

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
            // Filled in by `process_sql_edges` once its dependencies are
            // resolved.
            dependency_analysis: DependencyAnalysis::new([]),
        });
    }

    dependencies
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
            // A seed has no dependencies. A snapshot's are added by
            // `process_sql_edges`.
            dependency_analysis: DependencyAnalysis::new([]),
        });
    }
}

/// Parse SQL files for ref()/source() calls and add edges.
/// `model_dependencies` contains the refs/sources already extracted from
/// model files (by `process_model_files`).
/// `stem_to_versioned` is used to locate versioned model nodes by SQL file
/// stem without relying on node_map aliases (which may point to a different
/// version when `defined_in` uses a base-model name).
#[allow(clippy::too_many_arguments)]
fn process_sql_edges(
    gb: &mut GraphBuilder,
    files: &DiscoveredFiles,
    project_dir: &Path,
    model_dependencies: &ModelDependencies,
    vars: &HashMap<String, serde_json::Value>,
    stem_to_versioned: &HashMap<String, (String, String)>,
    library: &MacroLibrary,
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
                // Filled in below once its dependencies are resolved.
                dependency_analysis: DependencyAnalysis::new([]),
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

        // Model files were extracted already; extract the others here.
        let owned;
        let extraction = match model_dependencies.get(*sql_path) {
            Some(extraction) => extraction,
            None => {
                let content = read_file(sql_path)?;
                owned = extract_sql(&content, library, vars);
                &owned
            }
        };

        let owner_id = gb.graph[current_idx].unique_id.clone();
        let path = gb.display_path(sql_path);
        let at = |site: &ExtractionSite| site.clone().at_node(owner_id.clone(), path.clone());
        let reasons: Vec<_> = extraction
            .reasons
            .iter()
            .map(|reason| reason.clone().map_site(|site| at(&site)))
            .collect();
        gb.add_reasons(current_idx, reasons);

        // Use EdgeType::Test when the target node is a test, so all test
        // relationships render with consistent edge labels/styles.
        let is_test = *file_type == "test";
        let (ref_edge, source_edge) = if is_test {
            (EdgeType::Test, EdgeType::Test)
        } else {
            (EdgeType::Ref, EdgeType::Source)
        };
        for (call, site) in extraction.refs.iter().zip(&extraction.ref_sites) {
            gb.link_ref(current_idx, call, at(site), sql_path, ref_edge);
        }
        for (call, site) in extraction.sources.iter().zip(&extraction.source_sites) {
            gb.link_source(current_idx, call, at(site), sql_path, source_edge);
        }
    }

    Ok(())
}

/// A dependency written as text in YAML, such as an exposure's `depends_on`
/// entry or a snapshot's `relation`.
enum DependencyExpression {
    Ref(RefCall),
    Source(SourceCall),
    /// Not a single `ref()` or `source()` call with literal arguments. The
    /// function called, when one is recognised.
    Unsupported(Option<String>),
}

/// Parse a YAML dependency expression such as `ref('orders')`.
fn parse_dependency_expression(text: &str) -> DependencyExpression {
    let text = text.trim();
    let function = text
        .split_once('(')
        .map(|(name, _)| name.trim())
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        })
        .map(str::to_string);
    let (mut refs, mut sources) = literal_calls(&format!("{{{{ {text} }}}}"));
    match (function.as_deref(), refs.len(), sources.len()) {
        (Some("ref"), 1, 0) => DependencyExpression::Ref(refs.remove(0)),
        (Some("source"), 0, 1) => DependencyExpression::Source(sources.remove(0)),
        _ => DependencyExpression::Unsupported(function),
    }
}

/// Create exposure nodes and edges to their dependencies. An exposure does
/// not create phantom nodes: a target that does not exist is a reason.
fn process_exposures(gb: &mut GraphBuilder, exposures: &[(ExposureDefinition, PathBuf)]) {
    for (exposure, yaml_path) in exposures {
        let unique_id = format!("exposure.{}", exposure.name);
        let idx = gb.add_node(NodeData {
            unique_id: unique_id.clone(),
            label: exposure.name.clone(),
            node_type: NodeType::Exposure,
            file_path: Some(yaml_path.clone()),
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
            // Filled in below once its dependencies are resolved.
            dependency_analysis: DependencyAnalysis::new([]),
        });

        let path = gb.display_path(yaml_path);
        for dep in &exposure.depends_on {
            let at = NodeCallSite {
                unique_id: unique_id.clone(),
                path: path.clone(),
                line: None,
                expr: Some(dep.trim().to_string()),
            };
            let mut reasons = Vec::new();
            let target = match parse_dependency_expression(dep) {
                DependencyExpression::Ref(call) => {
                    if let Some(package) = &call.package
                        && *package != gb.project_name
                    {
                        reasons.push(DependencyReason::UnsupportedRefPackage {
                            package: package.clone(),
                            name: call.name.clone(),
                            at: at.clone(),
                        });
                    }
                    let (dep_id, name) = match &call.version {
                        Some(v) => (
                            format!("model.{}.v{}", call.name, v),
                            format!("{}.v{}", call.name, v),
                        ),
                        None => {
                            let candidates = ref_candidates(&call.name, &gb.node_map, &gb.graph);
                            if candidates.len() > 1 {
                                reasons.push(DependencyReason::AmbiguousRef {
                                    name: call.name.clone(),
                                    candidates,
                                    at: at.clone(),
                                });
                            }
                            (resolve_ref(&call.name, &gb.node_map), call.name.clone())
                        }
                    };
                    Some((dep_id, name))
                }
                DependencyExpression::Source(call) => Some((
                    format!("source.{}.{}", call.source_name, call.table_name),
                    format!("{}.{}", call.source_name, call.table_name),
                )),
                DependencyExpression::Unsupported(function) => {
                    reasons.push(DependencyReason::UnsupportedDependencyExpression {
                        function,
                        at: at.clone(),
                    });
                    None
                }
            };
            if let Some((dep_id, name)) = target {
                match gb.node_map.get(&dep_id).copied() {
                    Some(dep_idx) => {
                        // An edge to a phantom node is kept, as for SQL
                        // references, but it is not a known dependency.
                        if gb.graph[dep_idx].node_type == NodeType::Phantom {
                            reasons.push(DependencyReason::DependencyTargetMissing { name, at });
                        }
                        gb.graph
                            .add_edge(dep_idx, idx, EdgeData::direct(EdgeType::Exposure));
                    }
                    None => reasons.push(DependencyReason::DependencyTargetMissing { name, at }),
                }
            }
            gb.add_reasons(idx, reasons);
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

/// Add a generic test node to the graph and connect it to the parent and to
/// the `ref()`/`source()` calls in its arguments, such as the `to` of a
/// `relationships` test.
fn add_generic_test_node(
    gb: &mut GraphBuilder,
    parent_idx: NodeIndex,
    unique_id: String,
    label: String,
    yaml_path: &Path,
    test_def: &TestDefinition,
) {
    let idx = gb.add_node(NodeData {
        unique_id: unique_id.clone(),
        label,
        node_type: NodeType::Test,
        file_path: Some(yaml_path.to_path_buf()),
        description: None,
        materialization: None,
        tags: vec![],
        columns: vec![],
        exposure: None,
        aliases: vec![],
        // Filled in below once its dependencies are resolved.
        dependency_analysis: DependencyAnalysis::new([]),
    });
    gb.graph
        .add_edge(parent_idx, idx, EdgeData::direct(EdgeType::Test));

    let TestDefinition::Complex(value) = test_def else {
        return;
    };
    let mut strings = Vec::new();
    collect_strings(value, &mut strings);
    let path = gb.display_path(yaml_path);
    for text in strings {
        let template = if text.contains("{{") || text.contains("{%") {
            text.to_string()
        } else {
            format!("{{{{ {text} }}}}")
        };
        let calls = dependency_call_count(&template);
        if calls == 0 {
            continue;
        }
        let at = NodeCallSite {
            unique_id: unique_id.clone(),
            path: path.clone(),
            line: None,
            expr: Some(text.trim().to_string()),
        };
        let (refs, sources) = literal_calls(&template);
        if refs.len() + sources.len() < calls {
            // The text mentions a dependency function that is not a call
            // with literal arguments, alone or next to literal calls.
            gb.add_reasons(
                idx,
                [DependencyReason::UnsupportedDependencyExpression {
                    function: None,
                    at: at.clone(),
                }],
            );
        }
        for call in &refs {
            gb.link_ref(idx, call, at.clone(), yaml_path, EdgeType::Test);
        }
        for call in &sources {
            gb.link_source(idx, call, at.clone(), yaml_path, EdgeType::Test);
        }
    }
}

/// Every string in a YAML value, depth first.
fn collect_strings<'a>(value: &'a serde_json::Value, strings: &mut Vec<&'a str>) {
    match value {
        serde_json::Value::String(text) => strings.push(text),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, strings);
            }
        }
        serde_json::Value::Object(map) => {
            for item in map.values() {
                collect_strings(item, strings);
            }
        }
        _ => {}
    }
}

/// Create test nodes for YAML-declared generic tests (not_null, unique, etc.)
/// and connect them to their parent model/source nodes.
fn process_generic_tests(gb: &mut GraphBuilder, schemas: &[(SchemaFile, PathBuf)]) {
    for (schema, yaml_path) in schemas {
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
                add_generic_test_node(gb, parent_idx, unique_id, label, yaml_path, test_def);
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
                    add_generic_test_node(gb, parent_idx, unique_id, label, yaml_path, test_def);
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
                        add_generic_test_node(
                            gb, parent_idx, unique_id, label, yaml_path, test_def,
                        );
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
            // Filled in by pass 2 once its dependencies are resolved.
            dependency_analysis: DependencyAnalysis::new([]),
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
            let at = NodeCallSite {
                unique_id: unique_id.clone(),
                path: gb.display_path(yaml_path),
                line: None,
                expr: Some(relation.trim().to_string()),
            };
            match parse_dependency_expression(relation) {
                DependencyExpression::Ref(call) => {
                    gb.link_ref(snap_idx, &call, at, yaml_path, EdgeType::Ref);
                }
                DependencyExpression::Source(call) => {
                    gb.link_source(snap_idx, &call, at, yaml_path, EdgeType::Source);
                }
                DependencyExpression::Unsupported(function) => gb.add_reasons(
                    snap_idx,
                    [DependencyReason::UnsupportedDependencyExpression { function, at }],
                ),
            }
        }
    }
}

mod semantic_layer;

/// The result of building a lineage graph from SQL files.
#[derive(Debug)]
pub struct GraphBuild {
    pub graph: LineageGraph,
    /// Gaps that cannot be attributed to a node, sorted and deduplicated.
    pub project_gaps: Vec<ProjectGap>,
}

/// Build the lineage graph from discovered files.
///
/// `project_name` is the `name` in `dbt_project.yml`; a `ref()` naming it as
/// its package refers to the project itself.
pub fn build_graph(
    project_dir: &Path,
    project_name: &str,
    files: &DiscoveredFiles,
    vars: &HashMap<String, serde_json::Value>,
) -> Result<LineageGraph> {
    build_graph_report(project_dir, project_name, files, vars).map(|build| build.graph)
}

/// Like [`build_graph`], also returning the gaps found while building.
pub fn build_graph_report(
    project_dir: &Path,
    project_name: &str,
    files: &DiscoveredFiles,
    vars: &HashMap<String, serde_json::Value>,
) -> Result<GraphBuild> {
    let mut gb = GraphBuilder::new(project_dir, project_name);
    let mut skipped = files.skipped_inputs.clone();
    let library = load_macro_library(files, &mut skipped);

    let yaml_result = project_yaml::process_yaml_files(&mut gb, files, project_dir, &mut skipped)?;
    let model_dependencies = process_model_files(
        &mut gb,
        files,
        project_dir,
        &yaml_result.model_meta,
        vars,
        &yaml_result.stem_to_versioned,
        &library,
    );
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
        &model_dependencies,
        vars,
        &yaml_result.stem_to_versioned,
        &library,
    )?;
    process_exposures(&mut gb, &yaml_result.exposures);
    process_generic_tests(&mut gb, &yaml_result.schemas);
    semantic_layer::process_semantic_layer(
        &mut gb,
        &yaml_result.semantic_models,
        &yaml_result.metrics,
        &yaml_result.saved_queries,
    );

    Ok(GraphBuild {
        graph: gb.graph,
        project_gaps: project_gaps(skipped, project_dir),
    })
}

/// Turn skipped inputs into project gaps with project-relative paths.
fn project_gaps(skipped: Vec<SkippedInput>, project_dir: &Path) -> Vec<ProjectGap> {
    let base = crate::input::normalize_path(project_dir);
    let mut gaps: Vec<ProjectGap> = skipped
        .into_iter()
        .map(|input| ProjectGap::InputSkipped {
            location: InputLocation {
                path: relative_display_path(&input.path, &base),
                line: None,
            },
            kind: input.kind,
            stage: input.stage,
            failure: input.failure,
        })
        .collect();
    gaps.sort();
    gaps.dedup();
    gaps
}

/// Try to resolve a ref name to a node unique_id
/// The distinct nodes an unversioned `ref(name)` can mean, in the order
/// [`resolve_ref`] prefers them.
fn ref_candidates(
    name: &str,
    node_map: &HashMap<String, NodeIndex>,
    graph: &LineageGraph,
) -> Vec<String> {
    let mut candidates: Vec<String> = Vec::new();
    for prefix in ["model", "seed", "snapshot"] {
        if let Some(&idx) = node_map.get(&format!("{prefix}.{name}")) {
            let node = &graph[idx];
            if node.node_type != NodeType::Phantom && !candidates.contains(&node.unique_id) {
                candidates.push(node.unique_id.clone());
            }
        }
    }
    candidates
}

fn resolve_ref(name: &str, node_map: &HashMap<String, NodeIndex>) -> String {
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
    model_id
}

fn parse_exposure_ref(dep: &str) -> Option<(String, Option<String>)> {
    let dep = dep.trim();
    if dep.starts_with("ref(") {
        let wrapped = format!("{{{{ {} }}}}", dep);
        literal_calls(&wrapped)
            .0
            .into_iter()
            .next()
            .filter(|r| r.package.is_none())
            .map(|r| (r.name, r.version))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
