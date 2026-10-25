mod common;

use common::{analyze_one, concrete_sources, find_mapping};
use sqllineage::{
    AnalyzeOptions, CatalogProvider, ColumnOrigin, Dialect, TableRef, TransformKind, analyze,
};

fn analyze_with_dialect(sql: &str, dialect: Dialect) -> sqllineage::AnalyzeResult {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            ..AnalyzeOptions::default()
        },
    )
    .expect("SQL should parse")
    .into_iter()
    .next()
    .unwrap_or_default()
}

#[test]
fn extract_year() {
    let result = analyze_one("SELECT EXTRACT(YEAR FROM hire_date) AS yr FROM t");
    let m = find_mapping(&result.columns.mappings, "yr");
    assert_eq!(concrete_sources(m), vec![("t".into(), "hire_date".into())]);
    assert_eq!(m.transform, TransformKind::Expression);
}

#[test]
fn ceil_expr() {
    let result = analyze_one("SELECT CEIL(price) AS p FROM t");
    let m = find_mapping(&result.columns.mappings, "p");
    assert_eq!(concrete_sources(m), vec![("t".into(), "price".into())]);
}

#[test]
fn floor_expr() {
    let result = analyze_one("SELECT FLOOR(price) AS p FROM t");
    let m = find_mapping(&result.columns.mappings, "p");
    assert_eq!(concrete_sources(m), vec![("t".into(), "price".into())]);
}

#[test]
fn substring_expr() {
    let result = analyze_one("SELECT SUBSTRING(name FROM 1 FOR 3) AS sub FROM t");
    let m = find_mapping(&result.columns.mappings, "sub");
    assert_eq!(concrete_sources(m), vec![("t".into(), "name".into())]);
}

#[test]
fn trim_expr() {
    let result = analyze_one("SELECT TRIM(name) AS n FROM t");
    let m = find_mapping(&result.columns.mappings, "n");
    assert_eq!(concrete_sources(m), vec![("t".into(), "name".into())]);
}

#[test]
fn position_expr() {
    let result = analyze_one("SELECT POSITION('x' IN name) AS pos FROM t");
    let m = find_mapping(&result.columns.mappings, "pos");
    assert_eq!(concrete_sources(m), vec![("t".into(), "name".into())]);
}

#[test]
fn overlay_expr() {
    let result = analyze_one("SELECT OVERLAY(name PLACING 'X' FROM 1 FOR 1) AS o FROM t");
    let m = find_mapping(&result.columns.mappings, "o");
    assert_eq!(concrete_sources(m), vec![("t".into(), "name".into())]);
}

#[test]
fn at_time_zone() {
    let result = analyze_one("SELECT ts AT TIME ZONE 'UTC' AS utc FROM t");
    let m = find_mapping(&result.columns.mappings, "utc");
    assert_eq!(concrete_sources(m), vec![("t".into(), "ts".into())]);
}

#[test]
fn collate_expr() {
    let result = analyze_one("SELECT name COLLATE \"en_US\" AS n FROM t");
    let m = find_mapping(&result.columns.mappings, "n");
    assert_eq!(concrete_sources(m), vec![("t".into(), "name".into())]);
}

#[test]
fn is_true_expr() {
    let result = analyze_one("SELECT active IS TRUE AS flag FROM t");
    let m = find_mapping(&result.columns.mappings, "flag");
    assert_eq!(concrete_sources(m), vec![("t".into(), "active".into())]);
}

#[test]
fn is_distinct_from() {
    let result = analyze_one("SELECT a IS DISTINCT FROM b AS diff FROM t");
    let m = find_mapping(&result.columns.mappings, "diff");
    assert_eq!(
        concrete_sources(m),
        vec![("t".into(), "a".into()), ("t".into(), "b".into())]
    );
}

#[test]
fn like_expr() {
    let result = analyze_one("SELECT name LIKE '%test%' AS matched FROM t");
    let m = find_mapping(&result.columns.mappings, "matched");
    assert_eq!(concrete_sources(m), vec![("t".into(), "name".into())]);
}

#[test]
fn array_expr() {
    let result = analyze_one("SELECT ARRAY[a, b] AS arr FROM t");
    let m = find_mapping(&result.columns.mappings, "arr");
    assert_eq!(
        concrete_sources(m),
        vec![("t".into(), "a".into()), ("t".into(), "b".into())]
    );
}

