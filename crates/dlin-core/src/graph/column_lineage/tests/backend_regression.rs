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
    OutputColumnRequest, OutputDiscovery, OutputDiscoveryRequest, OutputName,
    SqllineageCatalogProvider, backend_for_tests, normalize_column_outcomes,
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

    let backend = backend_for_tests(BackendId::Polyglot);
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
    let backend = backend_for_tests(BackendId::Polyglot);
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
    let backend = backend_for_tests(BackendId::Polyglot);
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
fn cli_ordered_schema_aligns_select_star_union_columns() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("t", ["b".to_string(), "a".to_string()]);
    assert_eq!(catalog.table_columns("t").unwrap(), ["b", "a"]);

    let sql = "select * from t union all select * from t";
    let backend = backend_for_tests(BackendId::Polyglot);
    let duplicate_output_names = BTreeSet::new();

    for (column, expected_sources) in [("b", vec![("t", "b")]), ("a", vec![("t", "a")])].into_iter()
    {
        let outputs = [OutputColumnRequest {
            slot: AnalysisSlot(0),
            name: column.to_string(),
        }];
        let request = LineageRequest {
            sql,
            dialect: DlinDialect::Generic,
            catalog: Some(&catalog),
            outputs: &outputs,
            duplicate_output_names: &duplicate_output_names,
        };
        let analysis = backend.analyze(&request).unwrap();
        let statement = require_single_lineage_statement(analysis).unwrap();
        let (outcomes, diagnostics) = normalize_column_outcomes(&outputs, statement.columns);
        assert!(
            diagnostics.is_empty(),
            "backend contract diagnostics: {diagnostics:?}"
        );
        assert_eq!(outcomes.len(), outputs.len());
        let outcome = outcomes.into_iter().next().unwrap();
        let target = match &outcome {
            BackendColumnOutcome::Resolved(result) => &result.target,
            BackendColumnOutcome::Failed(failure) => &failure.target,
        };
        assert_eq!(target.slot, outputs[0].slot);
        assert_eq!(target.name, outputs[0].name);
        let sources = match outcome {
            BackendColumnOutcome::Resolved(result) => result
                .sources
                .into_iter()
                .map(|source| match source {
                    BackendSource::Concrete { table, column } => (table, column),
                    other => panic!("unexpected source: {:?}", other),
                })
                .collect::<Vec<_>>(),
            other => panic!("expected resolved output, got: {:?}", other),
        };
        let expected = expected_sources
            .into_iter()
            .map(|(table, column)| (table.to_string(), column.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(sources, expected, "lineage for output column {column}");
    }
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
    let backend = backend_for_tests(BackendId::Polyglot);
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect, &backend);

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
    let backend = backend_for_tests(BackendId::Polyglot);
    let catalog = schema::build_schema_from_manifest(&manifest, node, dialect, &backend);

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

fn assert_leading_star_set_operation_is_indeterminate(sql: &str) {
    let outputs = [
        OutputColumnRequest {
            slot: AnalysisSlot(0),
            name: "id".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(1),
            name: "explicit_col".to_string(),
        },
    ];
    let statement =
        sqllineage_statement_without_completeness(sql, None, &outputs, &BTreeSet::new());

    let AnalysisCompleteness::Indeterminate { reason } = &statement.completeness else {
        panic!(
            "leading-star set operation should be indeterminate, got {:?}",
            statement.completeness
        );
    };
    assert!(
        reason.contains("leading branch is SELECT *"),
        "reason: {reason}"
    );
    assert!(reason.contains("lineage for this statement cannot be trusted"));

    for slot in 0..outputs.len() {
        match sqllineage_outcome(&statement, slot) {
            BackendColumnOutcome::Failed(failure) => {
                assert_eq!(
                    failure.resolution,
                    super::super::backend::ResolutionState::Indeterminate
                );
                assert!(failure.error.message.contains("leading branch is SELECT *"));
            }
            other => panic!("expected output {slot} to be indeterminate, got {other:?}"),
        }
    }
}

#[test]
fn sqllineage_union_with_leading_star_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT * FROM first_source UNION SELECT id, explicit_col FROM second_source",
    );
}

#[test]
fn sqllineage_union_all_with_leading_star_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source",
    );
}

#[test]
fn sqllineage_qualified_leading_star_set_operation_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT first_source.* FROM first_source UNION SELECT id, explicit_col FROM second_source",
    );
}

#[test]
fn sqllineage_intersect_with_leading_star_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT * FROM first_source INTERSECT SELECT id, explicit_col FROM second_source",
    );
}

