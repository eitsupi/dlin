use crate::parser::manifest::Manifest;
use std::collections::{HashMap, HashSet};

mod backend;
mod cache;
mod cross_model;
mod impact;
mod relation;
mod schema;
#[cfg(test)]
mod tests;
mod types;

use backend::sqllineage::{
    OutputFailureKind, analyze_parsed, analyze_sql, lineage_failure, output_names, parse_sql,
    resolve_statement_output, statement_error,
};
pub use backend::{
    CatalogSnapshot, DialectClassification, DlinDialect, REMOVED_DIALECTS, check_sql_parses,
    debug_parse_sql_ast_debug, debug_parse_sql_json, debug_trace_column_json,
};
pub use cache::ColumnLineageCache;
pub use impact::{ColumnImpactReport, ImpactedColumn};
use schema::{SemanticDigestCache, build_yaml_schema_for_node};
pub use types::{
    ColumnLineageEntry, ColumnLineageError, ColumnLineageErrorKind, ColumnSource,
    ModelColumnLineage, TransformationType,
};
use types::{
    InternalColumnLineageEntry, InternalColumnSource, InternalModelColumnLineage,
    normalize_column_lineage_errors,
};

fn discover_named_output_columns(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
) -> HashSet<String> {
    // Deliberately preserve the previous parse-failure behavior:
    // YAML columns remain usable when discovery cannot parse the SQL.
    // Surfacing discovery parse errors is an error-model change to decide on
    // its own merits, rather than inherit from this refactor.
    let Ok(analysis) = analyze_sql(sql, dialect, catalog) else {
        return HashSet::new();
    };
    let Ok(statement) = analysis.into_single() else {
        return HashSet::new();
    };
    if !statement.kind.carries_lineage() {
        return HashSet::new();
    }
    output_names(&statement).0.into_iter().collect()
}

fn output_failure_state(kind: OutputFailureKind) -> &'static str {
    match kind {
        OutputFailureKind::NotFound => "not_found",
        OutputFailureKind::Ambiguous => "ambiguous",
        OutputFailureKind::Indeterminate => "indeterminate",
    }
}

fn column_resolution_error_with_message(
    column: &str,
    state: &str,
    detail: String,
    has_open_shape: bool,
) -> ColumnLineageError {
    let (kind, hint) = match state {
        "ambiguous" => (ColumnLineageErrorKind::ColumnAmbiguous, None),
        "not_found" => (ColumnLineageErrorKind::ColumnNotFound, None),
        _ => (
            ColumnLineageErrorKind::ColumnIndeterminate,
            has_open_shape.then(|| {
                "column may be introduced via SELECT * that could not be expanded; \
                 define upstream columns in the model YAML to enable full resolution"
                    .to_string()
            }),
        ),
    };
    ColumnLineageError {
        kind,
        column: Some(column.to_string()),
        what: format!("column '{column}': {detail}"),
        why: None,
        hint,
    }
}

fn parse_failure(model: &str, total: usize, reason: String) -> InternalModelColumnLineage {
    InternalModelColumnLineage {
        model: model.to_string(),
        traced_columns: 0,
        total_columns: total,
        columns: vec![],
        errors: vec![ColumnLineageError {
            kind: ColumnLineageErrorKind::ParseFailure,
            column: None,
            what: format!("failed to parse SQL for '{model}'"),
            why: Some(reason),
            hint: Some(
                "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set"
                    .to_string(),
            ),
        }],
    }
}

fn column_inference_failure(model: &str) -> InternalModelColumnLineage {
    InternalModelColumnLineage {
        model: model.to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnInferenceFailed,
            column: None,
            what: format!("model '{model}': could not determine output columns"),
            why: Some("YAML has no columns defined and SQL column inference failed".to_string()),
            hint: Some(
                "Add column definitions to the model's YAML, or ensure the SQL is parseable by `dlin debug parse-sql`".to_string(),
            ),
        }],
    }
}