#[test]
fn json_access() {
    let result = analyze_one("SELECT data->>'key' AS val FROM t");
    let m = find_mapping(&result.columns.mappings, "val");
    assert_eq!(concrete_sources(m), vec![("t".into(), "data".into())]);
}

#[test]
fn qualified_compound_field_access_uses_binding_column() {
    let result = analyze_one("SELECT base.items_array[1] AS item FROM actual_table AS base");
    let m = find_mapping(&result.columns.mappings, "item");
    assert_eq!(
        concrete_sources(m),
        vec![("actual_table".into(), "items_array".into())]
    );
}

#[test]
fn compound_field_access_retains_column_dependent_index() {
    let result = analyze_one("SELECT base.items_array[idx] AS item FROM actual_table AS base");
    let m = find_mapping(&result.columns.mappings, "item");
    assert_eq!(
        concrete_sources(m),
        vec![
            ("actual_table".into(), "idx".into()),
            ("actual_table".into(), "items_array".into()),
        ]
    );
}

#[test]
fn nested_qualified_compound_field_access_keeps_top_level_column() {
    let result = analyze_one("SELECT base.payload.items[1] AS item FROM actual_table AS base");
    let m = find_mapping(&result.columns.mappings, "item");
    assert_eq!(
        concrete_sources(m),
        vec![("actual_table".into(), "payload".into())]
    );
}

#[test]
fn cte_compound_field_access_uses_cte_binding_column() {
    let result = analyze_one(
        "WITH base AS (SELECT items_array FROM actual_table) SELECT base.items_array[1] AS item FROM base",
    );
    let m = find_mapping(&result.columns.mappings, "item");
    assert_eq!(
        concrete_sources(m),
        vec![("actual_table".into(), "items_array".into())]
    );
}

#[test]
fn unqualified_compound_field_access_uses_top_level_column() {
    let result = analyze_one("SELECT payload.items[1] AS item FROM t");
    let m = find_mapping(&result.columns.mappings, "item");
    assert_eq!(concrete_sources(m), vec![("t".into(), "payload".into())]);
}

#[test]
fn compound_identifier_without_visible_binding_keeps_qualified_relation_fallback() {
    let result = analyze_one("SELECT orders.id AS id");
    let m = find_mapping(&result.columns.mappings, "id");
    match m.sources.as_slice() {
        [ColumnOrigin::Concrete { table, column }] => {
            assert_eq!(table.catalog, None);
            assert_eq!(table.schema, None);
            assert_eq!(table.table, "orders");
            assert_eq!(column, "id");
        }
        other => panic!("expected structured orders.id source, got {other:?}"),
    }
}

#[test]
fn compound_identifier_without_visible_binding_preserves_relation_parts() {
    for (sql, catalog, schema, table) in [
        ("SELECT raw.orders.id AS id", None, Some("raw"), "orders"),
        (
            "SELECT warehouse.raw.orders.id AS id",
            Some("warehouse"),
            Some("raw"),
            "orders",
        ),
    ] {
        let result = analyze_one(sql);
        let m = find_mapping(&result.columns.mappings, "id");
        match m.sources.as_slice() {
            [
                ColumnOrigin::Concrete {
                    table: source_table,
                    column,
                },
            ] => {
                assert_eq!(source_table.catalog.as_deref(), catalog);
                assert_eq!(source_table.schema.as_deref(), schema);
                assert_eq!(source_table.table, table);
                assert_eq!(column, "id");
            }
            other => panic!("expected structured relation source, got {other:?}"),
        }
    }
}

#[test]
fn bigquery_offset_compound_field_access_uses_binding_column() {
    let result = analyze_with_dialect(
        "SELECT base.items_array[OFFSET(0)] AS item FROM actual_table AS base",
        Dialect::BigQuery,
    );
    let m = find_mapping(&result.columns.mappings, "item");
    assert_eq!(
        concrete_sources(m),
        vec![("actual_table".into(), "items_array".into())]
    );
}

#[test]
fn qualified_struct_field_access_uses_binding_column() {
    let result = analyze_with_dialect(
        "SELECT agg.event.qualified_field AS field FROM upstream_model AS agg",
        Dialect::BigQuery,
    );
    let m = find_mapping(&result.columns.mappings, "field");
    assert_eq!(
        concrete_sources(m),
        vec![("upstream_model".into(), "event".into())]
    );
}