#[test]
fn sqllineage_except_with_leading_star_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT * FROM first_source EXCEPT SELECT id, explicit_col FROM second_source",
    );
}

#[test]
fn sqllineage_second_branch_star_does_not_guard_explicit_output() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "explicit_col".to_string(),
    }];
    let statement = sqllineage_statement(
        "SELECT id, 1 AS explicit_col FROM raw.orders UNION SELECT id, * FROM some_unknown_source",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Resolved(result) => {
            assert_eq!(result.target.name, "explicit_col");
        }
        other => panic!("the second-branch star must not guard explicit output: {other:?}"),
    }
}

#[test]
fn sqllineage_nonleading_name_after_leading_star_is_indeterminate() {
    // The leading SELECT * determines the CTE's output names. Since its schema
    // is unknown, the second branch's col_a cannot establish the requested
    // output name or its ordinal alignment.
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "col_a".to_string(),
    }];
    let statement = sqllineage_statement_without_completeness(
        "WITH u AS (SELECT * FROM ext_a UNION ALL SELECT 2 AS col_a) SELECT col_a FROM u",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => assert_eq!(
            failure.resolution,
            super::super::backend::ResolutionState::Indeterminate
        ),
        other => panic!("expected unresolved output to be indeterminate, got {other:?}"),
    }
}

#[test]
fn sqllineage_leading_name_with_unresolved_aligned_branch_is_not_found() {
    // c1 is correctly named by the leading operand, but the ordinally aligned
    // a.col_x in the other branch cannot resolve because a is an unknown-schema
    // SELECT * CTE, so sqllineage reports the requested output as NotFound.
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "c1".to_string(),
    }];
    let statement = sqllineage_statement(
        "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c1, 2 AS c2 UNION ALL SELECT a.col_x, a.col_y FROM a) SELECT c1 FROM u",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => assert_eq!(
            failure.resolution,
            super::super::backend::ResolutionState::NotFound
        ),
        other => panic!("expected unresolved output to be not found, got {other:?}"),
    }
}

#[test]
fn sqllineage_nonleading_set_output_name_is_not_found() {
    // c9 is named only by the non-leading operand. Set-operation output names
    // come from c1 in the leading operand, so c9 has no output mapping.
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "c9".to_string(),
    }];
    let statement = sqllineage_statement(
        "WITH u AS (SELECT 1 AS c1 UNION ALL SELECT 2 AS c9) SELECT c9 FROM u",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => assert_eq!(
            failure.resolution,
            super::super::backend::ResolutionState::NotFound
        ),
        other => panic!("expected unresolved output to be not found, got {other:?}"),
    }
}

#[test]
fn sqllineage_guard_preserves_duplicate_output_ambiguity() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "explicit_col".to_string(),
    }];
    let duplicates = BTreeSet::from(["explicit_col".to_string()]);
    let statement = sqllineage_statement_without_completeness(
        "SELECT * FROM first_source UNION SELECT id, explicit_col FROM second_source",
        None,
        &outputs,
        &duplicates,
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::Ambiguous
            );
            assert!(failure.error.message.contains("output name is duplicated"));
        }
        other => panic!("duplicate output ambiguity must take precedence: {other:?}"),
    }
}

#[test]
fn sqllineage_leading_star_set_operation_inside_cte_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "WITH combined AS (SELECT * FROM first_source UNION SELECT id, explicit_col FROM second_source) SELECT id, explicit_col FROM combined",
    );
}

#[test]
fn sqllineage_leading_star_set_operation_inside_derived_table_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT id, explicit_col FROM (SELECT * FROM first_source UNION SELECT id, explicit_col FROM second_source) combined",
    );
}

#[test]
fn sqllineage_leading_star_set_operation_inside_parenthesized_query_cte_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "(WITH combined AS (SELECT * FROM first_source UNION SELECT id FROM second_source) SELECT id, 1 AS explicit_col FROM combined) UNION SELECT id, explicit_col FROM second_source",
    );
}

#[test]
fn sqllineage_leading_star_set_operation_inside_scalar_projection_subquery_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT id, 1 AS explicit_col, (SELECT * FROM first_source UNION SELECT id FROM second_source) AS nested FROM orders",
    );
}

#[test]
fn sqllineage_leading_star_set_operation_inside_case_arm_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT id, 1 AS explicit_col, CASE WHEN id > 0 THEN (SELECT * FROM first_source UNION SELECT id FROM second_source) ELSE NULL END AS nested FROM orders",
    );
}

