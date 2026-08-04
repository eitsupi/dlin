use std::collections::HashSet;
use std::path::Path;

use polyglot_sql::{ColumnResolutionReason, DialectType, Error};
use rayon::prelude::*;

use crate::parser::manifest::Manifest;

mod cache;
mod cross_model;
mod impact;
mod schema;
mod single_model;
mod star_guard;
#[cfg(test)]
mod tests;
mod types;

pub use cache::ColumnLineageCache;
pub use cross_model::{
    compute_cross_model_column_lineage, compute_cross_model_column_lineage_with_manifest_path,
};
pub use impact::{
    ColumnImpactReport, ImpactedColumn, compute_column_impact,
    compute_column_impact_with_manifest_path,
};
use schema::{build_yaml_schema_for_node, compute_manifest_columns_hash, infer_output_columns};
use single_model::{prepare_lineage_context_with_expr, run_column_lineage};
use star_guard::has_unresolved_stars;
pub use types::{
    ColumnLineageEntry, ColumnLineageError, ColumnLineageErrorKind, ColumnSource,
    ModelColumnLineage, TransformationType,
};

/// Compute column-level lineage for a model using polyglot-sql.
///
/// Takes the manifest and a model name (short label like "orders"),
/// and returns the column lineage for that model.
pub fn compute_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DialectType,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    compute_column_lineage_with_manifest_path(manifest, model_name, dialect, None, cache)
}

pub fn compute_column_lineage_with_manifest_path(
    manifest: &Manifest,
    model_name: &str,
    dialect: DialectType,
    manifest_path: Option<&Path>,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    let node = find_model_by_name(manifest, model_name);

    let node = match node {
        Some(n) => n,
        None => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                traced_columns: 0,
                total_columns: 0,
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ModelNotFound,
                    what: format!("model '{}' not found in manifest", model_name),
                    why: None,
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                }],
            };
        }
    };

    let display_name = node.name.as_str();

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => {
            return ModelColumnLineage {
                model: display_name.to_string(),
                traced_columns: 0,
                total_columns: 0,
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::NoCompiledCode,
                    what: format!("model '{}' has no compiled_code", display_name),
                    why: Some("compiled SQL is required for column lineage analysis".to_string()),
                    hint: Some(
                        "Run `dbt compile` first; use `dlin check-manifest` to verify the manifest is up to date".to_string(),
                    ),
                }],
            };
        }
    };

    let manifest_columns_hash = compute_manifest_columns_hash(manifest, node);
    if let Some(cached) = cache.get(
        model_name,
        compiled_code,
        dialect,
        manifest_path,
        Some(manifest_columns_hash),
    ) {
        return cached.clone();
    }

    let parsed_expr = polyglot_sql::parse_one(compiled_code, dialect).ok();

    let column_names: Vec<String> = {
        let mut names: HashSet<String> = node.columns.keys().cloned().collect();
        let schema = build_yaml_schema_for_node(manifest, node);
        names.extend(infer_output_columns(
            compiled_code,
            dialect,
            schema.as_ref(),
            parsed_expr.as_ref(),
        ));
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort();
        names
    };

    if column_names.is_empty() {
        return ModelColumnLineage {
            model: display_name.to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![ColumnLineageError {
                kind: ColumnLineageErrorKind::ColumnInferenceFailed,
                what: format!("model '{}': could not determine output columns", display_name),
                why: Some("YAML has no columns defined and SQL column inference failed".to_string()),
                hint: Some(
                    "Add column definitions to the model's YAML, or ensure the SQL is parseable by `dlin debug parse-sql`".to_string(),
                ),
            }],
        };
    }

    let ctx = match prepare_lineage_context_with_expr(
        compiled_code,
        manifest,
        node,
        dialect,
        parsed_expr.as_ref(),
    ) {
        Ok(ctx) => ctx,
        Err(e) => {
            return ModelColumnLineage {
                model: display_name.to_string(),
                traced_columns: 0,
                total_columns: column_names.len(),
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ParseFailure,
                    what: format!("failed to parse SQL for '{}'", display_name),
                    why: Some(e),
                    hint: Some(
                        "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set".to_string(),
                    ),
                }],
            };
        }
    };

    let total = column_names.len();
    let has_star_columns = has_unresolved_stars(&ctx.expanded_expr);

    let results: Vec<_> = column_names
        .par_iter()
        .map(|col_name| match run_column_lineage(col_name, &ctx) {
            Ok(result) => Ok(ColumnLineageEntry {
                column: col_name.clone(),
                transformation: result.transformation,
                sources: result.sources,
            }),
            Err(e) => {
                let hint =
                    (has_star_columns && is_indeterminate_column_resolution(&e)).then(|| {
                        "column may be introduced via SELECT * that could not be expanded; \
                     define upstream columns in the model YAML to enable full resolution"
                            .to_string()
                    });
                Err(ColumnLineageError {
                    kind: ColumnLineageErrorKind::ColumnNotFound,
                    what: format!(
                        "column '{}': {}",
                        col_name,
                        single_model::format_lineage_error(&e)
                    ),
                    why: None,
                    hint,
                })
            }
        })
        .collect();

    let mut columns = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(entry) => columns.push(entry),
            Err(e) => errors.push(e),
        }
    }

    let result = ModelColumnLineage {
        model: display_name.to_string(),
        traced_columns: columns.len(),
        total_columns: total,
        columns,
        errors,
    };

    cache.insert(
        model_name,
        compiled_code,
        dialect,
        manifest_columns_hash,
        manifest_path,
        result.clone(),
    );

    result
}

fn is_indeterminate_column_resolution(error: &Error) -> bool {
    matches!(
        error,
        Error::ColumnResolution {
            reason: ColumnResolutionReason::Indeterminate,
            ..
        }
    )
}

pub(super) fn find_model_by_name<'a>(
    manifest: &'a Manifest,
    name: &str,
) -> Option<&'a crate::parser::manifest::ManifestNode> {
    if let Some(node) = manifest.nodes.get(name)
        && node.resource_type == "model"
    {
        return Some(node);
    }
    let suffix = format!(".{}", name);
    let mut matches: Vec<&crate::parser::manifest::ManifestNode> = manifest
        .nodes
        .values()
        .filter(|n| n.resource_type == "model" && n.unique_id.ends_with(&suffix))
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(matches[0]),
        _ => {
            matches.sort_unstable_by(|a, b| a.unique_id.cmp(&b.unique_id));
            let ids: Vec<&str> = matches.iter().map(|n| n.unique_id.as_str()).collect();
            crate::warn!(
                "model name '{}' is ambiguous (matched: {}); using '{}'",
                name,
                ids.join(", "),
                matches[0].unique_id,
            );
            Some(matches[0])
        }
    }
}
