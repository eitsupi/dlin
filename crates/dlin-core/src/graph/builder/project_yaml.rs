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
pub(super) struct YamlModelMeta {
    pub(super) description: Option<String>,
    pub(super) materialization: Option<String>,
    pub(super) tags: Vec<String>,
    pub(super) columns: Vec<String>,
}

/// Result of parsing YAML schema files.
pub(super) struct YamlParseResult {
    pub(super) model_meta: HashMap<String, YamlModelMeta>,
    pub(super) exposures: Vec<ExposureDefinition>,
    /// Each SchemaFile paired with its YAML file relative path (for test node file_path).
    pub(super) schemas: Vec<(SchemaFile, PathBuf)>,
    /// Maps SQL file stems to (versioned_unique_id, base_model_name).
    pub(super) stem_to_versioned: HashMap<String, (String, String)>,
    /// Maps unversioned model IDs to the latest-version unique ID.
    pub(super) version_aliases: HashMap<String, String>,
    /// YAML-only snapshot defs with their yaml file (relative) path.
    pub(super) snapshot_defs: Vec<(SnapshotDefinition, PathBuf)>,
    /// Semantic layer defs paired with the relative YAML path they came from.
    pub(super) semantic_models: Vec<(SemanticModelDefinition, PathBuf)>,
    pub(super) metrics: Vec<(MetricDefinition, PathBuf)>,
    pub(super) saved_queries: Vec<(SavedQueryDefinition, PathBuf)>,
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
pub(super) fn process_yaml_files(
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
    let mut semantic_models: Vec<(SemanticModelDefinition, PathBuf)> = Vec::new();
    let mut metrics: Vec<(MetricDefinition, PathBuf)> = Vec::new();
    let mut saved_queries: Vec<(SavedQueryDefinition, PathBuf)> = Vec::new();

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

        let relative_path = yaml_path
            .strip_prefix(project_dir)
            .unwrap_or(yaml_path)
            .to_path_buf();

        semantic_models.extend(
            schema
                .semantic_models
                .iter()
                .cloned()
                .map(|sm| (sm, relative_path.clone())),
        );
        metrics.extend(
            schema
                .metrics
                .iter()
                .cloned()
                .map(|m| (m, relative_path.clone())),
        );
        saved_queries.extend(
            schema
                .saved_queries
                .iter()
                .cloned()
                .map(|sq| (sq, relative_path.clone())),
        );

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
use super::*;
