use std::collections::{HashMap, HashSet};

use polyglot_sql::DialectType;

use crate::parser::manifest::Manifest;

use super::{
    ColumnLineageCache, ColumnLineageError, ColumnSource, ModelColumnLineage, TransformationType,
    compute_column_lineage, find_model_by_name,
};

pub fn compute_cross_model_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DialectType,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    let mut ctx = CrossModelContext {
        manifest,
        dialect,
        in_memory_cache: HashMap::new(),
        computing: HashSet::new(),
    };
    ctx.computing.insert(model_name.to_string());
    compute_cross_model_inner(model_name, &mut ctx, cache)
}

struct CrossModelContext<'a> {
    manifest: &'a Manifest,
    dialect: DialectType,
    in_memory_cache: HashMap<String, ModelColumnLineage>,
    computing: HashSet<String>,
}

fn compute_cross_model_inner(
    model_name: &str,
    ctx: &mut CrossModelContext<'_>,
    disk_cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    let mut result = compute_column_lineage(ctx.manifest, model_name, ctx.dialect, disk_cache);
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

    let node = find_model_by_name(manifest, model_name);
    let node = match node {
        Some(n) => n,
        None => return map,
    };

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if dep_node.resource_type != "model" {
                continue;
            }
            map.insert(dep_node.name.clone(), dep_node.name.clone());
            let fq = make_fq_table_name(
                dep_node.database.as_deref(),
                dep_node.schema.as_deref(),
                &dep_node.name,
            );
            if fq != dep_node.name {
                map.insert(fq, dep_node.name.clone());
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
    let model_name = upstream_models
        .get(&source.table)
        .or_else(|| {
            let normalized = normalize_table_name(&source.table);
            upstream_models.get(&normalized)
        })
        .cloned();

    let model_name = match model_name {
        Some(name) => {
            let pair = (name.clone(), source.column.clone());
            if visited.contains(&pair) {
                let mut leaf = source.clone();
                leaf.model_path = current_path.to_vec();
                resolved.push(leaf);
                return;
            }
            visited.insert(pair);
            name
        }
        None => {
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
            return;
        }
    };

    if !ctx.in_memory_cache.contains_key(&model_name) {
        if ctx.computing.contains(&model_name) {
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
            return;
        }
        ctx.computing.insert(model_name.clone());
        let upstream_result = compute_cross_model_inner(&model_name, ctx, disk_cache);
        ctx.in_memory_cache
            .insert(model_name.clone(), upstream_result);
    }
    let upstream_result = ctx.in_memory_cache.get(&model_name).unwrap();

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
            model_name.clone(),
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
        let on_demand =
            compute_single_column_lineage(ctx.manifest, &model_name, &source.column, ctx.dialect);
        // Use Unknown transformation for on-demand columns since we lack the col_entry
        let mut extended_path = current_path.to_vec();
        extended_path.push((
            model_name.clone(),
            source.column.clone(),
            TransformationType::Unknown,
        ));
        if on_demand.is_empty() {
            // Leaf: column at model_name has no traceable sources — don't self-include in path.
            let mut leaf = source.clone();
            leaf.model_path = current_path.to_vec();
            resolved.push(leaf);
        } else {
            let further_upstream = build_upstream_model_names(ctx.manifest, &model_name);
            for s in &on_demand {
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
    dialect: DialectType,
) -> Vec<ColumnSource> {
    let node = find_model_by_name(manifest, model_name);

    let node = match node {
        Some(n) => n,
        None => return vec![],
    };

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => return vec![],
    };

    let ctx = match super::prepare_lineage_context(compiled_code, manifest, node, dialect) {
        Ok(ctx) => ctx,
        Err(_) => return vec![],
    };

    super::run_column_lineage(column_name, &ctx)
        .map(|r| r.sources)
        .unwrap_or_default()
}

fn make_fq_table_name(database: Option<&str>, schema: Option<&str>, name: &str) -> String {
    match (database, schema) {
        (Some(db), Some(s)) => format!("{}.{}.{}", db, s, name),
        (None, Some(s)) => format!("{}.{}", s, name),
        _ => name.to_string(),
    }
}
