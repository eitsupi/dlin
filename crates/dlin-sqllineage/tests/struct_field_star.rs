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
    for dialect in [
        Dialect::BigQuery,
        Dialect::Databricks,
        Dialect::Spark,
        Dialect::DuckDb,
    ] {
        let sql = r#"WITH e AS (SELECT item.*, 2 AS flag FROM src) SELECT flag FROM e"#;
        assert_eq!(
            render(sql, dialect),
            ["flag#0 P[ | Unverified(flag: cte e | )]", "Closed"],
            "{dialect}"
        );
    }
    for sql in [
        r#"WITH e AS (SELECT p.item.*, 2 AS flag FROM src AS p) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT (src.item).*, 2 AS flag FROM src) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT a.*, b.*, 2 AS flag FROM src) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT a.*, b.* FROM src) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT item.*, 2 AS flag FROM src), w AS (SELECT * FROM e) SELECT flag FROM w"#,
        r#"WITH e AS (SELECT item.* FROM src), f AS (SELECT 2 AS flag) SELECT flag FROM e CROSS JOIN f"#,
        r#"WITH e AS (SELECT item.*, 2 AS flag FROM src), f AS (SELECT 3 AS flag) SELECT flag FROM e JOIN f USING (flag)"#,
    ] {
        let analysis = analyze_one_with(sql, Dialect::BigQuery, None);
        let rendered = render_output(&analysis.output);
        assert!(
            rendered[0].contains("Unverified(flag:"),
            "{sql}: {rendered:?}"
        );
    }
}

