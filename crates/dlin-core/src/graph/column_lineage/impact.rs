use std::collections::{HashMap, HashSet};

use polyglot_sql::DialectType;
use serde::Serialize;

use crate::parser::manifest::Manifest;

use super::{
    ColumnLineageCache, ColumnLineageError, ColumnLineageErrorKind, TransformationType,
    compute_column_lineage, find_model_by_name,
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<String>,
}

pub fn compute_column_impact(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DialectType,
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
    let mut visited: HashSet<(String, String)> = HashSet::new();
    let initial_uid = initial_node.unique_id.clone();
    visited.insert((initial_uid.clone(), column_name.to_string()));
    let mut lineage_cache: HashMap<String, super::ModelColumnLineage> = HashMap::new();

    let mut queue: Vec<(String, String, Vec<String>)> =
        vec![(initial_uid, column_name.to_string(), vec![])];

    while let Some((source_uid, source_column, current_path)) = queue.pop() {
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

            let lineage = lineage_cache
                .entry(dep_uid.clone())
                .or_insert_with(|| compute_column_lineage(manifest, dep_uid, dialect, cache));
            for err in &lineage.errors {
                if !errors.contains(err) {
                    errors.push(err.clone());
                }
            }

            for entry in &lineage.columns {
                let pair = (dep_uid.clone(), entry.column.clone());
                if visited.contains(&pair) {
                    continue;
                }

                let references_source = entry.sources.iter().any(|s| {
                    let table_matches =
                        s.table == source_name || normalize_table_name(&s.table) == source_name;
                    table_matches && s.column == source_column
                });

                if references_source {
                    visited.insert(pair);

                    let mut path = current_path.clone();
                    path.push(dep_name.clone());

                    impacted.push(ImpactedColumn {
                        unique_id: dep_uid.clone(),
                        model: dep_name.clone(),
                        column: entry.column.clone(),
                        transformation: entry.transformation.clone(),
                        model_path: path.clone(),
                    });

                    queue.push((dep_uid.clone(), entry.column.clone(), path));
                }
            }
        }
    }

    impacted.sort_by(|a, b| (&a.unique_id, &a.column).cmp(&(&b.unique_id, &b.column)));
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