#[test]
fn sqllineage_leading_star_set_operation_inside_function_argument_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT id, 1 AS explicit_col, COALESCE((SELECT * FROM first_source UNION SELECT id FROM second_source), 0) AS nested FROM orders",
    );
}

#[test]
fn sqllineage_leading_star_set_operation_inside_in_subquery_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT id, 1 AS explicit_col FROM orders WHERE id IN (SELECT * FROM first_source UNION SELECT id FROM second_source)",
    );
}

#[test]
fn sqllineage_nested_leading_star_set_operation_is_indeterminate() {
    assert_leading_star_set_operation_is_indeterminate(
        "SELECT * FROM a UNION SELECT x FROM b UNION SELECT y FROM c",
    );
}

#[test]
fn sqllineage_plain_star_without_set_operation_keeps_existing_behavior() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement("SELECT * FROM orders", None, &outputs, &BTreeSet::new());

    assert!(statement.has_unresolved_stars);
    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => assert_eq!(
            failure.resolution,
            super::super::backend::ResolutionState::Indeterminate
        ),
        other => panic!("expected the existing unresolved-star behavior, got {other:?}"),
    }
}

#[test]
fn sqllineage_plain_projection_resolves_case_folded_concrete_source() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(7),
        name: "Output_ID".to_string(),
    }];
    let statement = sqllineage_statement(
        "select ID as output_id from SOURCE_TABLE",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 7) {
        BackendColumnOutcome::Resolved(result) => {
            assert_eq!(result.target.name, "Output_ID");
            assert_eq!(result.transformation, TransformationType::Direct);
            assert_eq!(
                result.sources,
                vec![BackendSource::Concrete {
                    table: "source_table".to_string(),
                    // The fork normalizes relation identifiers, while its
                    // expression visitor preserves this source column spelling.
                    column: "ID".to_string(),
                }]
            );
        }
        other => panic!("expected a concrete projection, got {other:?}"),
    }
}

#[test]
fn sqllineage_maps_expression_aggregate_and_case_transformations() {
    let outputs = [
        OutputColumnRequest {
            slot: AnalysisSlot(10),
            name: "total".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(20),
            name: "sum_amount".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(30),
            name: "flag".to_string(),
        },
    ];
    let statement = sqllineage_statement(
        "select amount + tax as total, sum(amount) as sum_amount, case when amount > 0 then amount else 0 end as flag from orders",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    for (slot, transformation) in [
        (10, TransformationType::Expression),
        (20, TransformationType::Aggregation),
        (30, TransformationType::Conditional),
    ] {
        match sqllineage_outcome(&statement, slot) {
            BackendColumnOutcome::Resolved(result) => {
                assert_eq!(result.transformation, transformation);
            }
            other => panic!("expected resolved transformation, got {other:?}"),
        }
    }
}

#[test]
fn sqllineage_cast_is_direct_by_decision() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "cast_amount".to_string(),
    }];
    let statement = sqllineage_statement(
        "select cast(amount as int) as cast_amount from orders",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Resolved(result) => {
            assert_eq!(result.transformation, TransformationType::Direct);
        }
        other => panic!("expected cast to resolve, got {other:?}"),
    }
}

#[test]
fn sqllineage_join_without_catalog_reports_genuine_ambiguity() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement(
        "select id from left_table join right_table on left_table.id = right_table.id",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::Ambiguous
            );
            assert!(matches!(
                failure.error.kind,
                BackendErrorKind::ColumnResolution {
                    state: super::super::backend::ResolutionState::Ambiguous
                }
            ));
            assert!(failure.error.message.contains("left_table"));
            assert!(failure.error.message.contains("right_table"));
        }
        other => panic!("expected genuine ambiguity, got {other:?}"),
    }
}

#[test]
fn sqllineage_join_with_catalog_resolves_mixed_case_column() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("left_table", ["ID".to_string()]);
    catalog.add_table("right_table", ["other".to_string()]);
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement(
        // Unqualified so that sqllineage has to ask the catalog which table owns
        // `id`. A qualified reference resolves from the binding alone and never
        // reaches `resolve_column`, so it would not exercise the case folding.
        "select id from left_table join right_table on left_table.id = right_table.other",
        Some(&catalog),
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Resolved(result) => assert_eq!(
            result.sources,
            vec![BackendSource::Concrete {
                table: "left_table".to_string(),
                column: "id".to_string(),
            }]
        ),
        other => panic!("expected catalog-resolved join column, got {other:?}"),
    }
}

