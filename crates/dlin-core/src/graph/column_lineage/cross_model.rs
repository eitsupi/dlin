use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use crate::parser::manifest::Manifest;

use super::backend::{
    AnalysisSlot, Backend, BackendColumnOutcome, BackendSource, DlinDialect, LineageBackend,
    LineageRequest, OutputColumnRequest, PolyglotBackend, normalize_column_outcomes,
    require_single_lineage_statement,
};
use super::schema;
use super::{
    ColumnLineageCache, ColumnLineageError, ColumnSource, ModelColumnLineage, TransformationType,
    find_model_by_name, find_model_by_unique_id,
};

pub fn compute_cross_model_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DlinDialect,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    compute_cross_model_column_lineage_with_manifest_path(
        manifest, model_name, dialect, None, cache,
    )
}

pub fn compute_cross_model_column_lineage_with_manifest_path(
    manifest: &Manifest,
    model_name: &str,
    dialect: DlinDialect,
    manifest_path: Option<&Path>,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    let mut ctx = CrossModelContext {
        manifest,
        dialect,
        manifest_path,
        in_memory_cache: HashMap::new(),
        computing: HashSet::new(),
    };
    ctx.computing.insert(model_name.to_string());
    compute_cross_model_inner(model_name, &mut ctx, cache)
}

struct CrossModelContext<'a> {
    manifest: &'a Manifest,
    dialect: DlinDialect,
    manifest_path: Option<&'a Path>,
    in_memory_cache: HashMap<String, ModelColumnLineage>,
    computing: HashSet<String>,
}

fn compute_cross_model_inner(
    model_name: &str,
    ctx: &mut CrossModelContext<'_>,
    disk_cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    let mut result = super::compute_column_lineage_with_manifest_path(
        ctx.manifest,
        model_name,
        ctx.dialect,
        ctx.manifest_path,
        disk_cache,
    );
    let upstream_models = build_upstream_model_names(ctx.manifest, model_name);

    for entry in &mut result.columns {
        let mut resolved_sources = Vec::new();
        let mut visited: HashSet<(String, String)> = HashSet::new();
        visited.insert((model_name.to_string(), entry.column.clone()));

        for source in &entry.sources {
            resolve_source_recursive(
                source,
                &upstream_models,
                &mut visited,
                &mut resolved_sources,
                &mut result.errors,
                ctx,
                disk_cache,
                &[],
            );
        }

        resolved_sources.sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
        resolved_sources.dedup();
        entry.sources = resolved_sources;
    }

    result
}

fn build_upstream_model_names(manifest: &Manifest, model_name: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    let node = find_model_by_unique_id(manifest, model_name)
        .or_else(|| find_model_by_name(manifest, model_name));
    let node = match node {
        Some(n) => n,
        None => return map,
    };

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if dep_node.resource_type != "model" {
                continue;
            }
            let relation_name = dep_node.relation_name();
            map.insert(relation_name.to_string(), dep_node.unique_id.clone());
            let fq = make_fq_table_name(
                dep_node.database.as_deref(),
                dep_node.schema.as_deref(),
                relation_name,
            );
            if fq != relation_name {
                map.insert(fq, dep_node.unique_id.clone());
            }
        }
    }

    map
}

pub(super) fn normalize_table_name(table: &str) -> String {
    let stripped: String = table.chars().filter(|c| *c != '"' && *c != '`').collect();
    stripped.rsplit('.').next().unwrap_or(&stripped).to_string()
}

#[allow(clippy::too_many_arguments)]
fn resolve_source_recursive(
    source: &ColumnSource,
    upstream_models: &HashMap<String, String>,
    visited: &mut HashSet<(String, String)>,
    resolved: &mut Vec<ColumnSource>,
    errors: &mut Vec<ColumnLineageError>,
    ctx: &mut CrossModelContext<'_>,
    disk_cache: &mut ColumnLineageCache,
    current_path: &[(String, String, TransformationType)],
) {
    let model_unique_id = upstream_models
        .get(&source.table)
        .or_else(|| {
            let normalized = normalize_table_name(&source.table);
            upstream_models.get(&normalized)
        })
        .cloned();

    let model_unique_id = match model_unique_id {
        Some(unique_id) => {
            let pair = (unique_id.clone(), source.column.clone());
            if visited.contains(&pair) {
                let mut leaf = source.clone();
                leaf.model_path = current_path.to_vec();
                resolved.push(leaf);
                return;
            }
            visited.insert(pair);
            unique_id
        }
        None => {
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
            return;
        }
    };

    if !ctx.in_memory_cache.contains_key(&model_unique_id) {
        if ctx.computing.contains(&model_unique_id) {
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
            return;
        }
        ctx.computing.insert(model_unique_id.clone());
        let upstream_result = compute_cross_model_inner(&model_unique_id, ctx, disk_cache);
        ctx.in_memory_cache
            .insert(model_unique_id.clone(), upstream_result);
    }
    let upstream_result = ctx.in_memory_cache.get(&model_unique_id).unwrap();

    for err in &upstream_result.errors {
        if !errors.contains(err) {
            errors.push(err.clone());
        }
    }

    if let Some(col_entry) = upstream_result
        .columns
        .iter()
        .find(|c| c.column == source.column)
    {
        // Build extended_path with the transformation type now that we know it
        let mut extended_path = current_path.to_vec();
        extended_path.push((
            upstream_result.model.clone(),
            source.column.clone(),
            col_entry.transformation.clone(),
        ));

        if col_entry.sources.is_empty() {
            // Leaf: the column exists at model_name but has no further sources.
            // Don't include model_name in model_path since it IS the leaf (avoids self-loop).
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
        } else {
            for s in &col_entry.sources {
                let mut merged = s.clone();
                let mut full_path = extended_path.clone();
                full_path.extend(s.model_path.iter().cloned());
                merged.model_path = full_path;
                resolved.push(merged);
            }
        }
    } else {
        let on_demand = compute_single_column_lineage(
            ctx.manifest,
            &model_unique_id,
            &source.column,
            ctx.dialect,
        );
        let transformation = on_demand
            .as_ref()
            .map_or(TransformationType::Unknown, |(_, t)| t.clone());
        let model_display_name = ctx
            .manifest
            .nodes
            .get(&model_unique_id)
            .map(|node| node.name.clone())
            .unwrap_or_else(|| model_unique_id.clone());
        let mut extended_path = current_path.to_vec();
        extended_path.push((model_display_name, source.column.clone(), transformation));
        let on_demand_sources = on_demand.map(|(sources, _)| sources).unwrap_or_default();
        if on_demand_sources.is_empty() {
            // Leaf: column at model_name has no traceable sources — don't self-include in path.
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
        } else {
            let further_upstream = build_upstream_model_names(ctx.manifest, &model_unique_id);
            for s in &on_demand_sources {
                resolve_source_recursive(
                    s,
                    &further_upstream,
                    visited,
                    resolved,
                    errors,
                    ctx,
                    disk_cache,
                    &extended_path,
                );
            }
        }
    }
}

