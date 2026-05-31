use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use polyglot_sql::DialectType;
use serde::Serialize;

use crate::parser::manifest::Manifest;

use super::{
    ColumnLineageCache, ColumnLineageError, ColumnLineageErrorKind, TransformationType,
    find_model_by_name,
};

#[derive(Debug, Serialize)]
pub struct ColumnImpactReport {
    pub model: String,
    pub column: String,
    pub impacted_columns: Vec<ImpactedColumn>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ColumnLineageError>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ImpactedColumn {
    pub unique_id: String,
    pub model: String,
    pub column: String,
    pub transformation: TransformationType,
    /// Full hop path from source model to this column, ordered source → this model.
    /// Each entry is (model_name, column_name, transformation). The last entry always
    /// matches (self.model, self.column, self.transformation).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<(String, String, TransformationType)>,
}

type ImpactPath = Vec<(String, String, TransformationType)>;
type NodeKey = (String, String);

#[derive(Debug, Clone)]
struct PathNode {
    hop: (String, String, TransformationType),
    parent: Option<Rc<PathNode>>,
}

#[derive(Debug, Clone)]
struct VisitedNode {
    key: NodeKey,
    parent: Option<Rc<VisitedNode>>,
}

type ImpactQueueItem = (String, String, Option<Rc<PathNode>>, Rc<VisitedNode>);

fn visited_contains(mut cursor: Option<&Rc<VisitedNode>>, key: &NodeKey) -> bool {
    while let Some(node) = cursor {
        if &node.key == key {
            return true;
        }
        cursor = node.parent.as_ref();
    }
    false
}

fn materialize_path(mut cursor: Option<&Rc<PathNode>>) -> ImpactPath {
    let mut path = Vec::new();
    while let Some(node) = cursor {
        path.push(node.hop.clone());
        cursor = node.parent.as_ref();
    }
    path.reverse();
    path
}

pub fn compute_column_impact(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DialectType,
    cache: &mut ColumnLineageCache,
) -> ColumnImpactReport {
    compute_column_impact_with_manifest_path(
        manifest,
        model_name,
        column_name,
        dialect,
        None,
        cache,
    )
}

pub fn compute_column_impact_with_manifest_path(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DialectType,
    manifest_path: Option<&Path>,
    cache: &mut ColumnLineageCache,
) -> ColumnImpactReport {
    let initial_node = match find_model_by_name(manifest, model_name) {
        Some(n) => n,
        None => {
            return ColumnImpactReport {
                model: model_name.to_string(),
                column: column_name.to_string(),
                impacted_columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ModelNotFound,
                    what: format!("model '{}' not found in manifest", model_name),
                    why: None,
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                }],
            };
        }
    };

    let downstream_map = build_downstream_model_map(manifest);

    let mut impacted = Vec::new();
    let mut errors = Vec::new();
    let initial_uid = initial_node.unique_id.clone();
    let mut lineage_cache: HashMap<String, super::ModelColumnLineage> = HashMap::new();

    // Each queue entry carries its own path-local set of visited (uid, col) pairs.
    // This allows a node to be processed once per unique path leading to it, while
    // still preventing cycles within any single path.
    let initial_visited = Rc::new(VisitedNode {
        key: (initial_uid.clone(), column_name.to_string()),
        parent: None,
    });
    let mut queue: Vec<ImpactQueueItem> =
        vec![(initial_uid, column_name.to_string(), None, initial_visited)];

    while let Some((source_uid, source_column, current_path, visited_nodes)) = queue.pop() {
        let source_name = manifest
            .nodes
            .get(&source_uid)
            .map(|n| n.name.as_str())
            .unwrap_or(source_uid.as_str());

        let dependents = match downstream_map.get(&source_uid) {
            Some(deps) => deps,
            None => continue,
        };

        for dep_uid in dependents {
            let dep_node = match manifest.nodes.get(dep_uid) {
                Some(n) => n,
                None => continue,
            };
            let dep_name = &dep_node.name;

            let lineage = lineage_cache.entry(dep_uid.clone()).or_insert_with(|| {
                super::compute_column_lineage_with_manifest_path(
                    manifest,
                    dep_uid,
                    dialect,
                    manifest_path,
                    cache,
                )
            });

            let mut found_on_path = false;

            for entry in &lineage.columns {
                let target_key = (dep_uid.clone(), entry.column.clone());
                if visited_contains(Some(&visited_nodes), &target_key) {
                    continue;
                }

                let references_source = entry.sources.iter().any(|s| {
                    let table_matches =
                        s.table == source_name || normalize_table_name(&s.table) == source_name;
                    table_matches && s.column == source_column
                });

                if references_source {
                    found_on_path = true;
                    let next_path = Rc::new(PathNode {
                        hop: (
                            dep_name.clone(),
                            entry.column.clone(),
                            entry.transformation.clone(),
                        ),
                        parent: current_path.clone(),
                    });

                    impacted.push(ImpactedColumn {
                        unique_id: dep_uid.clone(),
                        model: dep_name.clone(),
                        column: entry.column.clone(),
                        transformation: entry.transformation.clone(),
                        model_path: materialize_path(Some(&next_path)),
                    });

                    let next_visited = Rc::new(VisitedNode {
                        key: target_key,
                        parent: Some(visited_nodes.clone()),
                    });
                    queue.push((
                        dep_uid.clone(),
                        entry.column.clone(),
                        Some(next_path),
                        next_visited,
                    ));
                }
            }

            // ColumnNotFound errors are per-column noise: only include them when the model
            // has at least one column confirmed on the impact path. Model-level failures
            // (ParseFailure, NoCompiledCode, ColumnInferenceFailed) mean the model could
            // not be analyzed at all, so they are always propagated — the path through
            // that model may be incomplete and the user needs to know.
            for err in &lineage.errors {
                let is_column_level = err.kind == ColumnLineageErrorKind::ColumnNotFound;
                if (!is_column_level || found_on_path) && !errors.contains(err) {
                    errors.push(err.clone());
                }
            }
        }
    }

    impacted.sort_by(|a, b| {
        (
            &a.unique_id,
            &a.model,
            &a.column,
            &a.transformation,
            &a.model_path,
        )
            .cmp(&(
                &b.unique_id,
                &b.model,
                &b.column,
                &b.transformation,
                &b.model_path,
            ))
    });
    impacted.dedup();

    ColumnImpactReport {
        model: model_name.to_string(),
        column: column_name.to_string(),
        impacted_columns: impacted,
        errors,
    }
}

pub(super) fn build_downstream_model_map(manifest: &Manifest) -> HashMap<String, Vec<String>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    for node in manifest.nodes.values() {
        if node.resource_type != "model" {
            continue;
        }
        for dep_id in &node.depends_on.nodes {
            if let Some(dep_node) = manifest.nodes.get(dep_id)
                && dep_node.resource_type == "model"
            {
                map.entry(dep_node.unique_id.clone())
                    .or_default()
                    .push(node.unique_id.clone());
            }
        }
    }

    map
}

fn normalize_table_name(table: &str) -> String {
    let stripped: String = table.chars().filter(|c| *c != '"' && *c != '`').collect();
    stripped.rsplit('.').next().unwrap_or(&stripped).to_string()
}
