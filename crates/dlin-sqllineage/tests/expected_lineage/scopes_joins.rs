use super::support::*;
use dlin_sqllineage::{Analyzer, Dialect, OutputColumn, OutputName, SourceColumn, Transform};

pub fn cases() -> Vec<Case> {
    let mut cases = vec![
        scope(
            "cte_alias_preserves_base_column",
            "WITH c AS (SELECT id AS value FROM a) SELECT value FROM c",
            &[("value", "a", "id")],
        ),
        scope(
            "cte_sibling_chain",
            "WITH c AS (SELECT id AS x FROM a), d AS (SELECT x AS y FROM c) SELECT y FROM d",
            &[("y", "a", "id")],
        ),
        scope(
            "nested_cte_shadows_outer_cte",
            "WITH c AS (SELECT id FROM a) SELECT id FROM (WITH c AS (SELECT id FROM b) SELECT id FROM c) d",
            &[("id", "b", "id")],
        ),
        scope(
            "nested_cte_can_reference_outer_cte",
            "WITH c AS (SELECT id FROM a) SELECT value FROM (WITH d AS (SELECT id AS value FROM c) SELECT value FROM d) q",
            &[("value", "a", "id")],
        ),
        scope(
            "derived_alias_preserves_source",
            "SELECT d.value FROM (SELECT id AS value FROM a) d",
            &[("value", "a", "id")],
        ),
        scope(
            "nested_derived_projection_order",
            "SELECT d.y, d.x FROM (SELECT x, y FROM (SELECT id AS x, amount AS y FROM a) q) d",
            &[("y", "a", "amount"), ("x", "a", "id")],
        ),
        scope(
            "scalar_subquery_value_only",
            "SELECT (SELECT b.amount FROM b WHERE b.id = 1) AS value FROM a",
            &[("value", "b", "amount")],
        ),
        scope(
            "scalar_subquery_and_outer_projection",
            "SELECT a.id, (SELECT b.amount FROM b) AS value FROM a",
            &[("id", "a", "id"), ("value", "b", "amount")],
        ),
        scope(
            "correlated_predicate_is_not_value_lineage",
            "SELECT (SELECT b.amount FROM b WHERE b.id = a.id) AS value FROM a",
            &[("value", "b", "amount")],
        ),
        scope(
            "correlated_outer_value_is_a_source",
            "SELECT (SELECT a.amount FROM b WHERE b.id = a.id) AS value FROM a",
            &[("value", "a", "amount")],
        ),
        scope(
            "inner_alias_shadows_outer_alias",
            "SELECT (SELECT x.amount FROM b x) AS value FROM a x",
            &[("value", "b", "amount")],
        ),
        scope(
            "shadowed_alias_does_not_escape_subquery",
            "SELECT x.id, (SELECT x.amount FROM b x) AS value FROM a x",
            &[("id", "a", "id"), ("value", "b", "amount")],
        ),
        scope(
            "cte_used_twice_traces_physical_source",
            "WITH c AS (SELECT id FROM a) SELECT l.id AS left_id, r.id AS right_id FROM c l JOIN c r ON l.id = r.id",
            &[("left_id", "a", "id"), ("right_id", "a", "id")],
        ),
        scope(
            "derived_same_alias_in_sibling_scopes",
            "SELECT l.value AS left_value, r.value AS right_value FROM (SELECT x.id AS value FROM a x) l CROSS JOIN (SELECT x.amount AS value FROM b x) r",
            &[("left_value", "a", "id"), ("right_value", "b", "amount")],
        ),
    ];

    cases.extend(scalar_value_expectations().into_iter().map(|case| {
        Case::new(
            case.name,
            case.sql,
            Dialect::Generic,
            vec![col(0, u("value"), case.sources)],
        )
    }));
    cases.push(Case::new(
        "exists_projection_keeps_predicate_sources_out",
        "SELECT EXISTS(SELECT b.amount FROM b WHERE b.id = a.id) AS flag FROM a",
        Dialect::Generic,
        vec![col(0, u("flag"), vec![])],
    ));
    cases.push(scope(
        "in_subquery_keeps_only_left_value_source",
        "SELECT a.id IN (SELECT b.id FROM b WHERE b.amount > 0) AS flag FROM a",
        &[("flag", "a", "id")],
    ));

    // A merged INNER/FULL key can take values from both sides; LEFT/RIGHT
    // keys take their value from the preserved side.
    for (label, join, sources) in [
        ("inner", "INNER JOIN", vec!["a", "b"]),
        ("left", "LEFT JOIN", vec!["a"]),
        ("right", "RIGHT JOIN", vec!["b"]),
        ("full", "FULL JOIN", vec!["a", "b"]),
    ] {
        for natural in [false, true] {
            let (name, sql) = if natural {
                (
                    format!("natural_{label}_merged_value"),
                    format!("SELECT id FROM a NATURAL {join} b"),
                )
            } else {
                (
                    format!("using_{label}_merged_value"),
                    format!("SELECT id FROM a {join} b USING (id)"),
                )
            };
            cases.push(merged(&name, &sql, &sources));
        }
    }
    for (name, sql, sources) in [
        (
            "using_three_inner_relations",
            "SELECT id FROM a JOIN b USING (id) JOIN c USING (id)",
            vec!["a", "b", "c"],
        ),
        (
            "using_left_then_full",
            "SELECT id FROM a LEFT JOIN b USING (id) FULL JOIN c USING (id)",
            vec!["a", "c"],
        ),
        (
            "using_inner_then_right",
            "SELECT id FROM a JOIN b USING (id) RIGHT JOIN c USING (id)",
            vec!["c"],
        ),
        (
            "natural_three_inner_relations",
            "SELECT id FROM a NATURAL JOIN b NATURAL JOIN c",
            vec!["a", "b", "c"],
        ),
        (
            "natural_left_then_full",
            "SELECT id FROM a NATURAL LEFT JOIN b NATURAL FULL JOIN c",
            vec!["a", "c"],
        ),
    ] {
        cases.push(merged(name, sql, &sources));
    }
    for (name, sql, tables) in [
        (
            "cte_using_join_follows_base_sources",
            "WITH l AS (SELECT id FROM a), r AS (SELECT id FROM b) SELECT id FROM l JOIN r USING (id)",
            vec!["a", "b"],
        ),
        (
            "derived_full_join_follows_base_sources",
            "SELECT id FROM (SELECT id FROM a) l FULL JOIN (SELECT id FROM b) r USING (id)",
            vec!["a", "b"],
        ),
    ] {
        cases.push(merged(name, sql, &tables));
    }

    for (name, sql, tables) in [
        (
            "using_star_merges_key_once",
            "SELECT * FROM a JOIN b USING (id)",
            vec!["a", "b"],
        ),
        (
            "natural_star_merges_key_once",
            "SELECT * FROM a NATURAL JOIN b",
            vec!["a", "b"],
        ),
        (
            "using_left_star_preserved_key",
            "SELECT * FROM a LEFT JOIN b USING (id)",
            vec!["a"],
        ),
        (
            "using_right_star_preserved_key",
            "SELECT * FROM a RIGHT JOIN b USING (id)",
            vec!["b"],
        ),
        (
            "using_full_star_coalesced_key",
            "SELECT * FROM a FULL JOIN b USING (id)",
            vec!["a", "b"],
        ),
        (
            "natural_left_star_preserved_key",
            "SELECT * FROM a NATURAL LEFT JOIN b",
            vec!["a"],
        ),
        (
            "natural_right_star_preserved_key",
            "SELECT * FROM a NATURAL RIGHT JOIN b",
            vec!["b"],
        ),
        (
            "natural_full_star_coalesced_key",
            "SELECT * FROM a NATURAL FULL JOIN b",
            vec!["a", "b"],
        ),
    ] {
        // SQL's merged key precedes the non-key columns from each side.
        cases.push(
            Case::new(
                name,
                sql,
                Dialect::Generic,
                vec![
                    col(
                        0,
                        u("id"),
                        tables
                            .iter()
                            .map(|table| source(relation(table), u("id")))
                            .collect(),
                    ),
                    col(
                        1,
                        u("left_value"),
                        vec![source(relation("a"), u("left_value"))],
                    ),
                    col(
                        2,
                        u("right_value"),
                        vec![source(relation("b"), u("right_value"))],
                    ),
                ],
            )
            .catalog(vec![
                (relation("a"), vec![u("left_value"), u("id")]),
                (relation("b"), vec![u("right_value"), u("id")]),
            ])
            .merged_star_failure(
                vec![
                    col(
                        0,
                        u("left_value"),
                        vec![source(relation("a"), u("left_value"))],
                    ),
                    col(1, u("id"), vec![source(relation("a"), u("id"))]),
                    col(
                        2,
                        u("right_value"),
                        vec![source(relation("b"), u("right_value"))],
                    ),
                    col(3, u("id"), vec![source(relation("b"), u("id"))]),
                ],
                vec![duplicate_name(u("id"), vec![1, 3])],
                "Star expansion repeats the merged join key instead of emitting it once.",
            ),
        );
    }
    cases
}