/// A per-manifest column-lineage analysis session.
///
/// The session owns all in-process memoization. Persistent cache storage is
/// deliberately kept separate, so a disabled cache can still avoid repeated
/// semantic digest work and repeated model analysis within one invocation.
pub struct ColumnLineageAnalysis<'a> {
    pub(super) manifest: &'a Manifest,
    pub(super) dialect: DlinDialect,
    pub(super) cache: &'a mut ColumnLineageCache,
    semantic_digests: SemanticDigestCache,
    lineage_results: HashMap<String, InternalModelColumnLineage>,
}

impl<'a> ColumnLineageAnalysis<'a> {
    pub fn new(
        manifest: &'a Manifest,
        dialect: DlinDialect,
        cache: &'a mut ColumnLineageCache,
    ) -> Self {
        Self {
            manifest,
            dialect,
            cache,
            semantic_digests: SemanticDigestCache::default(),
            lineage_results: HashMap::new(),
        }
    }

    pub fn compute_column_lineage(&mut self, model_name: &str) -> ModelColumnLineage {
        self.compute_column_lineage_internal(model_name)
            .into_public()
    }

    pub fn compute_cross_model_column_lineage(&mut self, model_name: &str) -> ModelColumnLineage {
        cross_model::compute_cross_model_column_lineage(self, model_name).into_public()
    }

    pub fn compute_column_impact(
        &mut self,
        model_name: &str,
        column_name: &str,
    ) -> ColumnImpactReport {
        impact::compute_column_impact(self, model_name, column_name)
    }

    pub(super) fn compute_column_lineage_internal(
        &mut self,
        model_name: &str,
    ) -> InternalModelColumnLineage {
        compute_column_lineage_internal(self, model_name)
    }
}

#[cfg(test)]
fn compute_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DlinDialect,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    ColumnLineageAnalysis::new(manifest, dialect, cache).compute_column_lineage(model_name)
}

#[cfg(test)]
fn compute_cross_model_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    dialect: DlinDialect,
    cache: &mut ColumnLineageCache,
) -> ModelColumnLineage {
    ColumnLineageAnalysis::new(manifest, dialect, cache)
        .compute_cross_model_column_lineage(model_name)
}

#[cfg(test)]
fn compute_column_impact(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    dialect: DlinDialect,
    cache: &mut ColumnLineageCache,
) -> ColumnImpactReport {
    ColumnLineageAnalysis::new(manifest, dialect, cache)
        .compute_column_impact(model_name, column_name)
}