#[test]
fn known_duplicate_names_remain_ambiguous_with_an_unknown_field_star() {
    for sql in [
        r#"WITH e AS (SELECT item.*, 2 AS flag, 3 AS flag FROM src) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT item.*, 2 AS flag FROM src), f AS (SELECT 3 AS flag) SELECT flag FROM e CROSS JOIN f"#,
    ] {
        let rendered = render(sql, Dialect::BigQuery);
        assert!(
            rendered[0].contains("Ambiguous(flag:"),
            "{sql}: {rendered:?}"
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
            ["flag#0 P[ | Unverified(flag: cte e | )]", "Closed"],
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

#[test]
fn struct_column_named_like_its_relation_binds_as_its_value_does() {
    // item is both the CTE and its column. The value of renamed and the
    // fields of renamed.* follow the same reading of the name.
    let fields = render(
        r#"
        WITH item AS (SELECT STRUCT(src.a AS x) AS item FROM src),
        fwd AS (SELECT item AS renamed FROM item)
        SELECT renamed.* FROM fwd
        "#,
        Dialect::BigQuery,
    );
    let value = render(
        r#"
        WITH item AS (SELECT STRUCT(src.a AS x) AS item FROM src),
        fwd AS (SELECT item AS renamed FROM item)
        SELECT renamed FROM fwd
        "#,
        Dialect::BigQuery,
    );
    assert_eq!(value, ["renamed#0 C[src.a]", "Closed"]);
    assert_eq!(fields, ["item#0 C[src.a] Expression", "Closed"]);
}

#[test]
fn duckdb_struct_pack_expands_like_a_struct_literal() {
    for sql in [
        r#"
        WITH packed AS (SELECT struct_pack(col_a := src.a, col_b := src.b) AS item FROM src),
        e AS (SELECT item.*, 3 AS flag FROM packed)
        SELECT * FROM e
        "#,
        r#"
        WITH packed AS (SELECT {'col_a': src.a, 'col_b': src.b} AS item FROM src),
        e AS (SELECT item.*, 3 AS flag FROM packed)
        SELECT * FROM e
        "#,
    ] {
        assert_eq!(
            render(sql, Dialect::DuckDb),
            [
                "col_a#0 C[src.a]",
                "col_b#1 C[src.b]",
                "flag#2 C[]",
                "Closed"
            ],
            "{sql}"
        );
    }
}

#[test]
fn duckdb_struct_pack_with_a_field_of_the_listed_name_is_ambiguous() {
    assert_eq!(
        render(
            r#"
            WITH packed AS (SELECT struct_pack(FLAG := 1) AS item),
            e AS (SELECT item.*, 2 AS flag FROM packed)
            SELECT flag FROM e
            "#,
            Dialect::DuckDb,
        ),
        ["flag#0 P[ | Ambiguous(flag: cte e)]", "Closed"]
    );
}

#[test]
fn struct_constructor_of_another_dialect_is_not_assumed() {
    // struct_pack is a DuckDB function; elsewhere it may be a user function
    // that returns anything.
    assert_eq!(
        render(
            r#"
            WITH packed AS (SELECT struct_pack(col_a := 1) AS item),
            e AS (SELECT item.*, 2 AS flag FROM packed)
            SELECT flag FROM e
            "#,
            Dialect::BigQuery,
        ),
        ["flag#0 P[ | Unverified(flag: cte e | )]", "Closed"]
    );
}

#[test]
fn repeated_projections_of_one_range_are_ambiguous_with_an_unknown_field_star() {
    for sql in [
        r#"WITH c AS (SELECT 2 AS flag), e AS (SELECT c.flag, c.flag, s.item.* FROM c CROSS JOIN src AS s) SELECT flag FROM e"#,
        r#"WITH c AS (SELECT 2 AS flag), e AS (SELECT c.*, c.*, s.item.* FROM c CROSS JOIN src AS s) SELECT flag FROM e"#,
        r#"WITH e AS (SELECT c.*, c.*, s.item.* FROM (SELECT 2 AS flag) c CROSS JOIN src AS s) SELECT flag FROM e"#,
        r#"WITH c AS (SELECT 2 AS flag, 3 AS item), e AS (SELECT a.*, b.*, s.item.* FROM c a JOIN c b USING (flag, item) CROSS JOIN src AS s) SELECT flag FROM e"#,
    ] {
        let rendered = render(sql, Dialect::BigQuery);
        assert!(
            rendered[0].contains("Ambiguous(flag:"),
            "{sql}: {rendered:?}"
        );
    }
}

#[test]
fn bigquery_aggregate_element_expands_row_fields_through_a_cte() {
    for index in [
        "OFFSET(0)",
        "ORDINAL(1)",
        "SAFE_OFFSET(0)",
        "SAFE_ORDINAL(1)",
        "0",
    ] {
        let sql = format!(
            r#"WITH src AS (SELECT x, y, ts FROM raw),
            picked AS (SELECT ARRAY_AGG(t ORDER BY ts DESC LIMIT 1)[{index}] AS item FROM src t),
            forwarded AS (SELECT item AS value FROM picked)
            SELECT value.* FROM forwarded"#
        );
        assert_eq!(
            render(&sql, Dialect::BigQuery),
            [
                "x#0 C[raw.x] Aggregation",
                "y#1 C[raw.y] Aggregation",
                "ts#2 C[raw.ts] Aggregation",
                "Closed"
            ],
            "{index}"
        );
    }
}

#[test]
fn aggregate_element_keeps_only_each_field_and_the_index_inputs() {
    assert_eq!(
        render(
            r#"WITH p AS (
            SELECT ARRAY_AGG(STRUCT(a AS x, b AS y) ORDER BY ts DESC LIMIT 1)[SAFE_OFFSET(idx)] AS item
            FROM src GROUP BY idx
        )
        SELECT item.* FROM p"#,
            Dialect::BigQuery
        ),
        [
            "x#0 C[src.a, src.idx] Aggregation",
            "y#1 C[src.b, src.idx] Aggregation",
            "Closed"
        ]
    );
}

#[test]
fn equivalent_array_element_access_in_other_dialects() {
    for (dialect, expr, star) in [
        (Dialect::DuckDb, "array_agg(t ORDER BY x)[1]", "item.*"),
        (Dialect::DuckDb, "list(t ORDER BY x)[1]", "item.*"),
        (
            Dialect::Spark,
            "array_agg(struct(x AS x, y AS y))[0]",
            "item.*",
        ),
        (
            Dialect::Spark,
            "collect_list(named_struct('x', x, 'y', y))[0]",
            "item.*",
        ),
        (
            Dialect::Databricks,
            "element_at(array_agg(named_struct('x', x, 'y', y)), 1)",
            "item.*",
        ),
    ] {
        let sql = format!(
            r#"WITH p AS (SELECT {expr} AS item FROM (SELECT x, y FROM src) t) SELECT {star} FROM p"#
        );
        assert_eq!(
            render(&sql, dialect),
            [
                "x#0 C[src.x] Aggregation",
                "y#1 C[src.y] Aggregation",
                "Closed"
            ],
            "{dialect}: {sql}"
        );
    }
}

#[test]
fn row_alias_fields_and_a_same_named_column_use_distinct_bindings() {
    for (dialect, star) in [(Dialect::BigQuery, "item.*"), (Dialect::DuckDb, "item.*")] {
        let sql = format!(
            r#"WITH p AS (SELECT t AS item FROM (SELECT x, y FROM src) t) SELECT {star} FROM p"#
        );
        assert_eq!(
            render(&sql, dialect),
            ["x#0 C[src.x]", "y#1 C[src.y]", "Closed"],
            "{dialect}"
        );
        let sql = format!(
            r#"WITH p AS (SELECT t AS item FROM (SELECT x AS t, y FROM src) t) SELECT {star} FROM p"#
        );
        if dialect == Dialect::BigQuery {
            assert_eq!(
                render(&sql, dialect),
                ["t#0 C[src.x]", "y#1 C[src.y]", "Closed"]
            );
        } else {
            let analysis = analyze_one_with(&sql, dialect, None);
            assert!(
                matches!(analysis.output.shape, Shape::Open { .. }),
                "{dialect}: {sql}"
            );
        }
    }
}

#[test]
fn unknown_and_scalar_elements_do_not_invent_struct_fields() {
    for sql in [
        r#"WITH p AS (SELECT ARRAY_AGG(t)[OFFSET(0)] AS item FROM src t), e AS (SELECT item.*, 2 AS flag FROM p) SELECT flag FROM e"#,
        r#"WITH p AS (SELECT ARRAY_AGG(t)[0] AS item FROM UNNEST([1, 2]) t), e AS (SELECT item.*, 2 AS flag FROM p) SELECT flag FROM e"#,
        r#"WITH v AS (SELECT AS VALUE 1), p AS (SELECT ARRAY_AGG(t)[0] AS item FROM v t), e AS (SELECT item.*, 2 AS flag FROM p) SELECT flag FROM e"#,
        r#"WITH p AS (SELECT ARRAY_AGG(x)[0] AS item FROM src), e AS (SELECT item.*, 2 AS flag FROM p) SELECT flag FROM e"#,
        r#"WITH p AS (SELECT fn(STRUCT(x AS flag))[0] AS item FROM src), e AS (SELECT item.*, 2 AS flag FROM p) SELECT flag FROM e"#,
    ] {
        let output = render(sql, Dialect::BigQuery);
        assert!(output[0].contains("Unverified(flag:"), "{sql}: {output:?}");
    }
}

#[test]
fn aggregate_row_fields_survive_outer_join_nullability() {
    let sql = r#"WITH p AS (
        SELECT ARRAY_AGG(t)[SAFE_OFFSET(0)] AS item
        FROM other LEFT JOIN (SELECT x, y FROM src) t ON TRUE
    )
    SELECT item.* FROM p"#;
    assert_eq!(
        render(sql, Dialect::BigQuery),
        [
            "x#0 C[src.x] Aggregation",
            "y#1 C[src.y] Aggregation",
            "Closed"
        ]
    );
}

#[test]
fn postgres_composite_element_resolves_from_an_expression_star_ast() {
    let sql = r#"WITH p AS (
        SELECT (array_agg(t ORDER BY x))[1] AS item FROM (SELECT x, y FROM src) t
    ) SELECT (item).* FROM p"#;
    // sqlparser's PostgreSQL expression parser consumes the wildcard before
    // the SELECT projection can recognize it. Exercise the equivalent AST
    // with the PostgreSQL lineage rules.
    assert!(dlin_sqllineage::parse(sql, Dialect::PostgreSql).is_err());
    let analysis = dlin_sqllineage::parse(sql, Dialect::BigQuery)
        .unwrap()
        .analyze(&dlin_sqllineage::Analyzer::new(Dialect::PostgreSql))
        .into_single()
        .unwrap();
    assert_eq!(
        render_output(&analysis.output),
        [
            "x#0 C[src.x] Aggregation",
            "y#1 C[src.y] Aggregation",
            "Closed"
        ]
    );
}