fn scope(name: &str, sql: &str, columns: &[(&str, &str, &str)]) -> Case {
    Case::new(
        name,
        sql,
        Dialect::Generic,
        columns
            .iter()
            .enumerate()
            .map(|(ordinal, (name, table, column))| {
                col(ordinal, u(name), vec![source(relation(table), u(column))])
            })
            .collect(),
    )
}

fn merged(name: &str, sql: &str, tables: &[&str]) -> Case {
    Case::new(
        name,
        sql,
        Dialect::Generic,
        vec![col(
            0,
            u("id"),
            tables
                .iter()
                .map(|table| source(relation(table), u("id")))
                .collect(),
        )],
    )
    .catalog(vec![
        (relation("a"), vec![u("id"), u("left_value")]),
        (relation("b"), vec![u("id"), u("right_value")]),
        (relation("c"), vec![u("id"), u("third_value")]),
    ])
}

struct ScalarExpectation {
    name: &'static str,
    sql: &'static str,
    sources: Vec<SourceColumn>,
    transform: Transform,
}

fn scalar_value_expectations() -> Vec<ScalarExpectation> {
    [
        ("scalar_aggregate_value", "SELECT (SELECT max(b.amount) FROM b) AS value FROM a", vec![("b", "amount")], Transform::Aggregation),
        ("scalar_in_arithmetic", "SELECT 1 + (SELECT b.amount FROM b) AS value FROM a", vec![("b", "amount")], Transform::Expression),
        ("scalar_and_outer_value_in_arithmetic", "SELECT a.id + (SELECT b.amount FROM b WHERE b.id = a.id) AS value FROM a", vec![("a", "id"), ("b", "amount")], Transform::Expression),
        ("scalar_aggregate_in_arithmetic", "SELECT 1 + (SELECT max(b.amount) FROM b) AS value FROM a", vec![("b", "amount")], Transform::Aggregation),
        ("scalar_aggregate_in_function", "SELECT coalesce((SELECT max(b.amount) FROM b), 0) AS value FROM a", vec![("b", "amount")], Transform::Aggregation),
        ("scalar_source_free_aggregate", "SELECT (SELECT count(*) FROM b) AS value FROM a", vec![], Transform::Aggregation),
        ("scalar_correlated_outer_value_with_having", "SELECT (SELECT a.amount FROM b WHERE b.id = a.id HAVING max(b.id) > 0) AS value FROM a", vec![("a", "amount")], Transform::Expression),
        ("scalar_with_cte_projection", "SELECT (WITH c AS (SELECT b.amount AS x FROM b) SELECT x FROM c) AS value FROM a", vec![("b", "amount")], Transform::Expression),
        ("scalar_set_operation_projection", "SELECT (SELECT b.amount FROM b UNION ALL SELECT a.amount) AS value FROM a", vec![("a", "amount"), ("b", "amount")], Transform::Expression),
        ("scalar_nested_value", "SELECT (SELECT (SELECT a.amount FROM b) FROM b) AS value FROM a", vec![("a", "amount")], Transform::Expression),
    ].into_iter().map(|(name, sql, sources, transform)| ScalarExpectation {
        name, sql, transform,
        sources: sources.into_iter().map(|(table, column)| source(relation(table), u(column))).collect(),
    }).collect()
}

#[test]
fn scalar_value_sources_and_transforms_follow_projection_paths() {
    for case in scalar_value_expectations() {
        let result = Analyzer::new(Dialect::Generic)
            .analyze(case.sql)
            .unwrap()
            .into_single()
            .unwrap();
        assert_eq!(
            result.output.columns,
            vec![OutputColumn {
                ordinal: Some(0),
                name: OutputName::Explicit(u("value")),
                lineage: complete(case.sources),
                transform: case.transform,
            }],
            "{}",
            case.name
        );
        assert_eq!(
            result.read,
            vec![relation("a"), relation("b")],
            "{}",
            case.name
        );
        assert!(
            result.diagnostics.is_empty(),
            "{}: {:?}",
            case.name,
            result.diagnostics
        );
    }
}
