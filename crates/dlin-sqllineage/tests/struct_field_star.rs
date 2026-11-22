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

#[test]
fn field_star_over_struct_built_in_place_expands_its_fields() {
    let expected = ["col_a#0 C[]", "flag#1 C[]", "Closed"];
    for dialect in [Dialect::BigQuery, Dialect::Databricks, Dialect::Spark] {
        assert_eq!(
            render(
                r#"
                WITH packed AS (SELECT STRUCT(1 AS col_a) AS item),
                expanded AS (SELECT item.*, 2 AS flag FROM packed)
                SELECT * FROM expanded
                "#,
                dialect,
            ),
            expected,
            "{dialect}"
        );
    }
    assert_eq!(
        render(
            r#"
            WITH packed AS (SELECT {'col_a': 1} AS item),
            expanded AS (SELECT item.*, 2 AS flag FROM packed)
            SELECT * FROM expanded
            "#,
            Dialect::DuckDb,
        ),
        expected
    );
}

#[test]
fn each_expanded_field_keeps_its_own_value() {
    let expected = [
        "col_a#0 C[src.a]",
        "col_b#1 C[src.b]",
        "flag#2 C[]",
        "Closed",
    ];
    for sql in [
        r#"
        WITH packed AS (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src),
        e AS (SELECT item.*, 2 AS flag FROM packed)
        SELECT col_a, col_b, flag FROM e
        "#,
        r#"
        WITH packed AS (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src)
        SELECT p.item.*, 2 AS flag FROM packed AS p
        "#,
        r#"
        SELECT item.*, 2 AS flag
        FROM (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src) AS p
        "#,
        r#"
        WITH packed AS (SELECT STRUCT<col_a INT64, col_b INT64>(src.a, src.b) AS item FROM src)
        SELECT item.*, 2 AS flag FROM packed
        "#,
        // A column forwarded by another CTE keeps its fields, whether a
        // star or the column itself forwards it.
        r#"
        WITH packed AS (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src),
        fwd AS (SELECT * FROM packed)
        SELECT item.*, 2 AS flag FROM fwd
        "#,
        r#"
        WITH packed AS (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src),
        fwd AS (SELECT item FROM packed)
        SELECT item.*, 2 AS flag FROM fwd
        "#,
        r#"
        WITH packed AS (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src),
        fwd AS (SELECT p.item AS renamed FROM packed AS p)
        SELECT f.renamed.*, 2 AS flag FROM fwd AS f
        "#,
        r#"
        WITH packed AS (SELECT STRUCT(src.a AS col_a, src.b AS col_b) AS item FROM src)
        SELECT item.*, 2 AS flag FROM (SELECT (item) FROM packed)
        "#,
    ] {
        assert_eq!(render(sql, Dialect::BigQuery), expected, "{sql}");
    }
}

#[test]
fn expanded_field_is_not_a_column_of_the_struct_owner() {
    // packed also has a column col_a; the field is the value inside item.
    assert_eq!(
        render(
            r#"
            WITH packed AS (SELECT STRUCT(src.a AS col_a) AS item, src.z AS col_a FROM src)
            SELECT item.* FROM packed
            "#,
            Dialect::BigQuery,
        ),
        ["col_a#0 C[src.a]", "Closed"]
    );
}

#[test]
fn field_of_the_same_name_as_a_listed_column_is_ambiguous() {
    for sql in [
        r#"
        WITH packed AS (SELECT STRUCT(1 AS flag) AS item),
        e AS (SELECT item.*, 2 AS flag FROM packed)
        SELECT flag FROM e
        "#,
        r#"
        WITH packed AS (SELECT STRUCT(1 AS FLAG) AS item),
        e AS (SELECT item.*, 2 AS flag FROM packed)
        SELECT flag FROM e
        "#,
    ] {
        assert_eq!(
            render(sql, Dialect::BigQuery),
            ["flag#0 P[ | Ambiguous(flag: cte e)]", "Closed"],
            "{sql}"
        );
    }
    // Excluding the field leaves one flag.
    assert_eq!(
        render(
            r#"
            WITH packed AS (SELECT STRUCT(1 AS flag, 3 AS b) AS item),
            e AS (SELECT item.* EXCEPT (flag), 2 AS flag FROM packed)
            SELECT * FROM e
            "#,
            Dialect::BigQuery,
        ),
        ["b#0 C[]", "flag#1 C[]", "Closed"]
    );
}

