//! Which columns inside an aggregate call are lineage sources.
//!
//! The aggregated value is a source. For an ordered-set aggregate
//! (`PERCENTILE_CONT`, `MODE`, `LISTAGG ... WITHIN GROUP`) the `ORDER BY`
//! column of `WITHIN GROUP` is the aggregated value itself, so it is a
//! source. Columns that only select rows (`FILTER (WHERE ...)`,
//! `AGG(x WHERE ...)`) or order the inputs of an ordinary aggregate
//! (`ARRAY_AGG(x ORDER BY y)`) do not contribute values to the result.

use dlin_sqllineage::{
    AnalyzeOptions, AnalyzeResult, ColumnMapping, ColumnOrigin, Dialect, TransformKind, analyze,
};

fn run(sql: &str, dialect: Dialect) -> AnalyzeResult {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            ..AnalyzeOptions::default()
        },
    )
    .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
    .remove(0)
}

fn concrete(mapping: &ColumnMapping) -> Vec<String> {
    let mut sources: Vec<_> = mapping
        .sources
        .iter()
        .map(|source| match source {
            ColumnOrigin::Concrete { table, column } => format!("{}.{column}", table.table),
            other => panic!("expected Concrete, got {other:?}"),
        })
        .collect();
    sources.sort();
    sources
}

/// Analyze `SELECT <expr> AS r FROM t` and check sources and transform.
fn check(dialects: &[Dialect], expr: &str, sources: &[&str], transform: &TransformKind) {
    let sql = format!("SELECT {expr} AS r FROM t");
    for &dialect in dialects {
        let result = run(&sql, dialect);
        let mapping = &result.columns.mappings[0];
        assert_eq!(concrete(mapping), sources, "{dialect:?}: {sql}");
        assert_eq!(&mapping.transform, transform, "{dialect:?}: {sql}");
    }
}

const ORDERED_SET: &[Dialect] = &[
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::Snowflake,
    Dialect::Redshift,
    Dialect::DuckDb,
    Dialect::MsSql,
];

const FILTERABLE: &[Dialect] = &[
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::DuckDb,
    Dialect::SQLite,
];

#[test]
fn within_group_order_by_column_is_the_aggregated_value() {
    check(
        ORDERED_SET,
        "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY y)",
        &["t.y"],
        &TransformKind::Aggregation,
    );
    check(
        ORDERED_SET,
        "PERCENTILE_DISC(0.5) WITHIN GROUP (ORDER BY y DESC)",
        &["t.y"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn mode_within_group_reads_its_order_by_column() {
    check(
        &[Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb],
        "MODE() WITHIN GROUP (ORDER BY y)",
        &["t.y"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn listagg_within_group_reads_value_and_order_columns() {
    check(
        ORDERED_SET,
        "LISTAGG(x, ',') WITHIN GROUP (ORDER BY y)",
        &["t.x", "t.y"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn within_group_order_expression_contributes_every_column() {
    check(
        ORDERED_SET,
        "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY y + z)",
        &["t.y", "t.z"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn within_group_ignores_the_row_filter() {
    check(
        &[Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb],
        "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY y) FILTER (WHERE c > 0)",
        &["t.y"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn filter_clause_columns_are_not_sources() {
    check(
        FILTERABLE,
        "COUNT(x) FILTER (WHERE c > 0)",
        &["t.x"],
        &TransformKind::Aggregation,
    );
    check(
        FILTERABLE,
        "SUM(x) FILTER (WHERE c > 0 AND d IS NOT NULL)",
        &["t.x"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn aggregate_internal_order_by_columns_are_not_sources() {
    check(
        &[
            Dialect::Generic,
            Dialect::PostgreSql,
            Dialect::BigQuery,
            Dialect::DuckDb,
            Dialect::Snowflake,
        ],
        "ARRAY_AGG(x ORDER BY y)",
        &["t.x"],
        &TransformKind::Aggregation,
    );
    check(
        &[
            Dialect::Generic,
            Dialect::PostgreSql,
            Dialect::BigQuery,
            Dialect::DuckDb,
        ],
        "STRING_AGG(x, ',' ORDER BY y)",
        &["t.x"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn where_inside_the_argument_list_is_not_a_source() {
    check(
        &[
            Dialect::Generic,
            Dialect::PostgreSql,
            Dialect::DuckDb,
            Dialect::BigQuery,
        ],
        "SUM(x WHERE c > 0)",
        &["t.x"],
        &TransformKind::Aggregation,
    );
}

#[test]
fn clauses_combine_without_leaking_columns() {
    check(
        &[Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb],
        "ARRAY_AGG(x ORDER BY y) FILTER (WHERE c > 0)",
        &["t.x"],
        &TransformKind::Aggregation,
    );
}

/// The ordered-set aggregate keeps its classification across a CTE hop.
#[test]
fn within_group_aggregation_survives_a_hop() {
    let result = run(
        "WITH s AS (SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY y) AS p FROM t) \
         SELECT p FROM s",
        Dialect::PostgreSql,
    );
    let mapping = &result.columns.mappings[0];
    assert_eq!(concrete(mapping), ["t.y"]);
    assert_eq!(mapping.transform, TransformKind::Aggregation);
}
