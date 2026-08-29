#[test]
fn sqllineage_bigquery_temporal_date_parts_are_not_columns() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table(
        "events",
        [
            "event_date".to_string(),
            "other_date".to_string(),
            "event_ts".to_string(),
            "tz_name".to_string(),
        ],
    );
    let outputs = [
        OutputColumnRequest {
            slot: AnalysisSlot(0),
            name: "monday_start".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(1),
            name: "week_diff".to_string(),
        },
        OutputColumnRequest {
            slot: AnalysisSlot(2),
            name: "timestamp_year".to_string(),
        },
    ];
    let statement = sqllineage_statement_with_dialect(
        "SELECT DATE_TRUNC(event_date, WEEK(MONDAY)) AS monday_start, DATE_DIFF(event_date, other_date, ISOWEEK) AS week_diff, TIMESTAMP_TRUNC(event_ts, ISOYEAR, tz_name) AS timestamp_year FROM events",
        DlinDialect::BigQuery,
        Some(&catalog),
        &outputs,
        &BTreeSet::new(),
    );
    let expected = [
        (
            "monday_start",
            vec![BackendSource::Concrete {
                relation: RelationRef::from_manifest(None, None, "events"),
                column: "event_date".to_string(),
            }],
        ),
        (
            "week_diff",
            vec![
                BackendSource::Concrete {
                    relation: RelationRef::from_manifest(None, None, "events"),
                    column: "event_date".to_string(),
                },
                BackendSource::Concrete {
                    relation: RelationRef::from_manifest(None, None, "events"),
                    column: "other_date".to_string(),
                },
            ],
        ),
        (
            "timestamp_year",
            vec![
                BackendSource::Concrete {
                    relation: RelationRef::from_manifest(None, None, "events"),
                    column: "event_ts".to_string(),
                },
                BackendSource::Concrete {
                    relation: RelationRef::from_manifest(None, None, "events"),
                    column: "tz_name".to_string(),
                },
            ],
        ),
    ];

    for (slot, (name, expected_sources)) in expected.into_iter().enumerate() {
        match sqllineage_outcome(&statement, slot) {
            BackendColumnOutcome::Resolved(result) => {
                assert_eq!(result.target.name, name);
                assert_eq!(result.resolution, ResolutionState::Resolved);
                assert_eq!(result.sources, expected_sources);
                assert!(result.sources.iter().all(|source| match source {
                    BackendSource::Concrete { column, .. } => {
                        !matches!(column.as_str(), "MONDAY" | "ISOWEEK" | "ISOYEAR")
                    }
                    _ => false,
                }));
            }
            other => panic!("expected resolved BigQuery output {name}, got {other:?}"),
        }
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
fn sqllineage_empty_projection_is_parse_error() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "value".to_string(),
    }];
    let request = LineageRequest {
        sql: "select from from",
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
fn sqllineage_empty_projection_in_set_operation_branch_is_parse_error() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "value".to_string(),
    }];
    let request = LineageRequest {
        sql: "select 1 union all select from foo",
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
fn sqllineage_empty_projection_in_nested_subquery_is_parse_error() {
    let outputs = [OutputColumnRequest {
        slot: AnalysisSlot(0),
        name: "value".to_string(),
    }];
    let request = LineageRequest {
        sql: "select id from (select from foo) as t",
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
        dialect: DlinDialect::Presto,
        catalog: None,
        outputs: &[],
        duplicate_output_names: &BTreeSet::new(),
    };
    let backend = backend_for_tests(BackendId::Sqllineage);

    let error = backend.analyze(&request).unwrap_err();
    assert_eq!(error.kind, BackendErrorKind::UnsupportedDialect);
    assert!(error.message.contains("presto"));
}

fn sqllineage_discovery(
    sql: &str,
    catalog: Option<&CatalogSnapshot>,
) -> Result<OutputDiscovery, super::super::backend::BackendError> {
    sqllineage_discovery_with_dialect(sql, DlinDialect::Generic, catalog)
}

fn sqllineage_discovery_with_dialect(
    sql: &str,
    dialect: DlinDialect,
    catalog: Option<&CatalogSnapshot>,
) -> Result<OutputDiscovery, super::super::backend::BackendError> {
    let request = OutputDiscoveryRequest {
        sql,
        dialect,
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
    let discovery = sqllineage_discovery(r#"SELECT id AS "*" FROM orders"#, None).unwrap();

    assert_eq!(discovered_names(&discovery), ["*"]);
}

#[test]
fn sqllineage_discovery_drops_field_path_star_but_keeps_explicit_star_alias() {
    let discovery = sqllineage_discovery_with_dialect(
        r#"SELECT id AS "*", base.event.* FROM source AS base"#,
        DlinDialect::BigQuery,
        None,
    )
    .unwrap();

    assert_eq!(discovered_names(&discovery), ["*"]);
    assert!(!discovery.duplicate_names.contains("*"));
}

#[test]
fn sqllineage_discovery_drops_field_path_star_without_forged_output_name() {
    let analyzed = sqllineage::analyze(
        "SELECT base.event.* FROM source AS base",
        sqllineage::AnalyzeOptions {
            dialect: DlinDialect::BigQuery.to_sqllineage().unwrap(),
            normalize_case: true,
            ..sqllineage::AnalyzeOptions::default()
        },
    )
    .unwrap();
    assert!(analyzed[0].columns.mappings.iter().any(|mapping| {
        mapping.target.column == "*"
            && mapping.sources.iter().any(|source| {
                matches!(
                    source,
                    sqllineage::ColumnOrigin::Concrete { column, .. } if column == "event"
                )
            })
            && mapping.sources.iter().any(|source| {
                matches!(
                    source,
                    sqllineage::ColumnOrigin::Ambiguous { column, candidates }
                        if column == "*" && candidates.is_empty()
                )
            })
    }));

    let discovery = sqllineage_discovery_with_dialect(
        "SELECT base.event.* FROM source AS base",
        DlinDialect::BigQuery,
        None,
    )
    .unwrap();

    assert!(discovery.outputs.is_empty());
    assert!(!discovery.duplicate_names.contains("*"));
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