#[test]
fn an_outer_column_does_not_shadow_a_local_row_alias() {
    let sql = r#"WITH outside AS (SELECT 1 AS t), src AS (SELECT x FROM raw)
        SELECT (p.item).* FROM outside CROSS JOIN LATERAL
            (SELECT (array_agg(t))[1] AS item FROM src t) p"#;
    let analysis = dlin_sqllineage::parse(sql, Dialect::BigQuery)
        .unwrap()
        .analyze(&dlin_sqllineage::Analyzer::new(Dialect::PostgreSql))
        .into_single()
        .unwrap();
    assert_eq!(
        render_output(&analysis.output),
        ["x#0 C[raw.x] Aggregation", "Closed"]
    );
}

#[test]
fn recursive_aggregate_element_shapes_do_not_loop() {
    let sql = r#"WITH RECURSIVE a AS (SELECT item FROM b),
        b AS (SELECT ARRAY_AGG(item)[OFFSET(0)] AS item FROM a)
        SELECT a.item.* FROM a"#;
    assert!(matches!(
        analyze_one_with(sql, Dialect::BigQuery, None).output.shape,
        Shape::Open { .. }
    ));
}

#[test]
fn aggregate_row_fields_can_come_from_an_exhaustive_catalog() {
    let sql = r#"SELECT (ARRAY_AGG(t)[OFFSET(0)]).* FROM src t"#;
    let catalog = common::KnownTables(&[("src", &["x", "y"])]);
    assert_eq!(
        render_output(&analyze_one_with(sql, Dialect::BigQuery, Some(&catalog)).output),
        [
            "x#0 C[src.x] Aggregation",
            "y#1 C[src.y] Aggregation",
            "Closed"
        ]
    );
}

