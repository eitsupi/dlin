use crate::parser::manifest::{Manifest, ManifestNode};
use std::collections::{HashMap, HashSet};

mod backend;
mod cache;
mod cross_model;
mod ident;
mod impact;
mod relation;
mod schema;
#[cfg(test)]
mod tests;
mod types;

use backend::sqllineage::{
    OutputFailureKind, analyze_parsed, lineage_failure, parse_sql, single_lineage_statement,
    unresolved_reason,
};
pub use backend::{
    CatalogSnapshot, DialectClassification, DlinDialect, REMOVED_DIALECTS, check_sql_parses,
    debug_parse_sql_ast_debug, debug_parse_sql_json, debug_trace_column_json,
};
pub use cache::ColumnLineageCache;
pub use ident::{Ident, Quote};
pub use impact::{
    ColumnImpactReport, ConfirmedImpact, DependencyKind, DependencyRef, ExplorationStatus,
    ImpactEdge, ImpactScope, KindExploration, PossibleImpact, TraceGap,
};
pub use relation::RelationRef;
use schema::{SchemaContext, SemanticDigestCache};
pub use types::{
    ColumnLineageError, ColumnLineageErrorKind, ColumnSource, ModelColumnLineage, ModelOutputRef,
    ModelPathHop, OutputName, OutputShape, OutputSlot, OutputSlotId, OutputSlots,
    SchemaDependencyKind, SetCorrespondence, SetOperand, SetOperationKind, SetOperator,
    SetQuantifier, SlotLookup, TraceBoundary, TraceBoundaryPolicy, TraceBoundaryReason,
    TraceResource, TraceResourceCandidate, TraceResourceKind, TransformationType, UnexpandedStar,
    UnresolvedCandidate, UnresolvedContribution, UnresolvedReason, UnresolvedSource, ValueLineage,
};
use types::{
    InternalColumnSource, InternalModelColumnLineage, OPEN_SHAPE_HINT,
    normalize_column_lineage_errors,
};

fn parse_failure(node: &ManifestNode, reason: String) -> InternalModelColumnLineage {
    let model = node.name.as_str();
    InternalModelColumnLineage::failed(
        &node.unique_id,
        model,
        ColumnLineageError {
            kind: ColumnLineageErrorKind::ParseFailure,
            column: None,
            slot: None,
            what: format!("failed to parse SQL for '{model}'"),
            why: Some(reason),
            hint: Some(
                "Check the SQL with `dlin debug parse-sql`; ensure the correct --dialect is set"
                    .to_string(),
            ),
        },
    )
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
    schema: SchemaContext<'a>,
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
            schema: SchemaContext::new(manifest, dialect),
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
        scope: ImpactScope,
    ) -> ColumnImpactReport {
        impact::compute_column_impact(self, model_name, column_name, scope)
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
    compute_column_impact_with_scope(
        manifest,
        model_name,
        column_name,
        ImpactScope::ValueOnly,
        dialect,
        cache,
    )
}

