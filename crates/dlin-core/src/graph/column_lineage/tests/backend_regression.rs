//! Backend regression tests pinning column-lineage outcomes for a
//! representative set of SQL shapes: a plain `SELECT`, a CTE, `SELECT *`
//! with a known and an unknown schema, top-level `UNION ALL`/`EXCEPT`, an
//! unresolvable column, and unparseable SQL.
//!
//! These values were established by cross-checking the `Backend`-based
//! analysis pipeline against the lineage-tracing logic column lineage used
//! before it was routed through `Backend`, for the same inputs. That
//! comparison caught two bugs before they reached production: top-level
//! `UNION`/`INTERSECT`/`EXCEPT` statements were incorrectly rejected as
//! "not lineage-bearing", and unparseable SQL was misclassified as an
//! indeterminate column-resolution failure rather than a parse failure. Both
//! are now fixed and pinned here.

use std::collections::{BTreeSet, HashMap};

use super::super::backend::{
    AnalysisCompleteness, AnalysisSlot, BackendColumnOutcome, BackendErrorKind, BackendId,
    BackendSource, CatalogSnapshot, DlinDialect, LineageBackend, LineageRequest,
    OutputColumnRequest, OutputDiscovery, OutputDiscoveryRequest, OutputName, ResolutionState,
    SqllineageCatalogProvider, backend_for_tests, normalize_column_outcomes,
    require_single_lineage_statement,
};
use super::super::relation::RelationRef;
use super::super::{TransformationType, schema};
use crate::parser::manifest::{DependsOn, Manifest, ManifestColumn, ManifestConfig, ManifestNode};

/// Build a manifest with a single "root" model carrying `compiled_code`, plus
/// an optional upstream model that supplies YAML columns for a table the SQL
/// references — enough for `schema::build_schema_from_manifest` to produce a
/// non-empty `CatalogSnapshot` for the "known schema" case.
fn make_manifest(compiled_code: &str, known_table: Option<(&str, &[&str])>) -> (Manifest, String) {
    let mut nodes = HashMap::new();
    let mut depends_on = Vec::new();

    if let Some((table_name, columns)) = known_table {
        let mut cols = HashMap::new();
        for c in columns {
            cols.insert(
                c.to_string(),
                ManifestColumn {
                    name: c.to_string(),
                },
            );
        }
        let dep_id = format!("model.proj.{}", table_name);
        nodes.insert(
            dep_id.clone(),
            ManifestNode {
                unique_id: dep_id.clone(),
                name: table_name.to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn { nodes: vec![] },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                original_file_path: None,
                columns: cols,
                compiled_code: None,
                database: None,
                schema: None,
            },
        );
        depends_on.push(dep_id);
    }

    let root_id = "model.proj.root".to_string();
    nodes.insert(
        root_id.clone(),
        ManifestNode {
            unique_id: root_id.clone(),
            name: "root".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: depends_on },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: HashMap::new(),
            compiled_code: Some(compiled_code.to_string()),
            database: None,
            schema: None,
        },
    );

    (
        Manifest {
            nodes,
            sources: HashMap::new(),
            exposures: HashMap::new(),
            ..Default::default()
        },
        root_id,
    )
}

/// Outcome of tracing one column through the backend, normalized for
/// assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PathOutcome {
    Resolved {
        sources: Vec<(String, String)>,
        transformation: TransformationType,
    },
    Failed(String),
}

/// Result of analyzing a statement: either a normalized per-column outcome,
/// or the `(kind, message)` of a whole-statement failure (a parse failure,
/// or a statement rejected by `require_single_lineage_statement`).
type AnalysisResult = Result<PathOutcome, (BackendErrorKind, String)>;

fn analyze_one_column(
    manifest: &Manifest,
    node_id: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
    column: &str,
) -> AnalysisResult {
    let node = manifest.nodes.get(node_id).unwrap();
    let compiled_code = node.compiled_code.as_ref().unwrap();

    let outputs = vec![OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: column.to_string(),
    }];
    let duplicate_output_names = BTreeSet::new();
    let request = LineageRequest {
        sql: compiled_code,
        dialect,
        catalog,
        outputs: &outputs,
        duplicate_output_names: &duplicate_output_names,
    };

    let backend = backend_for_tests(BackendId::Sqllineage);
    let analysis = backend.analyze(&request).map_err(|e| (e.kind, e.message))?;
    let statement = require_single_lineage_statement(analysis).map_err(|e| (e.kind, e.message))?;
    let (outcomes, diagnostics) = normalize_column_outcomes(&outputs, statement.columns);
    assert!(
        diagnostics.is_empty(),
        "backend contract diagnostics: {diagnostics:?}"
    );
    assert_eq!(outcomes.len(), outputs.len());
    let outcome = outcomes
        .into_iter()
        .next()
        .expect("one column was requested");
    let target = match &outcome {
        BackendColumnOutcome::Resolved(result) => &result.target,
        BackendColumnOutcome::Failed(failure) => &failure.target,
    };
    assert_eq!(target.slot, outputs[0].slot);
    assert_eq!(target.name, outputs[0].name);

    Ok(match outcome {
        BackendColumnOutcome::Resolved(result) => PathOutcome::Resolved {
            sources: result
                .sources
                .into_iter()
                .map(|s| match s {
                    BackendSource::Concrete { relation, column } => (relation.render(), column),
                    other => panic!(
                        "the active backend never produces this source variant: {:?}",
                        other
                    ),
                })
                .collect(),
            transformation: result.transformation,
        },
        BackendColumnOutcome::Failed(failure) => PathOutcome::Failed(failure.error.message),
    })
}