#[test]
fn bigquery_date_trunc_week_modifier_is_syntax_only() {
    let result = analyze_with_dialect(
        "SELECT DATE_TRUNC(event_date, WEEK(MONDAY)) AS monday_start, DATE_TRUNC(event_date, WEEK(SUNDAY)) AS sunday_start FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "monday_start")),
        vec![("events".into(), "event_date".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "sunday_start")),
        vec![("events".into(), "event_date".into())]
    );
}

#[test]
fn unqualified_struct_field_access_uses_top_level_column() {
    let result = analyze_with_dialect(
        "SELECT event.bare_field AS field FROM upstream_model",
        Dialect::BigQuery,
    );
    let m = find_mapping(&result.columns.mappings, "field");
    assert_eq!(
        concrete_sources(m),
        vec![("upstream_model".into(), "event".into())]
    );
}

#[test]
fn bigquery_date_diff_isoweek_keeps_only_date_values() {
    let result = analyze_with_dialect(
        "SELECT DATE_DIFF(event_date, other_date, ISOWEEK) AS days FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "days")),
        vec![
            ("events".into(), "event_date".into()),
            ("events".into(), "other_date".into()),
        ]
    );
}

#[test]
fn cte_struct_field_access_uses_cte_binding_column() {
    let result = analyze_one(
        "WITH upstream AS (SELECT event FROM source) SELECT upstream.event.field AS value FROM upstream",
    );
    let m = find_mapping(&result.columns.mappings, "value");
    assert_eq!(concrete_sources(m), vec![("source".into(), "event".into())]);
}

#[test]
fn derived_struct_field_access_uses_derived_binding_column() {
    let result = analyze_one(
        "SELECT derived.event.field AS value FROM (SELECT event FROM source) AS derived",
    );
    let m = find_mapping(&result.columns.mappings, "value");
    assert_eq!(concrete_sources(m), vec![("source".into(), "event".into())]);
}

#[test]
fn physical_relation_prefix_struct_field_access_uses_table_parts() {
    let result =
        analyze_one("SELECT catalog.schema.source.event.field AS value FROM catalog.schema.source");
    let m = find_mapping(&result.columns.mappings, "value");
    assert_eq!(concrete_sources(m), vec![("source".into(), "event".into())]);
    assert_eq!(result.tables.inputs[0].catalog.as_deref(), Some("catalog"));
    assert_eq!(result.tables.inputs[0].schema.as_deref(), Some("schema"));
    match m.sources.as_slice() {
        [ColumnOrigin::Concrete { table, column }] => {
            assert_eq!(table.catalog.as_deref(), Some("catalog"));
            assert_eq!(table.schema.as_deref(), Some("schema"));
            assert_eq!(table.table, "source");
            assert_eq!(column, "event");
        }
        other => panic!("expected concrete physical relation source, got {other:?}"),
    }
}

#[test]
fn quoted_single_component_relation_name_keeps_embedded_dot() {
    let result = analyze_with_dialect(
        "SELECT \"orders.v2\".payload.field AS value FROM \"orders.v2\"",
        Dialect::PostgreSql,
    );
    let m = find_mapping(&result.columns.mappings, "value");
    match m.sources.as_slice() {
        [ColumnOrigin::Concrete { table, column }] => {
            assert_eq!(table.catalog, None);
            assert_eq!(table.schema, None);
            assert_eq!(table.table, "orders.v2");
            assert_eq!(column, "payload");
        }
        other => panic!("expected quoted relation source, got {other:?}"),
    }
}

#[test]
fn qualified_struct_field_access_keeps_normal_alias_column_resolution() {
    let result = analyze_one("SELECT source.user_id AS value FROM source");
    let m = find_mapping(&result.columns.mappings, "value");
    assert_eq!(
        concrete_sources(m),
        vec![("source".into(), "user_id".into())]
    );
}

#[test]
fn generic_date_trunc_keeps_date_part_identifiers() {
    let result =
        analyze_one("SELECT DATE_TRUNC(event_date, WEEK(MONDAY)) AS week_start FROM events");
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "week_start")),
        vec![
            ("events".into(), "MONDAY".into()),
            ("events".into(), "event_date".into()),
        ]
    );
}

#[test]
fn unqualified_struct_field_access_keeps_binding_ambiguity() {
    let result = analyze_one(
        "SELECT event.field AS value FROM first_source JOIN second_source ON first_source.id = second_source.id",
    );
    let m = find_mapping(&result.columns.mappings, "value");
    assert!(matches!(
        m.sources.as_slice(),
        [ColumnOrigin::Ambiguous { column, candidates }] if column == "event" && candidates.len() == 2
    ));
}