#[cfg(test)]
fn compute_column_impact_with_scope(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
    scope: ImpactScope,
    dialect: DlinDialect,
    cache: &mut ColumnLineageCache,
) -> ColumnImpactReport {
    ColumnLineageAnalysis::new(manifest, dialect, cache).compute_column_impact(
        model_name,
        column_name,
        scope,
    )
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
            let candidates = model_candidates_by_name(manifest, model_name);
            if candidates.len() > 1 {
                let candidates = candidates
                    .iter()
                    .map(|node| node.unique_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return InternalModelColumnLineage::failed(
                    model_name,
                    model_name,
                    ColumnLineageError::model(
                        ColumnLineageErrorKind::ModelAmbiguous,
                        format!(
                            "model name '{model_name}' is ambiguous; use a unique_id: {candidates}"
                        ),
                    ),
                );
            }
            let mut snapshots = manifest
                .nodes
                .values()
                .filter(|node| {
                    node.resource_type == "snapshot"
                        && (node.unique_id == model_name
                            || node.unique_id.ends_with(&format!(".{model_name}")))
                })
                .collect::<Vec<_>>();
            snapshots.sort_unstable_by(|a, b| a.unique_id.cmp(&b.unique_id));
            if snapshots.len() == 1 {
                let node = snapshots[0];
                let (kind, message) = if node.compiled_code.is_some() {
                    (
                        ColumnLineageErrorKind::UnsupportedMaterialization,
                        format!(
                            "snapshot '{}' has unsupported materialization and history semantics",
                            node.name
                        ),
                    )
                } else {
                    (
                        ColumnLineageErrorKind::NoCompiledCode,
                        format!("snapshot '{}' has no compiled SQL definition", node.name),
                    )
                };
                return InternalModelColumnLineage::failed(
                    &node.unique_id,
                    &node.name,
                    ColumnLineageError::model(kind, message),
                );
            }
            if snapshots.len() > 1 {
                let candidates = snapshots
                    .iter()
                    .map(|node| node.unique_id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return InternalModelColumnLineage::failed(
                    model_name,
                    model_name,
                    ColumnLineageError::model(
                        ColumnLineageErrorKind::ModelAmbiguous,
                        format!(
                            "snapshot name '{model_name}' is ambiguous; use a unique_id: {candidates}"
                        ),
                    ),
                );
            }
            return InternalModelColumnLineage::failed(
                model_name,
                model_name,
                ColumnLineageError {
                    hint: Some("Run `dlin check-manifest` to verify the manifest is up to date, then `dlin list --source manifest` to see available models (pass the same --project-dir/--manifest-path if you specified them)".to_string()),
                    ..ColumnLineageError::model(
                        ColumnLineageErrorKind::ModelNotFound,
                        format!("model '{}' not found in manifest", model_name),
                    )
                },
            );
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
            return InternalModelColumnLineage::failed(
                cache_key,
                display_name,
                ColumnLineageError {
                    why: Some("compiled SQL is required for column lineage analysis".to_string()),
                    hint: Some(
                        "Run `dbt compile` first; use `dlin check-manifest` to verify the manifest is up to date".to_string(),
                    ),
                    ..ColumnLineageError::model(
                        ColumnLineageErrorKind::NoCompiledCode,
                        format!("model '{}' has no compiled_code", display_name),
                    )
                },
            );
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

    let catalog = analysis.schema.catalog_for(node);

    let parsed = match parse_sql(compiled_code, dialect) {
        Ok(parsed) => parsed,
        Err(message) => return parse_failure(node, message),
    };
    let analyzed = analyze_parsed(&parsed, dialect, catalog.as_ref());
    let statement = match single_lineage_statement(&analyzed) {
        Ok(statement) => statement,
        Err(detail) => return parse_failure(node, detail),
    };

    // The SQL output is the only source of output slots. Columns that the
    // YAML declares and the SQL does not list are reported, not added.
    let output = output_slots(node, &statement.output, catalog.as_ref(), dialect);
    let mut errors = slot_errors(
        node,
        &statement.output,
        &output,
        statement.has_unresolved_stars,
    );
    errors.extend(declared_column_differences(node, &output, dialect));

    let result = InternalModelColumnLineage {
        unique_id: cache_key.to_string(),
        model: display_name.to_string(),
        output: Some(output),
        boundaries: Vec::new(),
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

/// The identity of one slot of `node`'s output.
fn output_ref(node: &ManifestNode, slot: OutputSlotId, name: &OutputName) -> ModelOutputRef {
    ModelOutputRef {
        unique_id: node.unique_id.clone(),
        model: node.name.clone(),
        slot,
        column: name.to_string(),
    }
}

fn output_name(name: &sqllineage::OutputName) -> OutputName {
    match name {
        sqllineage::OutputName::Explicit(ident) => OutputName::Explicit(Ident::from_engine(ident)),
        sqllineage::OutputName::Inferred(ident) => OutputName::Inferred(Ident::from_engine(ident)),
        sqllineage::OutputName::Anonymous => OutputName::Anonymous,
    }
}

/// The slots of the engine output of `node`, in the engine's order. Duplicate
/// names stay separate slots, and an unexpanded star stays a shape entry.
fn output_slots(
    node: &ManifestNode,
    output: &sqllineage::Output,
    catalog: Option<&CatalogSnapshot>,
    dialect: DlinDialect,
) -> OutputSlots<InternalColumnSource> {
    let columns = output
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let id = OutputSlotId::new(index);
            let name = output_name(&column.name);
            let origin = output_ref(node, id, &name);
            OutputSlot {
                id,
                ordinal: column.ordinal,
                lineage: value_lineage(&column.lineage, &origin, catalog, dialect),
                transformation: backend::sqllineage::transformation(column.transform),
                name,
            }
        })
        .collect();
    let shape = match &output.shape {
        sqllineage::Shape::Closed => OutputShape::Closed,
        sqllineage::Shape::Open { unexpanded } => OutputShape::Open {
            unexpanded: unexpanded
                .iter()
                .map(|star| UnexpandedStar {
                    relation: star
                        .relation
                        .as_ref()
                        .map(|relation| backend::source_relation(relation, catalog, dialect)),
                    after_ordinal: star.after_ordinal,
                })
                .collect(),
        },
    };
    OutputSlots { columns, shape }
}

/// Diagnostics for the slots whose lineage the engine could not fully
/// establish. Each one is scoped to its own slot. An indeterminate slot carries
/// the star hint whenever any star of the statement, including a nested one,
/// stayed unexpanded.
fn slot_errors(
    node: &ManifestNode,
    output: &sqllineage::Output,
    slots: &OutputSlots<InternalColumnSource>,
    has_unresolved_stars: bool,
) -> Vec<ColumnLineageError> {
    output
        .columns
        .iter()
        .zip(&slots.columns)
        .filter_map(|(column, slot)| {
            let failure = lineage_failure(&column.lineage)?;
            let kind = match failure.kind {
                OutputFailureKind::Ambiguous => ColumnLineageErrorKind::ColumnAmbiguous,
                OutputFailureKind::Indeterminate => ColumnLineageErrorKind::ColumnIndeterminate,
            };
            let hint = (kind == ColumnLineageErrorKind::ColumnIndeterminate
                && has_unresolved_stars)
                .then(|| OPEN_SHAPE_HINT.to_string());
            Some(ColumnLineageError::for_slot(
                kind,
                &output_ref(node, slot.id, &slot.name),
                &failure.message,
                hint,
            ))
        })
        .collect()
}

/// Non-fatal diagnostics for columns that the model's YAML declares and that
/// the SQL output does not list. They carry the declared name but no slot. Names are compared with the dialect's
/// identifier rules. The wording depends on the shape: a closed output does
/// not contain the column, while an open output only does not enumerate it.
fn declared_column_differences(
    node: &ManifestNode,
    output: &OutputSlots<InternalColumnSource>,
    dialect: DlinDialect,
) -> Vec<ColumnLineageError> {
    let Some(declared) = schema::evidence::yaml_schema_evidence(
        &node.unique_id,
        &node.columns,
        node.config.contract.as_ref(),
        dialect,
    ) else {
        return Vec::new();
    };
    let engine = dialect
        .to_engine_dialect()
        .expect("dialect checked during parsing");
    let listed: HashSet<String> = output
        .columns
        .iter()
        .filter_map(|slot| slot.name.ident())
        .map(|ident| ident.engine_key(engine))
        .collect();
    declared
        .columns
        .iter()
        .filter(|column| !listed.contains(column.name.column_key(engine).as_ref()))
        .map(|column| {
            let name = Ident::from_engine(&column.name).to_string();
            let (kind, what, hint) = match output.shape {
                OutputShape::Closed => (
                    ColumnLineageErrorKind::DeclaredColumnNotInOutput,
                    format!("column {name} is declared in YAML but is not in the SQL output"),
                    None,
                ),
                OutputShape::Open { .. } => (
                    ColumnLineageErrorKind::DeclaredColumnNotEnumerated,
                    format!(
                        "column {name} is declared in YAML but is not among the enumerated SQL outputs"
                    ),
                    Some(OPEN_SHAPE_HINT.to_string()),
                ),
            };
            // The declared column has no output slot, so only its name
            // identifies it.
            ColumnLineageError {
                kind,
                column: Some(name),
                slot: None,
                what,
                why: None,
                hint,
            }
        })
        .collect()
}

/// Convert an engine lineage into the typed value lineage of an output at
/// `origin`. Unresolved contributions are ordered and deduplicated.
pub(super) fn value_lineage(
    lineage: &sqllineage::Lineage,
    origin: &ModelOutputRef,
    catalog: Option<&CatalogSnapshot>,
    dialect: DlinDialect,
) -> ValueLineage<InternalColumnSource> {
    let known = |sources: &[sqllineage::SourceColumn]| {
        sources
            .iter()
            .map(|source| InternalColumnSource {
                relation: backend::source_relation(&source.relation, catalog, dialect),
                column: Ident::from_engine(&source.column),
                model_path: vec![],
            })
            .collect()
    };
    match lineage {
        sqllineage::Lineage::Complete(sources) => ValueLineage::complete(known(sources)),
        sqllineage::Lineage::Partial {
            known: sources,
            unresolved,
        } => {
            let mut unresolved: Vec<UnresolvedContribution> = unresolved
                .iter()
                .map(|item| UnresolvedContribution {
                    reason: unresolved_reason(item, catalog, dialect),
                    origin: origin.clone(),
                    model_path: vec![],
                })
                .collect();
            unresolved.sort();
            unresolved.dedup();
            ValueLineage::new(known(sources), unresolved)
        }
    }
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
    let matches = model_candidates_by_name(manifest, name);
    match matches.len() {
        0 => None,
        1 => Some(matches[0]),
        _ => None,
    }
}

pub(super) fn model_candidates_by_name<'a>(
    manifest: &'a Manifest,
    name: &str,
) -> Vec<&'a crate::parser::manifest::ManifestNode> {
    let suffix = format!(".{name}");
    let mut matches = manifest
        .nodes
        .values()
        .filter(|node| node.resource_type == "model" && node.unique_id.ends_with(&suffix))
        .collect::<Vec<_>>();
    matches.sort_unstable_by(|left, right| left.unique_id.cmp(&right.unique_id));
    matches
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