fn compute_single_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DlinDialect,
) -> Option<(Vec<ColumnSource>, TransformationType)> {
    let node = find_model_by_unique_id(manifest, model_name)
        .or_else(|| find_model_by_name(manifest, model_name))?;
    let compiled_code = node.compiled_code.as_ref()?;
    let backend = Backend::Polyglot(PolyglotBackend::new());
    let catalog = schema::build_schema_from_manifest(manifest, node, dialect, &backend);

    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: column_name.to_string(),
    }];
    let duplicate_output_names = BTreeSet::new();
    let request = LineageRequest {
        sql: compiled_code,
        dialect,
        catalog: catalog.as_ref(),
        outputs: &outputs,
        duplicate_output_names: &duplicate_output_names,
    };

    let analysis = backend.analyze(&request).ok()?;
    let statement = require_single_lineage_statement(analysis).ok()?;
    let outcome = normalize_single_column_outcome(&outputs, statement.columns)?;

    match outcome {
        BackendColumnOutcome::Resolved(result) => Some((
            result
                .sources
                .into_iter()
                .map(|s| match s {
                    BackendSource::Concrete { table, column } => ColumnSource {
                        table,
                        column,
                        model_path: vec![],
                    },
                    other => unreachable!(
                        "the polyglot backend never produces this source variant: {:?}",
                        other
                    ),
                })
                .collect(),
            result.transformation,
        )),
        BackendColumnOutcome::Failed(_) => None,
    }
}

fn normalize_single_column_outcome(
    outputs: &[OutputColumnRequest],
    outcomes: Vec<BackendColumnOutcome>,
) -> Option<BackendColumnOutcome> {
    let (normalized_outcomes, _contract_errors) = normalize_column_outcomes(outputs, outcomes);
    normalized_outcomes.into_iter().next()
}

fn make_fq_table_name(database: Option<&str>, schema: Option<&str>, name: &str) -> String {
    match (database, schema) {
        (Some(db), Some(s)) => format!("{}.{}.{}", db, s, name),
        (None, Some(s)) => format!("{}.{}", s, name),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::column_lineage::backend::{
        BackendColumnFailure, BackendError, BackendErrorKind,
    };
    use crate::graph::column_lineage::backend::{BackendColumnResult, ResolutionState};

    #[test]
    fn cross_model_keeps_requested_outcome_with_unrelated_contract_diagnostic() {
        let outputs = [OutputColumnRequest {
            slot: AnalysisSlot(0),
            name: "alpha".to_string(),
        }];
        let outcome = normalize_single_column_outcome(
            &outputs,
            vec![
                BackendColumnOutcome::Resolved(BackendColumnResult {
                    target: crate::graph::column_lineage::backend::OutputTarget {
                        slot: AnalysisSlot(0),
                        name: "alpha".to_string(),
                    },
                    resolution: ResolutionState::Resolved,
                    transformation: TransformationType::Direct,
                    sources: vec![BackendSource::Concrete {
                        table: "upstream".to_string(),
                        column: "alpha".to_string(),
                    }],
                }),
                BackendColumnOutcome::Failed(BackendColumnFailure {
                    target: crate::graph::column_lineage::backend::OutputTarget {
                        slot: AnalysisSlot(9),
                        name: "unrelated".to_string(),
                    },
                    resolution: ResolutionState::Indeterminate,
                    error: BackendError {
                        kind: BackendErrorKind::Internal,
                        message: "unrelated diagnostic".to_string(),
                    },
                }),
            ],
        );

        assert!(matches!(
            outcome,
            Some(BackendColumnOutcome::Resolved(result))
                if result.target.slot == AnalysisSlot(0)
                    && result.target.name == "alpha"
                    && result.sources == vec![BackendSource::Concrete {
                        table: "upstream".to_string(),
                        column: "alpha".to_string(),
                    }]
        ));
    }
}