struct UnqualifiedStructCatalog;

impl CatalogProvider for UnqualifiedStructCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        match table.table.as_str() {
            "first_source" => Some(vec!["event".into()]),
            "second_source" => Some(vec!["other".into()]),
            _ => None,
        }
    }

    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        (column == "event")
            .then(|| {
                candidates
                    .iter()
                    .find(|table| table.table == "first_source")
                    .cloned()
            })
            .flatten()
    }
}

#[test]
fn unqualified_struct_field_access_uses_catalog_owner_for_ambiguous_root() {
    let result = analyze(
        "SELECT event.field AS value FROM first_source JOIN second_source ON first_source.id = second_source.id",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            catalog: Some(Box::new(UnqualifiedStructCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("SQL should parse")
    .remove(0);
    let m = find_mapping(&result.columns.mappings, "value");
    match m.sources.as_slice() {
        [ColumnOrigin::Concrete { table, column }] => {
            assert_eq!(table.table, "first_source");
            assert_eq!(column, "event");
        }
        other => panic!("expected catalog-resolved source, got {other:?}"),
    }
}

struct RowValueCatalog;

impl CatalogProvider for RowValueCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        (table.table == "source_table").then(|| vec!["source".into()])
    }

    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        (column == "source")
            .then(|| candidates.first().cloned())
            .flatten()
    }
}