#[test]
fn sqllineage_column_without_visible_binding_reports_not_found() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement("select id", None, &outputs, &BTreeSet::new());

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::NotFound
            );
            assert!(matches!(
                failure.error.kind,
                BackendErrorKind::ColumnResolution {
                    state: super::super::backend::ResolutionState::NotFound
                }
            ));
        }
        other => panic!("expected unresolved column, got {other:?}"),
    }
}

#[test]
fn sqllineage_unknown_star_is_indeterminate_and_sets_star_flag() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement("select * from orders", None, &outputs, &BTreeSet::new());

    assert!(statement.has_unresolved_stars);
    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::Indeterminate
            );
        }
        other => panic!("expected unresolved star, got {other:?}"),
    }
}

#[test]
fn sqllineage_unmapped_output_depends_on_unresolved_star_state() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "missing".to_string(),
    }];

    let without_star =
        sqllineage_statement("select id from orders", None, &outputs, &BTreeSet::new());
    match sqllineage_outcome(&without_star, 0) {
        BackendColumnOutcome::Failed(failure) => assert_eq!(
            failure.resolution,
            super::super::backend::ResolutionState::NotFound
        ),
        other => panic!("expected missing output without a star to be not found, got {other:?}"),
    }

    let with_unexpanded_star =
        sqllineage_statement("select * from orders", None, &outputs, &BTreeSet::new());
    match sqllineage_outcome(&with_unexpanded_star, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::Indeterminate
            );
            assert!(
                failure
                    .error
                    .message
                    .contains("unexpanded SELECT * leaves the output columns unknown")
            );
        }
        other => panic!(
            "expected missing output with an unexpanded star to be indeterminate, got {other:?}"
        ),
    }
}

#[test]
fn sqllineage_catalog_expands_star_to_concrete_sources() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("orders", ["id".to_string(), "amount".to_string()]);
    let outputs = [
        OutputColumnRequest {
            slot: AnalysisSlot(4),
            name: "ID".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(9),
            name: "amount".to_string(),
        },
    ];
    let statement = sqllineage_statement(
        "select * from ORDERS",
        Some(&catalog),
        &outputs,
        &BTreeSet::new(),
    );

    assert!(!statement.has_unresolved_stars);
    match sqllineage_outcome(&statement, 4) {
        BackendColumnOutcome::Resolved(result) => assert_eq!(
            result.sources,
            vec![BackendSource::Concrete {
                table: "orders".to_string(),
                column: "id".to_string(),
            }]
        ),
        other => panic!("expected catalog-resolved star column, got {other:?}"),
    }
}

#[test]
fn sqllineage_returns_failed_outcome_for_unmapped_requested_output() {
    let outputs = [
        OutputColumnRequest {
            slot: AnalysisSlot(12),
            name: "id".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(27),
            name: "missing".to_string(),
        },
    ];
    let statement = sqllineage_statement("select id from orders", None, &outputs, &BTreeSet::new());

    assert_eq!(statement.columns.len(), outputs.len());
    match sqllineage_outcome(&statement, 27) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(failure.target.name, "missing");
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::NotFound
            );
        }
        other => panic!("expected missing output to fail, got {other:?}"),
    }
}

#[test]
fn sqllineage_duplicate_output_name_fails_as_ambiguous() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "value".to_string(),
    }];
    let duplicates = BTreeSet::from(["value".to_string()]);
    let statement = sqllineage_statement(
        "select id as value, amount as value from orders",
        None,
        &outputs,
        &duplicates,
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::Ambiguous
            );
            assert!(failure.error.message.contains("output name is duplicated"));
        }
        other => panic!("expected duplicate output failure, got {other:?}"),
    }
}

#[test]
fn sqllineage_parse_failure_is_parse_error() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "value".to_string(),
    }];
    let request = LineageRequest {
        sql: "select * from",
        dialect: DlinDialect::Generic,
        catalog: None,
        outputs: &outputs,
        duplicate_output_names: &BTreeSet::new(),
    };
    let backend = backend_for_tests(BackendId::Sqllineage);

    let error = backend.analyze(&request).unwrap_err();
    assert_eq!(error.kind, BackendErrorKind::Parse);
}

#[test]
fn sqllineage_unsupported_dialect_is_reported() {
    let request = LineageRequest {
        sql: "SELECT 1",
        dialect: DlinDialect::DuckDB,
        catalog: None,
        outputs: &[],
        duplicate_output_names: &BTreeSet::new(),
    };
    let backend = backend_for_tests(BackendId::Sqllineage);

    let error = backend.analyze(&request).unwrap_err();
    assert_eq!(error.kind, BackendErrorKind::UnsupportedDialect);
    assert!(error.message.contains("duckdb"));
}