#[test]
fn bigquery_struct_value_tables_expose_fields_through_a_cte() {
    for projection in [
        "SELECT AS STRUCT a AS x, b AS y FROM src",
        "SELECT AS VALUE STRUCT(a AS x, b AS y) FROM src",
    ] {
        for head in [
            "SELECT t.x, t.y FROM v t",
            "SELECT * FROM v",
            "SELECT t.* FROM v t",
        ] {
            let sql = format!(r#"WITH v AS ({projection}) {head}"#);
            assert_eq!(
                render(&sql, Dialect::BigQuery),
                ["x#0 C[src.a]", "y#1 C[src.b]", "Closed"],
                "{sql}"
            );
        }
        let sql = format!(
            r#"WITH v AS ({projection}),
            forwarded AS (SELECT AS VALUE t FROM v t),
            p AS (SELECT ARRAY_AGG(t)[OFFSET(0)] AS item FROM forwarded t)
            SELECT item.* FROM p"#
        );
        assert_eq!(
            render(&sql, Dialect::BigQuery),
            [
                "x#0 C[src.a] Aggregation",
                "y#1 C[src.b] Aggregation",
                "Closed"
            ],
            "{sql}"
        );
    }
}

#[test]
fn shape_and_value_use_the_same_alias_precedence() {
    let sql = r#"WITH a AS (SELECT STRUCT(col_a AS x) AS t FROM left_table),
        b AS (SELECT col_b AS y FROM right_table),
        p AS (SELECT ARRAY_AGG(t)[OFFSET(0)] AS item FROM a CROSS JOIN b AS t)
        SELECT item, item.* FROM p"#;
    assert_eq!(
        render(sql, Dialect::BigQuery),
        [
            "item#0 C[right_table.col_b] Expression",
            "y#1 C[right_table.col_b] Aggregation",
            "Closed"
        ]
    );
    assert_eq!(
        render(
            r#"WITH a AS (SELECT struct_pack(x := col_a) AS t FROM left_table),
            b AS (SELECT col_b AS y FROM right_table),
            p AS (SELECT list(t)[1] AS item FROM a CROSS JOIN b AS t)
            SELECT item, item.* FROM p"#,
            Dialect::DuckDb
        ),
        [
            "item#0 C[left_table.col_a] Expression",
            "x#1 C[left_table.col_a] Aggregation",
            "Closed"
        ]
    );
    let postgres = r#"WITH a AS (SELECT col_a AS t FROM left_table),
        b AS (SELECT col_b AS y FROM right_table)
        SELECT t FROM a CROSS JOIN b AS t"#;
    assert_eq!(
        render(postgres, Dialect::PostgreSql),
        ["t#0 C[left_table.col_a]", "Closed"]
    );
}

#[test]
fn as_value_subquery_reads_the_unexpanded_value_in_scalar_context() {
    for sql in [
        r#"SELECT (SELECT AS VALUE STRUCT(a AS x, b AS y) FROM src LIMIT 1) AS item"#,
        r#"SELECT (WITH q AS (SELECT a, b FROM src)
            SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM q LIMIT 1) AS item"#,
        r#"SELECT (WITH q AS (SELECT a, b FROM src)
            SELECT AS VALUE STRUCT(a AS x, b AS y) AS y FROM q LIMIT 1) AS item"#,
        r#"SELECT (WITH q AS (SELECT a, b FROM src)
            SELECT AS VALUE STRUCT(a AS x, b AS y) AS z FROM q LIMIT 1) AS item"#,
        r#"SELECT (WITH unused AS (SELECT AS VALUE STRUCT(c AS x) FROM decoy)
            SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM src LIMIT 1) AS item"#,
        r#"SELECT (WITH q AS (SELECT a, b FROM src)
            (WITH r AS (SELECT a, b FROM q)
                SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM r LIMIT 1)) AS item"#,
    ] {
        common::expect_rendered_in(
            sql,
            Dialect::BigQuery,
            &["item#0 C[src.a, src.b] Expression", "Closed"],
        );
    }
}