pub(super) fn compute_column_lineage_internal(
    analysis: &mut ColumnLineageAnalysis<'_>,
    model_name: &str,
) -> InternalModelColumnLineage {
    let manifest = analysis.manifest;
    let dialect = analysis.dialect;
    let node = find_model_by_unique_id(manifest, model_name)
        .or_else(|| find_model_by_name(manifest, model_name));

    let node = match node {
        Some(n) => n,
        None => {
            return InternalModelColumnLineage {
                model: model_name.to_string(),
                traced_columns: 0,
                total_columns: 0,
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::ModelNotFound,
                    column: None,
                    what: format!("model '{}' not found in manifest", model_name),
                    why: None,
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                }],
            };
        }
    };

    let display_name = node.name.as_str();
    let cache_key = node.unique_id.as_str();
    if let Some(cached) = analysis.lineage_results.get(cache_key) {
        return cached.clone();
    }

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => {
            return InternalModelColumnLineage {
                model: display_name.to_string(),
                traced_columns: 0,
                total_columns: 0,
                columns: vec![],
                errors: vec![ColumnLineageError {
                    kind: ColumnLineageErrorKind::NoCompiledCode,
                    column: None,
                    what: format!("model '{}' has no compiled_code", display_name),
                    why: Some("compiled SQL is required for column lineage analysis".to_string()),
                    hint: Some(
                        "Run `dbt compile` first; use `dlin check-manifest` to verify the manifest is up to date".to_string(),
                    ),
                }],
            };
        }
    };

    let semantic_digest = analysis.semantic_digests.digest_for_node(manifest, node);
    // Cache entries are keyed by the manifest's canonical identity. Callers
    // may address the same model by its short name or its unique_id, but both
    // spellings must share one persistent entry.
    if semantic_digest.persistent_cache_safe
        && let Some(cached) = analysis
            .cache
            .get(cache_key, dialect, semantic_digest.value)
    {
        let cached = cached.clone();
        analysis
            .lineage_results
            .insert(cache_key.to_string(), cached.clone());
        return cached;
    }

    #[cfg(test)]
    let catalog = {
        let backend = backend::backend_for_tests(backend::BackendId::Sqllineage);
        schema::build_schema_from_manifest(manifest, node, dialect, &backend)
    };
    #[cfg(not(test))]
    let catalog = schema::build_schema_from_manifest(manifest, node, dialect);

    let parsed = match parse_sql(compiled_code, dialect) {
        Ok(parsed) => parsed,
        Err(message) => {
            if node.columns.is_empty() {
                return column_inference_failure(display_name);
            }
            return parse_failure(display_name, node.columns.len(), message);
        }
    };
    let yaml_schema = build_yaml_schema_for_node(manifest, node);
    let analyzed = analyze_parsed(&parsed, dialect, catalog.as_ref());
    let discovered = if yaml_schema.as_ref() == catalog.as_ref() {
        analyzed
            .statements
            .first()
            .filter(|statement| statement.kind.carries_lineage())
            .map(|statement| output_names(statement).0)
            .unwrap_or_default()
    } else {
        let discovery = analyze_parsed(&parsed, dialect, yaml_schema.as_ref());
        discovery
            .statements
            .first()
            .filter(|statement| statement.kind.carries_lineage())
            .map(|statement| output_names(statement).0)
            .unwrap_or_default()
    };
    let mut names: HashSet<String> = node.columns.keys().cloned().collect();
    names.extend(discovered);
    let mut column_names: Vec<String> = names.into_iter().collect();
    column_names.sort();

    if column_names.is_empty() {
        return column_inference_failure(display_name);
    }

    let total = column_names.len();
    let statement = match analyzed.into_single() {
        Ok(statement) => statement,
        Err(error) => return parse_failure(display_name, total, error.to_string()),
    };
    if !statement.kind.carries_lineage() {
        return parse_failure(
            display_name,
            total,
            "statement does not carry column lineage".to_string(),
        );
    }
    if let Some(detail) = statement_error(&statement) {
        return parse_failure(display_name, total, detail);
    }

    let has_star_columns = statement.has_unresolved_stars;
    let mut columns = Vec::new();
    let mut errors = Vec::new();
    for col_name in &column_names {
        match resolve_statement_output(&statement, col_name, dialect, false) {
            Ok(output) => {
                if let Some(failure) = lineage_failure(&output.lineage) {
                    let state = output_failure_state(failure.kind);
                    errors.push(column_resolution_error_with_message(
                        col_name,
                        state,
                        failure.message,
                        has_star_columns,
                    ));
                    continue;
                }
                let sqllineage::Lineage::Complete(sources) = &output.lineage else {
                    unreachable!("lineage failure was handled above")
                };
                columns.push(InternalColumnLineageEntry {
                    column: col_name.clone(),
                    transformation: backend::sqllineage::transformation(output.transform),
                    sources: sources
                        .iter()
                        .map(|source| InternalColumnSource {
                            relation: backend::source_relation(
                                &source.relation,
                                catalog.as_ref(),
                                dialect,
                            ),
                            column: source.column.value.clone(),
                            model_path: vec![],
                        })
                        .collect(),
                });
            }
            Err(failure) => errors.push(column_resolution_error_with_message(
                col_name,
                output_failure_state(failure.kind),
                failure.message,
                has_star_columns,
            )),
        }
    }

    let result = InternalModelColumnLineage {
        model: display_name.to_string(),
        traced_columns: columns.len(),
        total_columns: total,
        columns,
        errors,
    };

    if semantic_digest.persistent_cache_safe {
        analysis
            .cache
            .insert(cache_key, dialect, semantic_digest.value, result.clone());
    }
    analysis
        .lineage_results
        .insert(cache_key.to_string(), result.clone());
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

pub(super) fn find_model_by_unique_id<'a>(
    manifest: &'a Manifest,
    unique_id: &str,
) -> Option<&'a crate::parser::manifest::ManifestNode> {
    manifest
        .nodes
        .get(unique_id)
        .filter(|node| node.resource_type == "model")
}