fn sqllineage_discovery(
    sql: &str,
    catalog: Option<&CatalogSnapshot>,
) -> Result<OutputDiscovery, super::super::backend::BackendError> {
    let request = OutputDiscoveryRequest {
        sql,
        dialect: DlinDialect::Generic,
        catalog,
    };
    backend_for_tests(BackendId::Sqllineage).discover_output_columns(&request)
}

fn discovered_names(discovery: &OutputDiscovery) -> Vec<String> {
    discovery
        .outputs
        .iter()
        .map(|output| match &output.name {
            OutputName::Named(name) => name.clone(),
            OutputName::UnaliasedExpression => {
                panic!("sqllineage discovery produced an unaliased expression")
            }
        })
        .collect()
}

#[test]
fn sqllineage_discovery_plain_projection_preserves_order_and_aliases() {
    let discovery =
        sqllineage_discovery("SELECT id, amount AS total, status FROM orders", None).unwrap();

    assert_eq!(discovered_names(&discovery), ["id", "total", "status"]);
}

#[test]
fn sqllineage_discovery_top_level_set_operation_uses_projection_names() {
    let discovery = sqllineage_discovery(
        "SELECT id, amount AS total FROM first_source UNION ALL SELECT id, amount AS other FROM second_source",
        None,
    )
    .unwrap();

    assert_eq!(discovered_names(&discovery), ["id", "total"]);
}

#[test]
fn sqllineage_discovery_unexpanded_star_has_no_name() {
    let discovery = sqllineage_discovery("SELECT * FROM orders", None).unwrap();

    assert!(discovery.outputs.is_empty());
    assert!(!discovery.duplicate_names.contains("*"));
}

#[test]
fn sqllineage_discovery_preserves_quoted_star_output_name() {
    let discovery = sqllineage_discovery("SELECT id AS \"*\" FROM orders", None).unwrap();

    assert_eq!(discovered_names(&discovery), ["*"]);
}

#[test]
fn sqllineage_discovery_expands_star_from_catalog() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("orders", ["id".to_string(), "amount".to_string()]);

    let discovery = sqllineage_discovery("SELECT * FROM orders", Some(&catalog)).unwrap();

    assert_eq!(discovered_names(&discovery), ["id", "amount"]);
}

#[test]
fn sqllineage_discovery_cte_uses_outer_projection() {
    let discovery = sqllineage_discovery(
        "WITH base AS (SELECT id, amount AS inner_amount FROM orders) SELECT inner_amount AS outer_amount FROM base",
        None,
    )
    .unwrap();

    assert_eq!(discovered_names(&discovery), ["outer_amount"]);
}

#[test]
fn sqllineage_discovery_records_duplicate_names() {
    let discovery =
        sqllineage_discovery("SELECT id AS value, amount AS value FROM orders", None).unwrap();

    assert_eq!(discovered_names(&discovery), ["value", "value"]);
    assert_eq!(
        discovery.duplicate_names,
        BTreeSet::from(["value".to_string()])
    );
}

#[test]
fn sqllineage_discovery_parse_failure_is_parse_error() {
    let error = sqllineage_discovery("SELECT * FROM", None).unwrap_err();

    assert_eq!(error.kind, BackendErrorKind::Parse);
}

#[test]
fn sqllineage_discovery_matches_analyze_mapping_targets() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table(
        "orders",
        ["id".to_string(), "amount".to_string(), "status".to_string()],
    );
    let sql = "SELECT id, amount AS total, status FROM orders";

    let discovery = sqllineage_discovery(sql, Some(&catalog)).unwrap();
    let discovered = discovered_names(&discovery);
    let provider = SqllineageCatalogProvider::new(&catalog, DlinDialect::Generic);
    let analyzed = sqllineage::analyze(
        sql,
        sqllineage::AnalyzeOptions {
            dialect: DlinDialect::Generic.to_sqllineage().unwrap(),
            catalog: Some(Box::new(provider)),
            normalize_case: true,
        },
    )
    .unwrap();
    let analyzed_names: Vec<String> = analyzed[0]
        .columns
        .mappings
        .iter()
        .map(|mapping| mapping.target.column.clone())
        .filter(|name| name != "*")
        .collect();

    assert_eq!(discovered, analyzed_names);
}
