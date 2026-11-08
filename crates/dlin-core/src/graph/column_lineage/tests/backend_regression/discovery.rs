use super::*;

#[test]
fn bigquery_temporal_date_parts_are_not_columns() {
    let result = analyze(
        "SELECT DATE_TRUNC(event_date, WEEK(MONDAY)) AS monday_start, DATE_DIFF(event_date, other_date, ISOWEEK) AS week_diff, TIMESTAMP_TRUNC(event_ts, ISOYEAR, tz_name) AS timestamp_year FROM events",
        DlinDialect::BigQuery,
        &[(
            "events",
            &["event_date", "other_date", "event_ts", "tz_name"],
        )],
        &[],
    );
    assert_no_errors(&result);
    assert_eq!(result.total_columns, 3);
    assert_eq!(result.traced_columns, 3);
    let expected: [(&str, &[(&str, &str)]); 3] = [
        ("monday_start", &[("events", "event_date")]),
        (
            "week_diff",
            &[("events", "event_date"), ("events", "other_date")],
        ),
        (
            "timestamp_year",
            &[("events", "event_ts"), ("events", "tz_name")],
        ),
    ];
    for (name, sources) in expected {
        let traced = entry(&result, name);
        assert_eq!(source_pairs(traced), sources, "sources for {name}");
        assert!(
            traced
                .lineage
                .known()
                .iter()
                .all(|source| !matches!(source.column.as_str(), "MONDAY" | "ISOWEEK" | "ISOYEAR")),
            "date parts must not be reported as columns: {traced:?}"
        );
    }
}

#[test]
fn unmapped_requested_output_fails_without_affecting_others() {
    let result = generic("select id from orders", &["id", "missing"]);
    assert_eq!(result.total_columns, 2);
    assert_eq!(result.traced_columns, 1);
    assert_resolved(
        &result,
        "id",
        &[("orders", "id")],
        TransformationType::Direct,
    );
    assert_column_error(
        &result,
        "missing",
        ColumnLineageErrorKind::ColumnNotFound,
        "no sqllineage mapping for output 'missing'",
    );
}

#[test]
fn duplicate_output_name_fails_as_ambiguous() {
    // The name is both declared in the YAML and produced twice by the SQL; it
    // is still one column, and it cannot be told apart.
    let result = generic(
        "select id as value, amount as value from orders",
        &["value"],
    );
    assert_eq!(result.total_columns, 1);
    assert_eq!(result.traced_columns, 0);
    assert_column_error(
        &result,
        "value",
        ColumnLineageErrorKind::ColumnAmbiguous,
        "more than one sqllineage mapping matches output 'value'",
    );
}

#[test]
fn parse_failure_is_parse_failure() {
    let result = generic("select * from", &["value"]);
    assert_parse_failure(&result, 1, "");
}

#[test]
fn empty_projection_is_parse_failure() {
    let result = generic("select from from", &["value"]);
    assert_parse_failure(&result, 1, "dlin found a SELECT with an empty projection");
}

#[test]
fn empty_projection_in_set_operation_branch_is_parse_failure() {
    let result = generic("select 1 union all select from foo", &["value"]);
    assert_parse_failure(&result, 1, "dlin found a SELECT with an empty projection");
}

#[test]
fn empty_projection_in_nested_subquery_is_parse_failure() {
    let result = generic("select id from (select from foo) as t", &["id"]);
    assert_parse_failure(&result, 1, "dlin found a SELECT with an empty projection");
}

#[test]
fn unsupported_dialect_is_reported_as_parse_failure() {
    let result = analyze("SELECT 1", DlinDialect::Presto, &[], &["value"]);
    assert_parse_failure(&result, 1, "presto");
}

