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
    Backend, BackendColumnOutcome, BackendErrorKind, BackendSource, CatalogSnapshot, DlinDialect,
    LineageBackend, LineageRequest, OutputColumnRequest, OutputOrdinal, PolyglotBackend,
    require_single_lineage_statement,
};
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
        ordinal: OutputOrdinal(0),
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

    let backend = Backend::Polyglot(PolyglotBackend::new());
    let analysis = backend.analyze(&request).map_err(|e| (e.kind, e.message))?;
    let mut statement =
        require_single_lineage_statement(analysis).map_err(|e| (e.kind, e.message))?;
    let outcome = statement.columns.pop().expect("one column was requested");

    Ok(match outcome {
        BackendColumnOutcome::Resolved(result) => PathOutcome::Resolved {
            sources: result
                .sources
                .into_iter()
                .map(|s| match s {
                    BackendSource::Concrete { table, column } => (table, column),
                    other => panic!(
                        "the polyglot backend never produces this source variant: {:?}",
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
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect);

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
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect);

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

#[test]
fn plain_select_resolves_each_column_directly() {
    assert_resolved(
        "select id, name from raw_table",
        DlinDialect::Generic,
        None,
        "id",
        &[("raw_table", "id")],
        TransformationType::Direct,
    );
    assert_resolved(
        "select id, name from raw_table",
        DlinDialect::Generic,
        None,
        "name",
        &[("raw_table", "name")],
        TransformationType::Direct,
    );
}

#[test]
fn cte_traces_through_to_the_base_table() {
    assert_resolved(
        "with base as (select id as order_id from raw_orders) select order_id from base",
        DlinDialect::Generic,
        None,
        "order_id",
        &[("raw_orders", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn select_star_with_known_schema_resolves_the_named_column() {
    assert_resolved(
        "select * from known_table",
        DlinDialect::Generic,
        Some(("known_table", &["id", "amount"])),
        "id",
        &[("known_table", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn select_star_with_unknown_schema_cannot_resolve_a_column() {
    assert_failed(
        "select * from unknown_table",
        DlinDialect::Generic,
        None,
        "id",
        "Cannot find column",
    );
}

/// Pins the fix for a bug where top-level `UNION`/`INTERSECT`/`EXCEPT`
/// statements were flagged as not lineage-bearing and rejected outright
/// before any column was even attempted.
#[test]
fn top_level_union_all_traces_both_operands() {
    assert_resolved(
        "select id, amt from t1 union all select id, amt from t2",
        DlinDialect::Generic,
        None,
        "id",
        &[("t1", "id"), ("t2", "id")],
        // Unlike a plain SELECT, the top-level lineage node for a set operation
        // is the Union/Except expression itself, not a Column reference, so
        // `classify_transformation` falls through to `Unknown` — even though
        // each individual leaf is a direct column reference.
        TransformationType::Unknown,
    );
}

#[test]
fn top_level_except_traces_both_operands() {
    assert_resolved(
        "select id from t1 except select id from t2",
        DlinDialect::Generic,
        None,
        "id",
        &[("t1", "id"), ("t2", "id")],
        // Unlike a plain SELECT, the top-level lineage node for a set operation
        // is the Union/Except expression itself, not a Column reference, so
        // `classify_transformation` falls through to `Unknown` — even though
        // each individual leaf is a direct column reference.
        TransformationType::Unknown,
    );
}

#[test]
fn unresolvable_column_fails_with_column_resolution_kind() {
    let sql = "select id from t1";
    let (manifest, node_id) = make_manifest(sql, None);
    let node = manifest.nodes.get(&node_id).unwrap();
    let dialect = DlinDialect::Generic;
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect);

    match analyze_one_column(
        &manifest,
        &node_id,
        dialect,
        catalog.as_ref(),
        "totally_missing_column",
    ) {
        Ok(PathOutcome::Failed(message)) => {
            assert!(
                message.contains("Cannot find column"),
                "message: {}",
                message
            );
        }
        other => panic!("expected a column-resolution failure, got: {:?}", other),
    }
}

/// Pins the fix for a bug where a `parse_one` failure was misclassified as
/// `BackendErrorKind::ColumnResolution` instead of `BackendErrorKind::Parse`.
#[test]
fn unparseable_sql_fails_with_parse_kind() {
    let sql = "select from from";
    let (manifest, node_id) = make_manifest(sql, None);
    let node = manifest.nodes.get(&node_id).unwrap();
    let dialect = DlinDialect::Generic;
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect);

    match analyze_one_column(&manifest, &node_id, dialect, catalog.as_ref(), "anything") {
        Err((kind, _message)) => {
            assert_eq!(
                kind,
                BackendErrorKind::Parse,
                "an unparseable statement must be classified as a parse failure, not a \
                 column-resolution failure"
            );
        }
        Ok(outcome) => panic!("expected the analysis to fail to parse, got {:?}", outcome),
    }
}
