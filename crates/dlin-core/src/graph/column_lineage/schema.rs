use std::collections::HashSet;

use super::backend::{
    DlinDialect, LineageBackend, OutputDiscoveryRequest, catalog::CatalogSnapshot,
};
use super::relation::RelationRef;

use crate::parser::cache::hash_str;
use crate::parser::manifest::Manifest;

pub(super) fn build_schema_from_manifest(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: DlinDialect,
    backend: &dyn LineageBackend,
) -> Option<CatalogSnapshot> {
    let mut schema = CatalogSnapshot::new();
    let mut has_entries = false;

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let col_names = resolve_node_columns(dep_node, manifest, dialect, backend);
            if !col_names.is_empty() {
                has_entries |= register_model_relation(
                    &mut schema,
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    dep_node.relation_name(),
                    &col_names,
                );
            }
            continue;
        }

        if let Some(dep_source) = manifest.sources.get(dep_id)
            && !dep_source.columns.is_empty()
        {
            let mut source_col_names: Vec<&String> = dep_source.columns.keys().collect();
            source_col_names.sort_unstable();
            let physical_identifier = dep_source.identifier.as_deref().unwrap_or(&dep_source.name);
            has_entries |= register_source_relation(
                &mut schema,
                dep_source.database.as_deref(),
                dep_source.schema.as_deref(),
                physical_identifier,
                &dep_source.source_name,
                &dep_source.name,
                source_col_names.iter().cloned().cloned().collect(),
            );
        }
    }

    if has_entries { Some(schema) } else { None }
}

fn resolve_node_columns(
    dep_node: &crate::parser::manifest::ManifestNode,
    manifest: &Manifest,
    dialect: DlinDialect,
    backend: &dyn LineageBackend,
) -> Vec<String> {
    let yaml_cols: HashSet<String> = dep_node.columns.keys().cloned().collect();
    let inferred_cols: HashSet<String> = dep_node
        .compiled_code
        .as_ref()
        .map(|code| {
            let schema = build_yaml_schema_for_node(manifest, dep_node);
            let request = OutputDiscoveryRequest {
                sql: code,
                dialect,
                catalog: schema.as_ref(),
            };
            super::discover_named_output_columns(backend, &request)
        })
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut merged: Vec<String> = yaml_cols.union(&inferred_cols).cloned().collect();
    merged.sort_unstable();
    merged
}

pub(super) fn build_yaml_schema_for_node(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> Option<CatalogSnapshot> {
    let mut schema = CatalogSnapshot::new();
    let mut has_entries = false;

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if !dep_node.columns.is_empty() {
                let mut node_col_names: Vec<&String> = dep_node.columns.keys().collect();
                node_col_names.sort_unstable();
                has_entries |= register_model_relation(
                    &mut schema,
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    dep_node.relation_name(),
                    &node_col_names.iter().cloned().cloned().collect::<Vec<_>>(),
                );
            }
            continue;
        }

        if let Some(dep_source) = manifest.sources.get(dep_id)
            && !dep_source.columns.is_empty()
        {
            let mut source_col_names: Vec<&String> = dep_source.columns.keys().collect();
            source_col_names.sort_unstable();
            let physical_identifier = dep_source.identifier.as_deref().unwrap_or(&dep_source.name);
            has_entries |= register_source_relation(
                &mut schema,
                dep_source.database.as_deref(),
                dep_source.schema.as_deref(),
                physical_identifier,
                &dep_source.source_name,
                &dep_source.name,
                source_col_names.iter().cloned().cloned().collect(),
            );
        }
    }

    if has_entries { Some(schema) } else { None }
}

fn register_model_relation(
    schema: &mut CatalogSnapshot,
    database: Option<&str>,
    schema_name: Option<&str>,
    relation_name: &str,
    columns: &[String],
) -> bool {
    let relation = RelationRef::from_manifest(database, schema_name, relation_name);
    let was_present = schema.contains_relation(&relation);
    schema.add_relation(relation.clone(), relation.render(), columns.iter().cloned());
    if relation.qualification_len() > 1 {
        schema.add_alias(
            RelationRef::from_manifest(None, None, relation_name),
            relation,
        );
    }
    !was_present
}

fn register_source_relation(
    schema: &mut CatalogSnapshot,
    database: Option<&str>,
    schema_name: Option<&str>,
    physical_identifier: &str,
    source_name: &str,
    logical_name: &str,
    columns: Vec<String>,
) -> bool {
    let physical = RelationRef::from_manifest(database, schema_name, physical_identifier);
    let was_present = schema.contains_relation(&physical);
    schema.add_relation(physical.clone(), physical.render(), columns);

    let aliases = [
        RelationRef::from_manifest(None, schema_name, physical_identifier),
        RelationRef::from_manifest(None, None, physical_identifier),
        RelationRef::from_manifest(None, None, logical_name),
        RelationRef::from_manifest(None, Some(source_name), logical_name),
    ];
    for alias in aliases {
        if alias != physical {
            schema.add_alias(alias, physical.clone());
        }
    }
    !was_present
}

pub(super) fn compute_manifest_columns_hash(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> u64 {
    let mut visited: HashSet<String> = HashSet::new();
    hash_node_columns_transitive(manifest, node, &mut visited)
}

fn hash_node_columns_transitive(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    visited: &mut HashSet<String>,
) -> u64 {
    let mut parts: Vec<String> = Vec::new();

    parts.push(format!("relation:{}", node.relation_name()));
    if let Some(database) = &node.database {
        parts.push(format!("db:{}", database));
    }
    if let Some(schema) = &node.schema {
        parts.push(format!("schema:{}", schema));
    }

    let mut own_cols: Vec<&String> = node.columns.keys().collect();
    own_cols.sort();
    for col in own_cols {
        parts.push(col.clone());
    }
    if let Some(code) = &node.compiled_code {
        parts.push(format!("sql:{}", hash_str(code)));
    }
    parts.push("|".to_string());

    let mut dep_ids: Vec<&String> = node.depends_on.nodes.iter().collect();
    dep_ids.sort();
    for dep_id in dep_ids {
        parts.push(dep_id.clone());
        if visited.contains(dep_id) {
            continue;
        }
        visited.insert(dep_id.clone());
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let dep_hash = hash_node_columns_transitive(manifest, dep_node, visited);
            parts.push(format!("node:{}", dep_hash));
        } else if let Some(dep_source) = manifest.sources.get(dep_id) {
            let mut cols: Vec<&String> = dep_source.columns.keys().collect();
            cols.sort();
            for col in cols {
                parts.push(col.clone());
            }
            if let Some(db) = &dep_source.database {
                parts.push(format!("db:{}", db));
            }
            if let Some(s) = &dep_source.schema {
                parts.push(format!("schema:{}", s));
            }
            if let Some(id) = &dep_source.identifier {
                parts.push(format!("id:{}", id));
            }
        }
    }

    hash_str(&parts.join("\0"))
}
