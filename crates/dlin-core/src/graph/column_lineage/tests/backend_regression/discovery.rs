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
    assert_counts(&result, 3, 3, 0);
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
        assert_source_pairs(traced, sources, name);
        assert!(
            traced.lineage.known().iter().all(|source| !matches!(
                source.column.value.as_str(),
                "MONDAY" | "ISOWEEK" | "ISOYEAR"
            )),
            "date parts must not be reported as columns: {traced:?}"
        );
    }
}

#[test]
fn declared_only_column_does_not_affect_the_listed_outputs() {
    let result = generic("select id from orders", &["id", "missing"]);
    assert_counts(&result, 1, 1, 0);
    assert_resolved(
        &result,
        "id",
        &[("orders", "id")],
        TransformationType::Direct,
    );
    assert_declared_difference(
        &result,
        ColumnLineageErrorKind::DeclaredColumnNotInOutput,
        "missing",
        "is declared in YAML but is not in the SQL output",
    );
    assert_eq!(result.errors.len(), 1, "errors: {:?}", result.errors);
}

#[test]
fn duplicate_output_names_stay_separate_slots() {
    // The name is both declared in the YAML and produced twice by the SQL.
    // Each output is its own slot with its own lineage; nothing is reported
    // because the SQL itself is valid.
    let result = generic(
        "select id as value, amount as value from orders",
        &["value"],
    );
    assert_no_errors(&result);
    assert_counts(&result, 2, 2, 0);
    let slots = result.slots();
    assert_eq!(result.names(), ["value", "value"]);
    assert_ne!(slots[0].id, slots[1].id);
    assert_source_pairs(&slots[0], &[("orders", "id")], "first value");
    assert_source_pairs(&slots[1], &[("orders", "amount")], "second value");
}

#[test]
fn parse_failure_is_parse_failure() {
    let result = generic("select * from", &["value"]);
    assert_parse_failure(&result, "");
}

#[test]
fn empty_projection_is_parse_failure() {
    let result = generic("select from from", &["value"]);
    assert_parse_failure(&result, "dlin found a SELECT with an empty projection");
}

#[test]
fn empty_projection_in_set_operation_branch_is_parse_failure() {
    let result = generic("select 1 union all select from foo", &["value"]);
    assert_parse_failure(&result, "dlin found a SELECT with an empty projection");
}

#[test]
fn empty_projection_in_nested_subquery_is_parse_failure() {
    let result = generic("select id from (select from foo) as t", &["id"]);
    assert_parse_failure(&result, "dlin found a SELECT with an empty projection");
}

#[test]
fn unsupported_dialect_is_reported_as_parse_failure() {
    let result = analyze("SELECT 1", DlinDialect::Presto, &[], &["value"]);
    assert_parse_failure(&result, "presto");
}

#[test]
fn discovery_plain_projection_names_outputs_by_alias() {
    let result = generic("SELECT id, amount AS total, status FROM orders", &[]);
    assert_no_errors(&result);
    // Columns are listed in projection order, not sorted.
    assert_eq!(result.names(), ["id", "total", "status"]);
    assert_counts(&result, 3, 3, 0);
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
    assert_eq!(result.names(), ["id", "total"]);
    assert_resolved(
        &result,
        "total",
        &[("first_source", "amount"), ("second_source", "amount")],
        TransformationType::Direct,
    );
}

#[test]
fn discovery_unexpanded_star_lists_no_column_and_leaves_the_shape_open() {
    let result = generic("SELECT * FROM orders", &[]);
    assert_no_errors(&result);
    assert_counts(&result, 0, 0, 0);
    let output = result.output.as_ref().expect("the SQL was analysed");
    assert!(output.columns.is_empty());
    assert!(matches!(&output.shape, OutputShape::Open { unexpanded } if unexpanded.len() == 1));
}

#[test]
fn discovery_preserves_quoted_star_output_name() {
    let result = generic(r#"SELECT id AS "*" FROM orders"#, &[]);
    assert_no_errors(&result);
    // The quoted spelling is part of the output name.
    assert_eq!(result.names(), [r#""*""#]);
    assert_eq!(
        result.slots()[0].name,
        OutputName::Explicit(Ident::quoted("*", Quote::Double))
    );
    assert_resolved(
        &result,
        r#""*""#,
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
    assert_eq!(result.names(), [r#""*""#]);
    assert_counts(&result, 1, 1, 0);
    assert_resolved(
        &result,
        r#""*""#,
        &[("source", "id")],
        TransformationType::Direct,
    );
    assert!(matches!(
        &result.output.as_ref().unwrap().shape,
        OutputShape::Open { unexpanded } if unexpanded.len() == 1
    ));
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
    assert_counts(&result, 0, 0, 0);
    assert!(result.slots().is_empty());
    assert_no_errors(&result);
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
    assert_eq!(result.names(), ["id", "amount"]);
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
    assert_eq!(result.names(), ["outer_amount"]);
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
    // Both outputs are listed, in SQL order.
    assert_counts(&result, 2, 2, 0);
    assert_eq!(result.names(), ["value", "value"]);
    assert_no_errors(&result);
}

#[test]
fn parse_failure_has_no_output_even_without_yaml_columns() {
    let result = generic("SELECT * FROM", &[]);
    assert_parse_failure(&result, "");
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
    assert_eq!(result.names(), ["id", "total", "status"]);
    assert_counts(&result, 3, 3, 0);
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
fn outputs_keep_their_spelling_and_are_told_apart_by_the_dialect_rules() {
    for dialect in [
        DlinDialect::Generic,
        DlinDialect::Snowflake,
        DlinDialect::PostgreSQL,
        DlinDialect::BigQuery,
        DlinDialect::ClickHouse,
    ] {
        for (sql, names, shared_key) in [
            (
                "SELECT a AS Foo, b AS foo FROM t",
                ["Foo", "foo"],
                dialect != DlinDialect::ClickHouse,
            ),
            (
                r#"SELECT a AS id, b AS "ID" FROM t"#,
                ["id", r#""ID""#],
                !matches!(dialect, DlinDialect::PostgreSQL | DlinDialect::ClickHouse),
            ),
            (
                r#"SELECT a AS id, b AS "id" FROM t"#,
                ["id", r#""id""#],
                !matches!(dialect, DlinDialect::Generic | DlinDialect::Snowflake),
            ),
        ] {
            let context = format!("{dialect:?}: {sql}");
            let result = analyze(sql, dialect, &[], &[]);
            // Both outputs are listed in SQL order, spelled as written, and
            // none is an error.
            assert_no_errors(&result);
            assert_eq!(result.names(), names, "{context}");
            assert_counts(&result, 2, 2, 0);
            // Looking either name up finds both outputs exactly when the
            // dialect gives them the same column key.
            let output = result.output.as_ref().unwrap();
            for slot in &output.columns {
                let ident = slot.name.ident().expect("named output");
                let lookup = output.lookup(ident, dialect);
                if shared_key {
                    assert_eq!(
                        lookup,
                        SlotLookup::Ambiguous(vec![output.columns[0].id, output.columns[1].id]),
                        "{context}"
                    );
                } else {
                    assert_eq!(lookup, SlotLookup::Unique(slot.id), "{context}");
                }
            }
        }
    }
}
