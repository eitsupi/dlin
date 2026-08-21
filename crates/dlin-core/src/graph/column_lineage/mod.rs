use std::collections::{BTreeSet, HashSet};
use std::path::Path;

use crate::parser::manifest::Manifest;

mod backend;
mod cache;
mod cross_model;
mod impact;
mod schema;
#[cfg(test)]
mod tests;
mod types;

use backend::{
    Backend, BackendColumnOutcome, BackendErrorKind, BackendSource, LineageBackend, LineageRequest,
    OutputColumnRequest, OutputOrdinal, PolyglotBackend, ResolutionState,
    require_single_lineage_statement,
};
pub use backend::{
    CatalogSnapshot, DlinDialect, check_sql_parses, debug_parse_sql_ast_debug,
    debug_parse_sql_json, debug_trace_column_json,
};
pub use cache::ColumnLineageCache;
pub use cross_model::{
    compute_cross_model_column_lineage, compute_cross_model_column_lineage_with_manifest_path,
};
pub use impact::{
    ColumnImpactReport, ImpactedColumn, compute_column_impact,
    compute_column_impact_with_manifest_path,
};
use schema::{build_yaml_schema_for_node, compute_manifest_columns_hash};
pub use types::{
    ColumnLineageEntry, ColumnLineageError, ColumnLineageErrorKind, ColumnSource,
    ModelColumnLineage, TransformationType,
};

/// Compute column-level lineage for a model.
///
/// Takes the manifest and a model name (short label like "orders"),
/// and returns the column lineage for that model.
pub fn compute_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DlinDialect,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    compute_column_lineage_with_manifest_path(manifest, model_name, dialect, None, cache)
}

pub fn compute_column_lineage_with_manifest_path(
    manifest: &Manifest,
    model_name: &str,
    dialect: DlinDialect,
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

    let catalog = schema::build_schema_from_manifest(manifest, node, dialect);

    let column_names: Vec<String> = {
        let mut names: HashSet<String> = node.columns.keys().cloned().collect();
        let yaml_schema = build_yaml_schema_for_node(manifest, node);
        names.extend(backend::polyglot::infer_output_columns(
            compiled_code,
            dialect,
            yaml_schema.as_ref(),
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

    let total = column_names.len();

    let outputs: Vec<OutputColumnRequest> = column_names
        .iter()
        .enumerate()
        .map(|(i, name)| OutputColumnRequest {
            ordinal: OutputOrdinal(i),
            name: name.clone(),
        })
        .collect();
    // Duplicate detection is a backend capability that mod.rs's own caller never
    // triggers: `column_names` above is built from a `HashSet`, so it never contains
    // duplicate names.
    let duplicate_output_names: BTreeSet<String> = BTreeSet::new();
    let request = LineageRequest {
        sql: compiled_code,
        dialect,
        catalog: catalog.as_ref(),
        outputs: &outputs,
        duplicate_output_names: &duplicate_output_names,
    };

    let backend = Backend::Polyglot(PolyglotBackend::new());
    let analysis = match backend.analyze(&request) {
        Ok(analysis) => analysis,
        Err(e) => {
            return ModelColumnLineage {
                model: display_name.to_string(),
                traced_columns: 0,
                total_columns: column_names.len(),
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ParseFailure,
                    what: format!("failed to parse SQL for '{}'", display_name),
                    why: Some(e.message),
                    hint: Some(
                        "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set".to_string(),
                    ),
                }],
            };
        }
    };

    let statement = match require_single_lineage_statement(analysis) {
        Ok(statement) => statement,
        Err(e) => {
            return ModelColumnLineage {
                model: display_name.to_string(),
                traced_columns: 0,
                total_columns: column_names.len(),
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ParseFailure,
                    what: format!("failed to parse SQL for '{}'", display_name),
                    why: Some(e.message),
                    hint: Some(
                        "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set".to_string(),
                    ),
                }],
            };
        }
    };

    let has_star_columns = statement.has_unresolved_stars;

    let mut columns = Vec::new();
    let mut errors = Vec::new();
    for (col_name, outcome) in column_names.iter().zip(statement.columns) {
        match outcome {
            BackendColumnOutcome::Resolved(result) => columns.push(ColumnLineageEntry {
                column: col_name.clone(),
                transformation: result.transformation,
                sources: result
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
            }),
            BackendColumnOutcome::Failed(failure) => {
                let hint = (has_star_columns
                    && matches!(
                        failure.error.kind,
                        BackendErrorKind::ColumnResolution {
                            state: ResolutionState::NotFound
                        }
                    ))
                .then(|| {
                    "column may be introduced via SELECT * that could not be expanded; \
                     define upstream columns in the model YAML to enable full resolution"
                        .to_string()
                });
                errors.push(ColumnLineageError {
                    kind: ColumnLineageErrorKind::ColumnNotFound,
                    what: format!("column '{}': {}", col_name, failure.error.message),
                    why: None,
                    hint,
                });
            }
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