#[test]
fn struct_whose_field_names_are_not_all_known_stays_open() {
    // BigQuery infers the name of the second field from src.a.
    for sql in [
        r#"
        WITH packed AS (SELECT STRUCT(1, src.a) AS item FROM src),
        e AS (SELECT item.*, 2 AS flag FROM packed)
        SELECT flag FROM e
        "#,
        r#"
        WITH packed AS (SELECT STRUCT(1 AS a, 2 AS a) AS item),
        e AS (SELECT item.*, 2 AS flag FROM packed)
        SELECT flag FROM e
        "#,
    ] {
        assert_eq!(
            render(sql, Dialect::BigQuery),
            ["flag#0 P[ | Ambiguous(flag: cte e)]", "Closed"],
            "{sql}"
        );
    }
}

#[test]
fn struct_value_of_a_set_operation_or_of_several_relations_stays_open() {
    for sql in [
        r#"
        WITH packed AS (
            SELECT STRUCT(src.a AS col_a) AS item FROM src
            UNION ALL SELECT STRUCT(src.b AS col_a) FROM src
        )
        SELECT item.* FROM packed
        "#,
        r#"
        WITH p1 AS (SELECT STRUCT(1 AS a) AS item), p2 AS (SELECT STRUCT(2 AS b) AS item)
        SELECT item.* FROM p1, p2
        "#,
        // A conversion may rename the fields.
        r#"
        WITH packed AS (SELECT STRUCT(1 AS a) AS item),
        fwd AS (SELECT CAST(item AS STRUCT<b INT64>) AS item FROM packed)
        SELECT item.* FROM fwd
        "#,
        // Only a direct field of the column is expanded.
        r#"
        WITH packed AS (SELECT STRUCT(STRUCT(src.a AS x) AS inner_s) AS item FROM src)
        SELECT p.item.inner_s.* FROM packed AS p
        "#,
    ] {
        assert_eq!(render(sql, Dialect::BigQuery), ["Open[? after -]"], "{sql}");
    }
}

#[test]
fn columns_that_forward_each_other_have_no_known_fields() {
    assert_eq!(
        render(
            r#"
            WITH RECURSIVE
              a AS (SELECT item FROM b),
              b AS (SELECT item FROM a)
            SELECT item.* FROM a
            "#,
            Dialect::BigQuery,
        ),
        ["Open[? after -]"]
    );
}

#[test]
fn recursive_struct_column_has_no_known_fields() {
    // The anchor alone does not decide what the recursive term adds.
    assert_eq!(
        render(
            r#"
            WITH RECURSIVE r AS (
              SELECT STRUCT(src.a AS x) AS item, 1 AS n FROM src
              UNION ALL
              SELECT item, n + 1 FROM r WHERE n < 3
            )
            SELECT item.* FROM r
            "#,
            Dialect::BigQuery,
        ),
        ["Open[? after -]"]
    );
}

#[test]
fn replace_takes_the_place_of_an_expanded_field() {
    // x keeps its position but no longer reads src.a.
    assert_eq!(
        render(
            r#"
            WITH packed AS (SELECT STRUCT(src.a AS x, src.c AS y) AS item, src.b AS other FROM src)
            SELECT item.* REPLACE (other AS x) FROM packed
            "#,
            Dialect::BigQuery,
        ),
        ["x#0 C[src.b]", "y#1 C[src.c]", "Closed"]
    );
}