#[test]
fn bigquery_row_value_alias_prefers_catalog_column_with_same_name() {
    let result = analyze(
        "SELECT ARRAY_AGG(source) AS event FROM source_table AS source",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            catalog: Some(Box::new(RowValueCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("SQL should parse")
    .remove(0);
    let m = find_mapping(&result.columns.mappings, "event");
    assert_eq!(
        concrete_sources(m),
        vec![("source_table".into(), "source".into())]
    );
}

#[test]
fn bigquery_row_value_alias_prefers_cte_output_column_with_same_name() {
    let result = analyze_with_dialect(
        "WITH source AS (SELECT source_table AS source FROM base) SELECT ARRAY_AGG(source) AS event FROM source",
        Dialect::BigQuery,
    );
    let m = find_mapping(&result.columns.mappings, "event");
    assert_eq!(
        concrete_sources(m),
        vec![("base".into(), "source_table".into())]
    );
}

#[test]
fn bigquery_date_trunc_isoyear_and_timezone_data_are_classified() {
    let result = analyze_with_dialect(
        "SELECT DATE_TRUNC(event_date, ISOYEAR) AS year_start, TIMESTAMP_TRUNC(event_ts, DAY, tz_name) AS ts_day FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "year_start")),
        vec![("events".into(), "event_date".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "ts_day")),
        vec![
            ("events".into(), "event_ts".into()),
            ("events".into(), "tz_name".into()),
        ]
    );
}

#[test]
fn bigquery_row_value_alias_prefers_derived_output_column_with_same_name() {
    let result = analyze_with_dialect(
        "SELECT ARRAY_AGG(source) AS event FROM (SELECT source_table AS source FROM base) AS source",
        Dialect::BigQuery,
    );
    let m = find_mapping(&result.columns.mappings, "event");
    assert_eq!(
        concrete_sources(m),
        vec![("base".into(), "source_table".into())]
    );
}

#[test]
fn bigquery_date_trunc_three_argument_form_is_not_a_timezone_signature() {
    let result = analyze_with_dialect(
        "SELECT DATE_TRUNC(event_date, DAY, tz_name) AS day_start FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "day_start")),
        vec![
            ("events".into(), "DAY".into()),
            ("events".into(), "event_date".into()),
            ("events".into(), "tz_name".into()),
        ]
    );
}

#[test]
fn bigquery_row_value_relation_alias_is_not_a_column() {
    let result = analyze_with_dialect(
        "SELECT ARRAY_AGG(source) AS event FROM source_table AS source",
        Dialect::BigQuery,
    );
    let m = find_mapping(&result.columns.mappings, "event");
    assert!(matches!(
        m.sources.as_slice(),
        [ColumnOrigin::Ambiguous { column, candidates }]
            if column == "source" && candidates.is_empty()
    ));
}

#[test]
fn postgresql_row_value_relation_alias_is_not_a_column() {
    let result = analyze_with_dialect(
        "SELECT ARRAY_AGG(source) AS event FROM source_table AS source",
        Dialect::PostgreSql,
    );
    let m = find_mapping(&result.columns.mappings, "event");
    assert!(matches!(
        m.sources.as_slice(),
        [ColumnOrigin::Ambiguous { column, candidates }]
            if column == "source" && candidates.is_empty()
    ));
}

#[test]
fn generic_row_value_relation_alias_preserves_existing_behavior() {
    let result = analyze_one("SELECT ARRAY_AGG(source) AS event FROM source_table AS source");
    let m = find_mapping(&result.columns.mappings, "event");
    assert_eq!(
        concrete_sources(m),
        vec![("source_table".into(), "source".into())]
    );
}

#[test]
fn bigquery_date_part_position_can_still_be_a_data_expression() {
    let result = analyze_with_dialect(
        "SELECT DATE_TRUNC(WEEK, WEEK(MONDAY)) AS week_start FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "week_start")),
        vec![("events".into(), "WEEK".into())]
    );
}

#[test]
fn bigquery_last_day_has_optional_static_part() {
    let result = analyze_with_dialect(
        "SELECT LAST_DAY(event_date, MONTH) AS month_end, LAST_DAY(event_date) AS day_end FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "month_end")),
        vec![("events".into(), "event_date".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "day_end")),
        vec![("events".into(), "event_date".into())]
    );
}

#[test]
fn unknown_udf_keeps_all_arguments() {
    let result = analyze_with_dialect(
        "SELECT my_udf(event_date, ISOYEAR) AS udf_value FROM events",
        Dialect::BigQuery,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "udf_value")),
        vec![
            ("events".into(), "ISOYEAR".into()),
            ("events".into(), "event_date".into()),
        ]
    );
}

#[test]
fn snowflake_date_part_signatures_skip_only_static_parts() {
    let result = analyze_with_dialect(
        "SELECT DATEADD(DAY, amount, event_date) AS added, DATE_PART(YEAR, event_date) AS year_value, TRUNC(event_date, dynamic_part) AS truncated FROM events",
        Dialect::Snowflake,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_date".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "year_value")),
        vec![("events".into(), "event_date".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "truncated")),
        vec![
            ("events".into(), "dynamic_part".into()),
            ("events".into(), "event_date".into()),
        ]
    );
}

#[test]
fn mysql_and_databricks_date_part_signatures_are_dialect_scoped() {
    let mysql = analyze_with_dialect(
        "SELECT TIMESTAMPDIFF(DAY, start_date, end_date) AS elapsed FROM events",
        Dialect::MySql,
    );
    assert_eq!(
        concrete_sources(find_mapping(&mysql.columns.mappings, "elapsed")),
        vec![
            ("events".into(), "end_date".into()),
            ("events".into(), "start_date".into()),
        ]
    );

    let databricks = analyze_with_dialect(
        "SELECT DATEDIFF(DAY, start_date, end_date) AS elapsed FROM events",
        Dialect::Databricks,
    );
    assert_eq!(
        concrete_sources(find_mapping(&databricks.columns.mappings, "elapsed")),
        vec![
            ("events".into(), "end_date".into()),
            ("events".into(), "start_date".into()),
        ]
    );
}

#[test]
fn mysql_date_part_aliases_are_limited_to_legal_timestamp_units() {
    let result = analyze_with_dialect(
        "SELECT TIMESTAMPDIFF(SQL_TSI_DAY, start_date, end_date) AS aliased, TIMESTAMPDIFF(DAY_SECOND, start_date, end_date) AS composite FROM events",
        Dialect::MySql,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "aliased")),
        vec![
            ("events".into(), "end_date".into()),
            ("events".into(), "start_date".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "composite")),
        vec![
            ("events".into(), "DAY_SECOND".into()),
            ("events".into(), "end_date".into()),
            ("events".into(), "start_date".into()),
        ]
    );
}

#[test]
fn databricks_add_and_diff_have_distinct_date_part_grammars() {
    let result = analyze_with_dialect(
        "SELECT DATEADD(DAYOFYEAR, amount, event_ts) AS added, DATEDIFF(DAYOFYEAR, start_ts, end_ts) AS elapsed FROM events",
        Dialect::Databricks,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "elapsed")),
        vec![
            ("events".into(), "DAYOFYEAR".into()),
            ("events".into(), "end_ts".into()),
            ("events".into(), "start_ts".into()),
        ]
    );
}