#[test]
fn as_value_with_is_read_by_array_and_in_but_not_exists() {
    for (sql, expected) in [
        (
            r#"SELECT ARRAY(WITH q AS (SELECT a, b FROM src)
                SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM q) AS item"#,
            "item#0 C[src.a, src.b] Expression",
        ),
        (
            r#"SELECT STRUCT(1 AS x, 2 AS y) IN (WITH q AS (SELECT a, b FROM src)
                SELECT AS VALUE STRUCT(a AS x, b AS y) AS y FROM q) AS item"#,
            "item#0 C[src.a, src.b] Expression",
        ),
        (
            r#"SELECT EXISTS(WITH q AS (SELECT a, b FROM src)
                SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM q) AS item"#,
            "item#0 C[] Expression",
        ),
    ] {
        common::expect_rendered_in(sql, Dialect::BigQuery, &[expected, "Closed"]);
    }
}

#[test]
fn value_table_set_operands_are_unsupported_in_expression_subqueries() {
    for sql in [
        r#"SELECT (WITH q AS (SELECT a, b FROM src)
            (SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM q)
            UNION ALL
            (WITH r AS (SELECT c, d FROM other_src)
                SELECT AS VALUE STRUCT(c AS x, d AS y) AS x FROM r)
            LIMIT 1) AS item"#,
        r#"SELECT (SELECT STRUCT(a AS x, b AS y) AS x FROM src
            UNION ALL
            SELECT AS VALUE STRUCT(c AS x, d AS y) AS x FROM other_src
            LIMIT 1) AS item"#,
    ] {
        common::expect_rendered_in(
            sql,
            Dialect::BigQuery,
            &[
                r#"item#0 P[ | Unsupported { what: "subquery value table set operation" }] Expression"#,
                "Closed",
            ],
        );
    }

    common::expect_rendered_in(
        r#"SELECT (WITH unused AS (SELECT AS VALUE STRUCT(c AS x) FROM decoy)
            SELECT a AS x FROM src UNION ALL SELECT b AS x FROM other_src
            LIMIT 1) AS item"#,
        Dialect::BigQuery,
        &["item#0 C[other_src.b, src.a] Expression", "Closed"],
    );
}

