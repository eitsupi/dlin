//! Which columns inside an aggregate call are lineage sources.
//!
//! The aggregated value is a source. For an ordered-set aggregate
//! (`PERCENTILE_CONT`, `MODE`, `LISTAGG ... WITHIN GROUP`) the `ORDER BY`
//! column of `WITHIN GROUP` is the aggregated value itself, so it is a
//! source. Columns that only select rows (`FILTER (WHERE ...)`,
//! `AGG(x WHERE ...)`) or order the inputs of an ordinary aggregate
//! (`ARRAY_AGG(x ORDER BY y)`) do not contribute values to the result.

use dlin_sqllineage::{Analyzer, Dialect, Lineage, OutputColumn, Transform};

fn run(sql: &str, dialect: Dialect) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(dialect)
        .analyze(sql)
        .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
        .into_single()
        .unwrap()
}

fn concrete(mapping: &OutputColumn) -> Vec<String> {
    let mut sources: Vec<_> = match &mapping.lineage {
        Lineage::Complete(sources) => sources
            .iter()
            .map(|source| format!("{}.{}", source.relation.name.value, source.column.value))
            .collect(),
        other => panic!("expected complete lineage, got {other:?}"),
    };
    sources.sort();
    sources
}

/// Analyze `SELECT <expr> AS r FROM t` and check sources and transform.
fn check(dialects: &[Dialect], expr: &str, sources: &[&str], transform: &Transform) {
    let sql = format!("SELECT {expr} AS r FROM t");
    for &dialect in dialects {
        let result = run(&sql, dialect);
        let mapping = &result.output.columns[0];
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
        &Transform::Aggregation,
    );
    check(
        ORDERED_SET,
        "PERCENTILE_DISC(0.5) WITHIN GROUP (ORDER BY y DESC)",
        &["t.y"],
        &Transform::Aggregation,
    );
}

#[test]
fn mode_within_group_reads_its_order_by_column() {
    check(
        &[Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb],
        "MODE() WITHIN GROUP (ORDER BY y)",
        &["t.y"],
        &Transform::Aggregation,
    );
}

#[test]
fn listagg_within_group_reads_value_and_order_columns() {
    check(
        ORDERED_SET,
        "LISTAGG(x, ',') WITHIN GROUP (ORDER BY y)",
        &["t.x", "t.y"],
        &Transform::Aggregation,
    );
}

#[test]
fn within_group_order_expression_contributes_every_column() {
    check(
        ORDERED_SET,
        "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY y + z)",
        &["t.y", "t.z"],
        &Transform::Aggregation,
    );
}

#[test]
fn within_group_ignores_the_row_filter() {
    check(
        &[Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb],
        "PERCENTILE_CONT(0.5) WITHIN GROUP (ORDER BY y) FILTER (WHERE c > 0)",
        &["t.y"],
        &Transform::Aggregation,
    );
}

#[test]
fn filter_clause_columns_are_not_sources() {
    check(
        FILTERABLE,
        "COUNT(x) FILTER (WHERE c > 0)",
        &["t.x"],
        &Transform::Aggregation,
    );
    check(
        FILTERABLE,
        "SUM(x) FILTER (WHERE c > 0 AND d IS NOT NULL)",
        &["t.x"],
        &Transform::Aggregation,
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
        &Transform::Aggregation,
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
        &Transform::Aggregation,
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
        &Transform::Aggregation,
    );
}

#[test]
fn clauses_combine_without_leaking_columns() {
    check(
        &[Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb],
        "ARRAY_AGG(x ORDER BY y) FILTER (WHERE c > 0)",
        &["t.x"],
        &Transform::Aggregation,
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
    let mapping = &result.output.columns[0];
    assert_eq!(concrete(mapping), ["t.y"]);
    assert_eq!(mapping.transform, Transform::Aggregation);
}