#[test]
fn redshift_temporal_profiles_keep_date_part_functions_and_fallbacks() {
    let result = analyze_with_dialect(
        "SELECT DATEADD(day, amount, event_ts) AS added, DATEDIFF(week, start_ts, end_ts) AS elapsed, DATE_PART(dow, event_ts) AS weekday_value, PGDATE_PART(dow, event_ts) AS pg_weekday_value, DATE_PART(dayofyear, event_ts) AS dayofyear_unknown, DATEADD(m, amount, event_ts) AS minute_alias, DATEADD(w, amount, event_ts) AS week_alias, DATEADD(mon, amount, event_ts) AS month_alias, DATEADD(mm, amount, event_ts) AS sqlserver_month, DATEADD(wk, amount, event_ts) AS sqlserver_week, DATEADD(dynamic_part, amount, event_ts) AS dynamic_added, DATE_TRUNC('week', event_ts) AS truncated FROM events",
        Dialect::Redshift,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "elapsed")),
        vec![
            ("events".into(), "end_ts".into()),
            ("events".into(), "start_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "weekday_value")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "pg_weekday_value")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "dayofyear_unknown")),
        vec![
            ("events".into(), "dayofyear".into()),
            ("events".into(), "event_ts".into()),
        ]
    );
    for output in ["minute_alias", "week_alias", "month_alias"] {
        assert_eq!(
            concrete_sources(find_mapping(&result.columns.mappings, output)),
            vec![
                ("events".into(), "amount".into()),
                ("events".into(), "event_ts".into()),
            ]
        );
    }
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "sqlserver_month")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_ts".into()),
            ("events".into(), "mm".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "sqlserver_week")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_ts".into()),
            ("events".into(), "wk".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "dynamic_added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "dynamic_part".into()),
            ("events".into(), "event_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "truncated")),
        vec![("events".into(), "event_ts".into())]
    );
}

#[test]
fn mssql_temporal_profiles_keep_family_specific_date_parts() {
    let result = analyze_with_dialect(
        "SELECT DATEADD(day, amount, event_ts) AS added, DATEDIFF(weekday, start_ts, end_ts) AS elapsed, DATEDIFF_BIG(ns, start_ts, end_ts) AS big_elapsed, DATEPART(tzoffset, event_ts) AS offset_value, DATENAME(iso_week, event_ts) AS iso_name, DATETRUNC(week, event_ts) AS truncated, DATETRUNC(weekday, event_ts) AS weekday_truncated, DATE_BUCKET(day, 7, event_ts) AS bucketed, DATE_BUCKET(day, 7, event_ts, origin_ts) AS bucketed_with_origin, DATE_BUCKET(mcs, 7, event_ts) AS bucket_microsecond, DATEADD(dynamic_part, amount, event_ts) AS dynamic_added, DATEADD(unknown_part, amount, event_ts) AS unknown_added FROM events",
        Dialect::MsSql,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "elapsed")),
        vec![
            ("events".into(), "end_ts".into()),
            ("events".into(), "start_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "big_elapsed")),
        vec![
            ("events".into(), "end_ts".into()),
            ("events".into(), "start_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "offset_value")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "iso_name")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "truncated")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "weekday_truncated")),
        vec![
            ("events".into(), "event_ts".into()),
            ("events".into(), "weekday".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "bucketed")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(
            &result.columns.mappings,
            "bucketed_with_origin"
        )),
        vec![
            ("events".into(), "event_ts".into()),
            ("events".into(), "origin_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "bucket_microsecond")),
        vec![
            ("events".into(), "event_ts".into()),
            ("events".into(), "mcs".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "dynamic_added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "dynamic_part".into()),
            ("events".into(), "event_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "unknown_added")),
        vec![
            ("events".into(), "amount".into()),
            ("events".into(), "event_ts".into()),
            ("events".into(), "unknown_part".into()),
        ]
    );
}

#[test]
fn spark_temporal_functions_use_generic_value_fallback() {
    let result = analyze_with_dialect(
        "SELECT DATE_TRUNC('WEEK', event_ts) AS truncated, DATEDIFF(end_ts, start_ts) AS elapsed, DATEDIFF(DAY, start_ts, end_ts) AS nonstandard FROM events",
        Dialect::Spark,
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "truncated")),
        vec![("events".into(), "event_ts".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "elapsed")),
        vec![
            ("events".into(), "end_ts".into()),
            ("events".into(), "start_ts".into()),
        ]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "nonstandard")),
        vec![
            ("events".into(), "DAY".into()),
            ("events".into(), "end_ts".into()),
            ("events".into(), "start_ts".into()),
        ]
    );
}
