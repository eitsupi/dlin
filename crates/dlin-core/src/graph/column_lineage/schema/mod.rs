use std::collections::{HashMap, HashSet};

pub(super) mod context;
pub(super) mod evidence;
mod sql_output;

pub(super) use context::SchemaContext;
pub(super) use evidence::SchemaEvidence;

use crate::parser::cache::hash_str;
use crate::parser::manifest::Manifest;

/// Per-analysis memoization for semantic manifest digests.
///
/// Digests are intentionally kept in memory only. A `ColumnLineageAnalysis`
/// owns one for the lifetime of a manifest analysis session. Persistent cache
/// compatibility is decided separately by the cache file header.
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
    // Absent, `enforced: false` and `enforced: true` give different coverage
    // bases, so the three states stay distinct.
    parts.push(format!(
        "contract_enforced:{:?}",
        node.config
            .contract
            .as_ref()
            .map(|contract| contract.enforced)
    ));
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

/// Append the declared columns in declaration order with their raw `quote`
/// setting. The order is the display order of YAML evidence and decides how a
/// star expands over a YAML-only relation, and an unset `quote` differs from
/// an explicit `false`, so neither is normalized away.
fn append_columns(
    parts: &mut Vec<String>,
    columns: &indexmap::IndexMap<String, crate::parser::manifest::ManifestColumn>,
) {
    for (name, column) in columns {
        parts.push(format!("column:{name}:quote={:?}", column.quote));
    }
}