#[test]
fn discovery_plain_projection_names_outputs_by_alias() {
    let result = generic("SELECT id, amount AS total, status FROM orders", &[]);
    assert_no_errors(&result);
    // Columns are reported in sorted order, not in projection order.
    assert_eq!(column_names(&result), ["id", "status", "total"]);
    assert_eq!(result.total_columns, 3);
    assert_resolved(
        &result,
        "total",
        &[("orders", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_top_level_set_operation_uses_projection_names() {
    let result = generic(
        "SELECT id, amount AS total FROM first_source UNION ALL SELECT id, amount AS other FROM second_source",
        &[],
    );
    assert_no_errors(&result);
    assert_eq!(column_names(&result), ["id", "total"]);
    assert_resolved(
        &result,
        "total",
        &[("first_source", "amount"), ("second_source", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_unexpanded_star_has_no_name() {
    let result = generic("SELECT * FROM orders", &[]);
    assert_column_inference_failed(&result);
}

#[test]
fn discovery_preserves_quoted_star_output_name() {
    let result = generic(r#"SELECT id AS "*" FROM orders"#, &[]);
    assert_no_errors(&result);
    assert_eq!(column_names(&result), ["*"]);
    assert_resolved(
        &result,
        "*",
        &[("orders", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_drops_field_path_star_but_keeps_explicit_star_alias() {
    let result = analyze(
        r#"SELECT id AS "*", base.event.* FROM source AS base"#,
        DlinDialect::BigQuery,
        &[],
        &[],
    );
    assert_eq!(column_names(&result), ["*"]);
    assert_eq!(result.total_columns, 1);
    assert_resolved(
        &result,
        "*",
        &[("source", "id")],
        TransformationType::Direct,
    );
    assert_no_errors(&result);
}

#[test]
fn discovery_drops_field_path_star_without_forged_output_name() {
    let parsed = sqllineage::parse(
        "SELECT base.event.* FROM source AS base",
        DlinDialect::BigQuery.to_engine_dialect().unwrap(),
    )
    .unwrap();
    let analyzed = sqllineage::Analyzer::new(DlinDialect::BigQuery.to_engine_dialect().unwrap());
    let statement = parsed.analyze(&analyzed).into_single().unwrap();
    assert!(statement.output.columns.is_empty());
    let sqllineage::Shape::Open { unexpanded } = &statement.output.shape else {
        panic!("expression field-path star should leave the output shape open");
    };
    assert_eq!(unexpanded.len(), 1);
    assert!(unexpanded[0].relation.is_none());
    // The field itself stays a known source of the star, and the expansion is
    // marked as unknown.
    let sqllineage::Lineage::Partial { known, unresolved } = &unexpanded[0].lineage else {
        panic!("expected partial lineage, got {:?}", unexpanded[0].lineage);
    };
    assert_eq!(
        known
            .iter()
            .map(|source| (
                source.relation.name.value.as_str(),
                source.column.value.as_str()
            ))
            .collect::<Vec<_>>(),
        [("source", "event")]
    );
    assert_eq!(
        unresolved.as_slice(),
        [sqllineage::Unresolved::Unsupported {
            what: "star expansion"
        }]
    );

    let result = analyze(
        "SELECT base.event.* FROM source AS base",
        DlinDialect::BigQuery,
        &[],
        &[],
    );
    assert_column_inference_failed(&result);
}

#[test]
fn discovery_expands_star_from_known_schema() {
    let result = analyze(
        "SELECT * FROM orders",
        DlinDialect::Generic,
        &[("orders", &["id", "amount"])],
        &[],
    );
    assert_no_errors(&result);
    // Columns are reported in sorted order.
    assert_eq!(column_names(&result), ["amount", "id"]);
    assert_resolved(
        &result,
        "amount",
        &[("orders", "amount")],
        TransformationType::Direct,
    );
    assert_resolved(
        &result,
        "id",
        &[("orders", "id")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_cte_uses_outer_projection() {
    let result = generic(
        "WITH base AS (SELECT id, amount AS inner_amount FROM orders) SELECT inner_amount AS outer_amount FROM base",
        &[],
    );
    assert_no_errors(&result);
    assert_eq!(column_names(&result), ["outer_amount"]);
    assert_resolved(
        &result,
        "outer_amount",
        &[("orders", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_records_duplicate_names() {
    let result = generic("SELECT id AS value, amount AS value FROM orders", &[]);
    // The duplicated name is listed once and cannot be traced.
    assert_eq!(result.total_columns, 1);
    assert_eq!(result.traced_columns, 0);
    assert_column_error(
        &result,
        "value",
        ColumnLineageErrorKind::ColumnAmbiguous,
        "more than one sqllineage mapping matches output 'value'",
    );
}

#[test]
fn discovery_parse_failure_does_not_name_columns() {
    // Discovery cannot name columns of SQL that does not parse; without YAML
    // columns the model's outputs are undetermined rather than a parse error.
    let result = generic("SELECT * FROM", &[]);
    assert_column_inference_failed(&result);
}

#[test]
fn discovered_names_are_all_traced_through_a_known_schema() {
    let result = analyze(
        "SELECT id, amount AS total, status FROM orders",
        DlinDialect::Generic,
        &[("orders", &["id", "amount", "status"])],
        &[],
    );
    assert_no_errors(&result);
    assert_eq!(column_names(&result), ["id", "status", "total"]);
    assert_eq!(result.total_columns, result.traced_columns);
    assert_resolved(
        &result,
        "id",
        &[("orders", "id")],
        TransformationType::Direct,
    );
    assert_resolved(
        &result,
        "status",
        &[("orders", "status")],
        TransformationType::Direct,
    );
    assert_resolved(
        &result,
        "total",
        &[("orders", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_groups_sql_names_without_changing_display_names() {
    for dialect in [
        DlinDialect::Generic,
        DlinDialect::Snowflake,
        DlinDialect::PostgreSQL,
        DlinDialect::BigQuery,
        DlinDialect::ClickHouse,
    ] {
        for (sql, names, duplicate) in [
            (
                "SELECT a AS Foo, b AS foo FROM t",
                ["Foo", "foo"],
                dialect != DlinDialect::ClickHouse,
            ),
            (
                r#"SELECT a AS id, b AS "ID" FROM t"#,
                ["id", "ID"],
                !matches!(dialect, DlinDialect::PostgreSQL | DlinDialect::ClickHouse),
            ),
            (
                r#"SELECT a AS id, b AS "id" FROM t"#,
                ["id", "id"],
                !matches!(dialect, DlinDialect::Generic | DlinDialect::Snowflake),
            ),
        ] {
            let context = format!("{dialect:?}: {sql}");
            let result = analyze(sql, dialect, &[], &[]);
            // Display names are kept as written, and a name used by several
            // outputs is listed once.
            let mut expected_names = names.to_vec();
            expected_names.sort();
            expected_names.dedup();
            assert_eq!(result.total_columns, expected_names.len(), "{context}");
            let mut reported: Vec<&str> = column_names(&result);
            reported.extend(
                result
                    .errors
                    .iter()
                    .map(|error| error.column.as_deref().expect("column-scoped error")),
            );
            reported.sort();
            assert_eq!(reported, expected_names, "{context}");
            if duplicate {
                // Outputs sharing one SQL identifier cannot be told apart.
                assert_eq!(result.traced_columns, 0, "{context}");
                for name in &expected_names {
                    assert_column_error(
                        &result,
                        name,
                        ColumnLineageErrorKind::ColumnAmbiguous,
                        &format!("more than one sqllineage mapping matches output '{name}'"),
                    );
                }
            } else if names[0] == names[1] {
                // Distinct SQL identifiers that display alike.
                assert_eq!(result.traced_columns, 0, "{context}");
                assert_column_error(
                    &result,
                    names[0],
                    ColumnLineageErrorKind::ColumnAmbiguous,
                    "distinct SQL identifiers share the same display name",
                );
            } else {
                assert_no_errors(&result);
                assert_eq!(result.traced_columns, 2, "{context}");
            }
        }
    }
}