#[test]
fn as_value_with_still_exposes_fields_to_from_readers() {
    common::expect_rendered_in(
        r#"SELECT v.x, v.* FROM (WITH q AS (SELECT a, b FROM src)
            SELECT AS VALUE STRUCT(a AS x, b AS y) AS x FROM q) AS v"#,
        Dialect::BigQuery,
        &["x#0 C[src.a]", "x#1 C[src.a]", "y#2 C[src.b]", "Closed"],
    );
}

#[test]
fn as_struct_with_multiple_projections_is_unsupported_in_scalar_context() {
    common::expect_rendered_in(
        r#"SELECT (WITH q AS (SELECT a, b FROM src)
            SELECT AS STRUCT a AS x, b AS y FROM q LIMIT 1) AS item"#,
        Dialect::BigQuery,
        &[
            r#"item#0 P[ | Unsupported { what: "subquery value output" }] Expression"#,
            "Closed",
        ],
    );
}

#[test]
fn named_struct_fields_match_their_star_expansion_across_dialects() {
    for (dialect, constructor) in [
        (Dialect::BigQuery, "STRUCT(a AS x, b AS y)"),
        (Dialect::Databricks, "named_struct('x', a, 'y', b)"),
        (Dialect::Spark, "named_struct('x', a, 'y', b)"),
        (Dialect::DuckDb, "struct_pack(x := a, y := b)"),
    ] {
        let sql = format!(
            "WITH q AS (SELECT {constructor} AS s FROM src),
                expanded AS (SELECT s.* FROM q)
            SELECT q.s.x AS direct, expanded.x AS expanded
            FROM q CROSS JOIN expanded"
        );
        common::expect_rendered_in(
            &sql,
            dialect,
            &["direct#0 C[src.a]", "expanded#1 C[src.a]", "Closed"],
        );
    }
    common::expect_rendered_in(
        "WITH q AS (SELECT STRUCT(a AS x, b AS y) AS s FROM src) SELECT s.x AS x FROM q",
        Dialect::BigQuery,
        &["x#0 C[src.a]", "Closed"],
    );
}

#[test]
fn named_struct_fields_resolve_nested_paths_and_forwarded_values() {
    common::expect_rendered_in(
        r#"WITH q AS (
            SELECT STRUCT(STRUCT(a AS x, b AS y) AS inner_value, c AS z) AS s FROM src
        ), forwarded AS (
            SELECT p.s AS s, p.s.inner_value AS i FROM (SELECT * FROM q) AS p
        )
        SELECT s.inner_value.x AS direct, i.x + 1 AS computed FROM forwarded"#,
        Dialect::BigQuery,
        &[
            "direct#0 C[src.a]",
            "computed#1 C[src.a] Expression",
            "Closed",
        ],
    );
}

