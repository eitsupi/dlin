use std::collections::{HashMap, HashSet};

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

/// Per-analysis memoization for semantic manifest digests.
///
/// Digests are intentionally kept in memory only. A `ColumnLineageAnalysis`
/// owns one for the lifetime of a manifest analysis session, while the dlin
/// package version remains the sole persistent cache compatibility boundary.
#[derive(Debug, Default)]
pub(super) struct SemanticDigestCache {
    digests: HashMap<String, u64>,
    computed_nodes: usize,
}

/// The semantic digest of a node and whether it is safe to use as a
/// persistent cache validation key.
///
/// Digests for malformed dependency cycles are still useful for terminating
/// the traversal deterministically, but they must not be persisted. A cycle
/// marker deliberately omits some dependency state, so using such a digest
/// for persistent reuse could turn a changed dependency into a stale hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SemanticDigest {
    pub(super) value: u64,
    pub(super) persistent_cache_safe: bool,
}

impl SemanticDigestCache {
    pub(super) fn digest_for_node(
        &mut self,
        manifest: &Manifest,
        node: &crate::parser::manifest::ManifestNode,
    ) -> SemanticDigest {
        self.digest_for_id(
            manifest,
            &node.unique_id,
            &mut HashSet::new(),
            &mut Vec::new(),
            &mut HashSet::new(),
        )
    }

    fn digest_for_id(
        &mut self,
        manifest: &Manifest,
        id: &str,
        visiting: &mut HashSet<String>,
        path: &mut Vec<String>,
        cycle_nodes: &mut HashSet<String>,
    ) -> SemanticDigest {
        if let Some(digest) = self.digests.get(id) {
            return SemanticDigest {
                value: *digest,
                persistent_cache_safe: true,
            };
        }
        if visiting.contains(id) {
            // dbt DAGs should be acyclic, but malformed manifests must not
            // recurse forever. Mark the complete active cycle, rather than
            // only the back-edge, so every edge within that cycle receives
            // the same deterministic treatment regardless of query order.
            if let Some(start) = path.iter().position(|active| active == id) {
                cycle_nodes.extend(path[start..].iter().cloned());
            }
            return SemanticDigest {
                value: hash_str("column-lineage:cycle"),
                persistent_cache_safe: false,
            };
        }
        visiting.insert(id.to_string());
        path.push(id.to_string());

        let mut parts = Vec::new();
        let mut persistent_cache_safe = true;
        if let Some(node) = manifest.nodes.get(id) {
            parts.push(format!("node:{id}"));
            append_node_local_parts(&mut parts, node);

            let mut dependency_ids = node.depends_on.nodes.iter().collect::<Vec<_>>();
            dependency_ids.sort_unstable();
            for dependency_id in dependency_ids {
                parts.push(format!("dependency:{dependency_id}"));
                if manifest.nodes.contains_key(dependency_id)
                    || manifest.sources.contains_key(dependency_id)
                {
                    let dependency_digest =
                        self.digest_for_id(manifest, dependency_id, visiting, path, cycle_nodes);
                    persistent_cache_safe &= dependency_digest.persistent_cache_safe;
                    if cycle_nodes.contains(id) && cycle_nodes.contains(dependency_id.as_str()) {
                        parts.push(format!("cycle-dependency:{dependency_id}"));
                    } else {
                        parts.push(format!("digest:{}", dependency_digest.value));
                    }
                } else {
                    parts.push("missing-dependency".to_string());
                }
            }
        } else if let Some(source) = manifest.sources.get(id) {
            parts.push(format!("source:{id}"));
            append_source_local_parts(&mut parts, source);
        } else {
            parts.push(format!("missing:{id}"));
        }

        path.pop();
        visiting.remove(id);
        let digest = hash_str(&parts.join("\0"));
        let is_cycle_participant = cycle_nodes.contains(id);
        persistent_cache_safe &= !is_cycle_participant;
        if persistent_cache_safe {
            self.digests.insert(id.to_string(), digest);
        }
        self.computed_nodes += 1;
        SemanticDigest {
            value: digest,
            persistent_cache_safe,
        }
    }

    #[cfg(test)]
    pub(super) fn computed_nodes(&self) -> usize {
        self.computed_nodes
    }
}

/// Compute one model's semantic input digest without retaining memoized state.
/// Callers that analyze multiple models should use one analysis session so
/// shared upstream subtrees are visited once per session.
#[cfg(test)]
pub(super) fn compute_semantic_digest(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> u64 {
    SemanticDigestCache::default()
        .digest_for_node(manifest, node)
        .value
}

fn append_node_local_parts(parts: &mut Vec<String>, node: &crate::parser::manifest::ManifestNode) {
    parts.push(format!("relation:{}", node.relation_name()));
    parts.push(format!("database:{:?}", node.database));
    parts.push(format!("schema:{:?}", node.schema));
    append_columns(parts, &node.columns);
    parts.push(format!("compiled_code:{:?}", node.compiled_code));
}

fn append_source_local_parts(
    parts: &mut Vec<String>,
    source: &crate::parser::manifest::ManifestSource,
) {
    parts.push(format!("name:{}", source.name));
    parts.push(format!("source_name:{}", source.source_name));
    parts.push(format!("identifier:{:?}", source.identifier));
    parts.push(format!("database:{:?}", source.database));
    parts.push(format!("schema:{:?}", source.schema));
    append_columns(parts, &source.columns);
}

fn append_columns(
    parts: &mut Vec<String>,
    columns: &HashMap<String, crate::parser::manifest::ManifestColumn>,
) {
    let mut names = columns.keys().collect::<Vec<_>>();
    names.sort_unstable();
    for name in names {
        parts.push(format!("column:{name}"));
    }
}