fn assert_resolved(
    sql: &str,
    dialect: DlinDialect,
    known_table: Option<(&str, &[&str])>,
    column: &str,
    expected_sources: &[(&str, &str)],
    expected_transformation: TransformationType,
) {
    let (manifest, node_id) = make_manifest(sql, known_table);
    let node = manifest.nodes.get(&node_id).unwrap();
    let backend = backend_for_tests(BackendId::Sqllineage);
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect, &backend);

    match analyze_one_column(&manifest, &node_id, dialect, catalog.as_ref(), column) {
        Ok(PathOutcome::Resolved {
            sources,
            transformation,
        }) => {
            let expected: Vec<(String, String)> = expected_sources
                .iter()
                .map(|(t, c)| (t.to_string(), c.to_string()))
                .collect();
            assert_eq!(
                sources, expected,
                "sources for column '{}' in: {}",
                column, sql
            );
            assert_eq!(
                transformation, expected_transformation,
                "transformation for column '{}' in: {}",
                column, sql
            );
        }
        other => panic!(
            "expected column '{}' to resolve in SQL: {}, got: {:?}",
            column, sql, other
        ),
    }
}

fn assert_failed(
    sql: &str,
    dialect: DlinDialect,
    known_table: Option<(&str, &[&str])>,
    column: &str,
    expected_message_contains: &str,
) {
    let (manifest, node_id) = make_manifest(sql, known_table);
    let node = manifest.nodes.get(&node_id).unwrap();
    let backend = backend_for_tests(BackendId::Sqllineage);
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect, &backend);

    match analyze_one_column(&manifest, &node_id, dialect, catalog.as_ref(), column) {
        Ok(PathOutcome::Failed(message)) => {
            assert!(
                message.contains(expected_message_contains),
                "message '{}' does not contain '{}' for column '{}' in: {}",
                message,
                expected_message_contains,
                column,
                sql
            );
        }
        other => panic!(
            "expected column '{}' to fail in SQL: {}, got: {:?}",
            column, sql, other
        ),
    }
}

mod basic {
    use super::*;
    include!("backend_regression/basic.rs");
}
fn sqllineage_statement(
    sql: &str,
    catalog: Option<&CatalogSnapshot>,
    outputs: &[OutputColumnRequest],
    duplicate_output_names: &BTreeSet<String>,
) -> super::super::backend::BackendStatementResult {
    let statement =
        sqllineage_statement_without_completeness(sql, catalog, outputs, duplicate_output_names);
    assert_eq!(statement.completeness, AnalysisCompleteness::Complete);
    statement
}

fn sqllineage_statement_with_dialect(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
    outputs: &[OutputColumnRequest],
    duplicate_output_names: &BTreeSet<String>,
) -> super::super::backend::BackendStatementResult {
    let request = LineageRequest {
        sql,
        dialect,
        catalog,
        outputs,
        duplicate_output_names,
    };
    let backend = backend_for_tests(BackendId::Sqllineage);
    let statement = backend
        .analyze(&request)
        .unwrap()
        .statements
        .into_iter()
        .next()
        .expect("one statement was analyzed");
    assert_eq!(statement.completeness, AnalysisCompleteness::Complete);
    statement
}

fn sqllineage_statement_without_completeness(
    sql: &str,
    catalog: Option<&CatalogSnapshot>,
    outputs: &[OutputColumnRequest],
    duplicate_output_names: &BTreeSet<String>,
) -> super::super::backend::BackendStatementResult {
    let request = LineageRequest {
        sql,
        dialect: DlinDialect::Generic,
        catalog,
        outputs,
        duplicate_output_names,
    };
    let backend = backend_for_tests(BackendId::Sqllineage);
    backend
        .analyze(&request)
        .unwrap()
        .statements
        .into_iter()
        .next()
        .expect("one statement was analyzed")
}

fn sqllineage_outcome(
    statement: &super::super::backend::BackendStatementResult,
    slot: usize,
) -> &BackendColumnOutcome {
    statement
        .columns
        .iter()
        .find(|outcome| match outcome {
            BackendColumnOutcome::Resolved(result) => result.target.slot == AnalysisSlot(slot),
            BackendColumnOutcome::Failed(failure) => failure.target.slot == AnalysisSlot(slot),
        })
        .unwrap_or_else(|| panic!("no outcome for slot {slot}: {:?}", statement.columns))
}

mod set_operations {
    use super::*;
    include!("backend_regression/set_operations.rs");
}
mod discovery {
    use super::*;
    include!("backend_regression/discovery.rs");
}
