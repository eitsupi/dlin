//! Stars over the fields of a STRUCT value (`item.*`, `t.item.*`).
//!
//! In BigQuery, Databricks, Spark and DuckDB a qualifier that names no range
//! variable names a column, and the star expands its fields. Range variables
//! keep precedence over columns of the same name.

mod common;

use common::{analyze_one_with, render_output};
use dlin_sqllineage::{Dialect, Lineage, Shape};

fn render(sql: &str, dialect: Dialect) -> Vec<String> {
    render_output(&analyze_one_with(sql, dialect, None).output)
}

/// The known sources of every unexpanded star, as `relation.column`.
fn unexpanded_sources(sql: &str, dialect: Dialect) -> Vec<String> {
    let analysis = analyze_one_with(sql, dialect, None);
    let Shape::Open { unexpanded } = &analysis.output.shape else {
        panic!("expected an open output, got {:?}", analysis.output.shape)
    };
    unexpanded
        .iter()
        .flat_map(|star| match &star.lineage {
            Lineage::Complete(sources) | Lineage::Partial { known: sources, .. } => sources.iter(),
        })
        .map(|source| format!("{}.{}", source.relation.name.value, source.column.value))
        .collect()
}

#[test]
fn unqualified_column_field_star_reads_the_column() {
    for dialect in [
        Dialect::BigQuery,
        Dialect::Databricks,
        Dialect::Spark,
        Dialect::DuckDb,
    ] {
        let sql = r#"
            WITH p AS (SELECT src.a AS item FROM src)
            SELECT item.* FROM p
        "#;
        let analysis = analyze_one_with(sql, dialect, None);
        assert!(
            !analysis.read.iter().any(|table| table.name.value == "item"),
            "{dialect}: a column is not a relation"
        );
        assert_eq!(
            unexpanded_sources(sql, dialect),
            ["src.a"],
            "{dialect}: the field star reads the column item"
        );
        let Shape::Open { unexpanded } = &analysis.output.shape else {
            unreachable!()
        };
        assert!(unexpanded[0].relation.is_none(), "{dialect}");
    }
}

#[test]
fn unqualified_column_field_star_reads_a_derived_table_column() {
    assert_eq!(
        unexpanded_sources(
            r#"SELECT item.* FROM (SELECT src.a AS item FROM src) AS p"#,
            Dialect::BigQuery,
        ),
        ["src.a"]
    );
}

#[test]
fn range_variable_takes_precedence_over_column_field_star() {
    assert_eq!(
        render(
            r#"
            WITH item AS (SELECT 1 AS a), p AS (SELECT 2 AS item)
            SELECT item.* FROM p, item
            "#,
            Dialect::BigQuery,
        ),
        ["a#0 C[]", "Closed"]
    );
}

#[test]
fn qualifier_without_range_variable_stays_a_relation_elsewhere() {
    // Snowflake has no star over a column: the qualifier is kept as the
    // relation it names, and no field of p.item is claimed.
    let analysis = analyze_one_with(
        r#"WITH p AS (SELECT src.a AS item FROM src) SELECT item.* FROM p"#,
        Dialect::Snowflake,
        None,
    );
    let Shape::Open { unexpanded } = &analysis.output.shape else {
        panic!("expected an open output")
    };
    assert_eq!(
        unexpanded[0]
            .relation
            .as_ref()
            .map(|relation| relation.name.value.as_str()),
        Some("item")
    );
}

#[test]
fn column_next_to_field_star_of_unknown_type_is_not_complete() {
    // Without the type of item, its fields may include another flag.
    for sql in [
        r#"WITH e AS (SELECT p.item.*, 2 AS flag FROM src AS p) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT item.*, 2 AS flag FROM src) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT (src.item).*, 2 AS flag FROM src) SELECT flag FROM e"#,
    ] {
        assert_eq!(
            render(sql, Dialect::BigQuery),
            ["flag#0 P[ | Ambiguous(flag: cte e)]", "Closed"],
            "{sql}"
        );
    }
}

#[test]
fn name_read_through_field_star_of_unknown_type_is_not_complete() {
    assert_eq!(
        render(
            r#"WITH e AS (SELECT p.item.* FROM src AS p) SELECT col_a FROM e"#,
            Dialect::BigQuery,
        ),
        [
            r#"col_a#0 P[ | Unsupported { what: "star expansion" }]"#,
            "Closed"
        ]
    );
}