#[test]
fn named_fields_of_aggregate_elements_keep_the_index_and_aggregation() {
    common::expect_rendered_in(
        r#"WITH q AS (SELECT a AS x, b AS y FROM src),
        p AS (
            SELECT ARRAY_AGG(q ORDER BY y)[OFFSET(MAX(idx))] AS item
            FROM q CROSS JOIN (SELECT idx FROM offsets)
        )
        SELECT item.x AS x FROM p"#,
        Dialect::BigQuery,
        &["x#0 C[offsets.idx, src.a] Aggregation", "Closed"],
    );
}

#[test]
fn named_fields_of_structs_published_by_a_value_table() {
    common::expect_rendered_in(
        r#"SELECT v.s.x AS x FROM (
            SELECT AS VALUE STRUCT(STRUCT(a AS x, b AS y) AS s, c AS z)
            FROM src
        ) AS v"#,
        Dialect::BigQuery,
        &["x#0 C[src.a]", "Closed"],
    );
}

#[test]
fn named_struct_fields_narrow_only_for_a_unique_proven_path() {
    for (sql, expected) in [
        ("SELECT q.s.x AS x FROM src AS q", "x#0 C[src.s]"),
        (
            "WITH q AS (SELECT STRUCT(a AS x, b AS x) AS s FROM src)
             SELECT s.x AS x FROM q",
            "x#0 C[src.a, src.b] Expression",
        ),
        (
            "WITH q AS (SELECT STRUCT(a AS x, b AS y) AS s FROM src)
             SELECT s.missing AS x FROM q",
            "x#0 P[src.a, src.b | Unbound(missing)] Expression",
        ),
        (
            "WITH q AS (SELECT STRUCT(a AS x, b AS y) AS s FROM src),
                        open AS (SELECT * FROM unknown_source)
             SELECT s.x AS x FROM q CROSS JOIN open",
            "x#0 P[ | Unverified(s: cte q | cte open)] Expression",
        ),
    ] {
        common::expect_rendered_in(sql, Dialect::BigQuery, &[expected, "Closed"]);
    }
}

#[test]
fn subscript_keeps_the_named_struct_field_and_index_inputs() {
    common::expect_rendered_in(
        r#"WITH q AS (
            SELECT STRUCT([a] AS x, b AS y,
                STRUCT([c] AS x, d AS y) AS nested) AS s, i FROM src
        )
        SELECT q.s.x[OFFSET(0)] AS fixed,
            s.x[OFFSET(i)] AS dynamic,
            s.nested.x[SAFE_OFFSET(i)] AS nested
        FROM q"#,
        Dialect::BigQuery,
        &[
            "fixed#0 C[src.a] Expression",
            "dynamic#1 C[src.a, src.i] Expression",
            "nested#2 C[src.c, src.i] Expression",
            "Closed",
        ],
    );
}

#[test]
fn subscript_preserves_unknown_and_missing_struct_field_results() {
    common::expect_rendered_in(
        "SELECT q.s.x[OFFSET(i)] AS item FROM src AS q",
        Dialect::BigQuery,
        &["item#0 C[src.i, src.s] Expression", "Closed"],
    );
    common::expect_rendered_in(
        r#"WITH q AS (SELECT STRUCT([a] AS x, b AS y) AS s, i FROM src)
        SELECT s.missing[OFFSET(i)] AS item FROM q"#,
        Dialect::BigQuery,
        &[
            "item#0 P[src.a, src.b, src.i | Unbound(missing)] Expression",
            "Closed",
        ],
    );
}

#[test]
fn field_after_a_subscript_reads_the_selected_array_field_as_a_whole() {
    common::expect_rendered_in(
        r#"WITH q AS (
            SELECT STRUCT([STRUCT(a AS x, b AS y)] AS arr, c AS other) AS s
            FROM src
        )
        SELECT s.arr[OFFSET(0)].x AS item FROM q"#,
        Dialect::BigQuery,
        &["item#0 C[src.a, src.b] Expression", "Closed"],
    );
}
