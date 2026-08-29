#[test]
fn sqllineage_unknown_set_operation_outputs_are_indeterminate() {
    let cases = [
        (
            "SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source",
            ["id", "explicit_col"].as_slice(),
        ),
        (
            "SELECT id, 1 AS explicit_col FROM raw.orders UNION SELECT id, * FROM some_unknown_source",
            ["explicit_col"].as_slice(),
        ),
        (
            "WITH u AS (SELECT * FROM first_source UNION ALL SELECT id, explicit_col FROM second_source) SELECT id, explicit_col FROM u",
            ["id", "explicit_col"].as_slice(),
        ),
    ];
    for (sql, names) in cases {
        let outputs: Vec<_> = names
            .iter()
            .enumerate()
            .map(|(slot, name)| OutputColumnRequest {
                slot: AnalysisSlot(slot),
                name: (*name).to_string(),
            })
            .collect();
        let statement = sqllineage_statement(sql, None, &outputs, &BTreeSet::new());
        assert_eq!(statement.completeness, AnalysisCompleteness::Complete);
        for slot in 0..outputs.len() {
            match sqllineage_outcome(&statement, slot) {
                BackendColumnOutcome::Failed(failure) => assert_eq!(
                    failure.resolution,
                    super::super::backend::ResolutionState::Indeterminate,
                    "SQL: {sql}, slot: {slot}"
                ),
                other => {
                    panic!("expected output {slot} to be indeterminate for {sql}, got {other:?}")
                }
            }
        }
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
fn sqllineage_leading_name_with_unresolved_aligned_branch_is_indeterminate() {
    // c1 is correctly named by the leading operand and therefore exists. Only
    // its lineage is incomplete because the ordinally aligned a.col_x in the
    // other branch cannot resolve through the unknown-schema SELECT * CTE.
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
            super::super::backend::ResolutionState::Indeterminate
        ),
        other => panic!("expected unresolved output to be indeterminate, got {other:?}"),
    }
}

#[test]
fn sqllineage_nonleading_set_output_name_through_cte_is_indeterminate() {
    // Through a CTE, sqllineage still produces a mapping targeting c9 even though
    // the set operation's only output is named c1 by its leading operand, so it
    // cannot tell us the output is absent. Indeterminate is as far as dlin can
    // honestly go: NotFound would claim an absence the engine has not established.
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
            super::super::backend::ResolutionState::Indeterminate
        ),
        other => panic!("expected unresolved output to be indeterminate, got {other:?}"),
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
        "SELECT 1 AS c1 UNION ALL SELECT 2 AS c9",
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
fn sqllineage_duplicate_output_ambiguity_takes_precedence() {
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
                    relation: RelationRef::bare("source_table"),
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
                relation: RelationRef::from_manifest(None, None, "left_table"),
                column: "id".to_string(),
            }]
        ),
        other => panic!("expected catalog-resolved join column, got {other:?}"),
    }
}

#[test]
fn sqllineage_preserve_case_does_not_restore_differently_cased_catalog_relation() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("MixedCaseTable", ["id".to_string()]);
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement_with_dialect(
        "select id from mixedcasetable",
        DlinDialect::MySQL,
        Some(&catalog),
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Resolved(result) => assert_eq!(
            result.sources,
            vec![BackendSource::Concrete {
                relation: RelationRef::from_backend(None, None, "mixedcasetable"),
                column: "id".to_string(),
            }]
        ),
        other => panic!("expected raw-spelling source, got {other:?}"),
    }
}

#[test]
fn sqllineage_column_without_visible_binding_reports_indeterminate() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "id".to_string(),
    }];
    let statement = sqllineage_statement("select id", None, &outputs, &BTreeSet::new());

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Failed(failure) => {
            assert_eq!(
                failure.resolution,
                super::super::backend::ResolutionState::Indeterminate
            );
            assert!(matches!(
                failure.error.kind,
                BackendErrorKind::ColumnResolution {
                    state: super::super::backend::ResolutionState::Indeterminate
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
                relation: RelationRef::from_manifest(None, None, "orders"),
                column: "id".to_string(),
            }]
        ),
        other => panic!("expected catalog-resolved star column, got {other:?}"),
    }
}

#[test]
fn sqllineage_catalog_set_operation_with_literal_branch_keeps_concrete_sources() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("ext_a", ["col_x".to_string(), "col_y".to_string()]);
    let outputs = [
        OutputColumnRequest {
            slot: AnalysisSlot(0),
            name: "col_x".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(1),
            name: "col_y".to_string(),
        },
    ];
    let statement = sqllineage_statement(
        "SELECT * FROM ext_a UNION ALL SELECT 1 AS col_x, 2 AS col_y",
        Some(&catalog),
        &outputs,
        &BTreeSet::new(),
    );

    for (slot, column) in [(0, "col_x"), (1, "col_y")] {
        match sqllineage_outcome(&statement, slot) {
            BackendColumnOutcome::Resolved(result) => {
                assert_eq!(result.resolution, ResolutionState::Resolved);
                assert_eq!(result.sources.len(), 1);
                assert_eq!(
                    result.sources[0],
                    BackendSource::Concrete {
                        relation: RelationRef::from_manifest(None, None, "ext_a"),
                        column: column.to_string(),
                    }
                );
            }
            other => panic!("expected concrete branch to resolve, got {other:?}"),
        }
    }
}

#[test]
fn sqllineage_source_free_branch_does_not_discard_concrete_branch() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("ext_a", ["col_x".to_string(), "col_y".to_string()]);
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "col_a".to_string(),
    }];
    let statement = sqllineage_statement(
        "WITH lit AS (SELECT 1 AS col_a), u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM ext_a) SELECT col_a FROM u",
        Some(&catalog),
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Resolved(result) => {
            assert_eq!(result.resolution, ResolutionState::Resolved);
            assert_eq!(
                result.sources,
                vec![BackendSource::Concrete {
                    relation: RelationRef::from_manifest(None, None, "ext_a"),
                    column: "col_x".to_string(),
                }]
            );
        }
        other => panic!("expected concrete branch to resolve, got {other:?}"),
    }
}

#[test]
fn sqllineage_source_free_only_branch_resolves_without_sources() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "col_a".to_string(),
    }];
    let statement = sqllineage_statement(
        "SELECT CAST(NULL AS STRING) AS col_a",
        None,
        &outputs,
        &BTreeSet::new(),
    );

    match sqllineage_outcome(&statement, 0) {
        BackendColumnOutcome::Resolved(result) => {
            assert_eq!(result.resolution, ResolutionState::Resolved);
            assert!(result.sources.is_empty(), "unexpected sources: {result:?}");
        }
        other => panic!("expected source-free output to resolve without sources, got {other:?}"),
    }
}
