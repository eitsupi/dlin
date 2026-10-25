//! The transform of an output column covers every hop between the column and
//! its source relations, not only the last projection.

mod common;

use common::{concrete_sources, find_mapping};
use dlin_sqllineage::{Analyzer, Candidate, Dialect, Lineage, OutputColumn, Transform, Unresolved};

fn run(sql: &str, dialect: Dialect) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(dialect)
        .analyze(sql)
        .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
        .into_single()
        .unwrap()
}

fn mapping<'a>(result: &'a dlin_sqllineage::StatementAnalysis, target: &str) -> &'a OutputColumn {
    find_mapping(&result.output.columns, target)
}

fn pair(table: &str, column: &str) -> (String, String) {
    (table.to_string(), column.to_string())
}

const DIALECTS: [Dialect; 4] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::Snowflake,
    Dialect::BigQuery,
];

#[test]
fn aggregate_in_a_derived_table_is_seen_through_the_outer_reference() {
    for dialect in DIALECTS {
        for sql in [
            "SELECT c FROM (SELECT SUM(x) AS c FROM t) d",
            "SELECT d.c FROM (SELECT SUM(x) AS c FROM t) AS d",
            "WITH x AS (SELECT SUM(x) AS c FROM t) SELECT c FROM x",
        ] {
            let result = run(sql, dialect);
            let m = mapping(&result, "c");
            assert_eq!(m.transform, Transform::Aggregation, "{dialect:?}: {sql}");
            assert_eq!(concrete_sources(m), [pair("t", "x")], "{dialect:?}: {sql}");
        }
    }
}

#[test]
fn source_free_aggregate_is_still_an_aggregation_after_a_hop() {
    for dialect in DIALECTS {
        for sql in [
            "WITH x AS (SELECT COUNT(*) AS c FROM t) SELECT c FROM x",
            "SELECT c FROM (SELECT COUNT(*) AS c FROM t) d",
        ] {
            let result = run(sql, dialect);
            let m = mapping(&result, "c");
            assert_eq!(m.transform, Transform::Aggregation, "{dialect:?}: {sql}");
            assert!(
                matches!(&m.lineage, Lineage::Complete(sources) if sources.is_empty()),
                "{dialect:?}: {sql}"
            );
        }
    }
}

#[test]
fn star_and_named_selection_classify_alike() {
    let named = run(
        "SELECT c FROM (SELECT COUNT(*) AS c FROM t) d",
        Dialect::Generic,
    );
    let star = run(
        "SELECT * FROM (SELECT COUNT(*) AS c FROM t) d",
        Dialect::Generic,
    );
    assert_eq!(mapping(&named, "c").transform, Transform::Aggregation);
    assert_eq!(mapping(&star, "c").transform, Transform::Aggregation);
}

#[test]
fn transform_survives_several_hops() {
    let result = run(
        "WITH a AS (SELECT SUM(x) AS s FROM t), \
              b AS (SELECT s FROM a), \
              c AS (SELECT s FROM b) \
         SELECT s FROM c",
        Dialect::Generic,
    );
    let m = mapping(&result, "s");
    assert_eq!(m.transform, Transform::Aggregation);
    assert_eq!(concrete_sources(m), [pair("t", "x")]);

    let result = run(
        "SELECT s FROM (SELECT s FROM (SELECT SUM(x) AS s FROM t) d1) d2",
        Dialect::Generic,
    );
    assert_eq!(mapping(&result, "s").transform, Transform::Aggregation);
}

#[test]
fn strongest_transform_along_the_chain_wins() {
    // expression upstream, aggregate downstream
    let result = run(
        "WITH a AS (SELECT x + 1 AS s FROM t), b AS (SELECT MAX(s) AS s FROM a) SELECT s FROM b",
        Dialect::Generic,
    );
    assert_eq!(mapping(&result, "s").transform, Transform::Aggregation);
    // aggregate upstream, expression downstream
    let result = run(
        "WITH a AS (SELECT SUM(x) AS s FROM t), b AS (SELECT s + 1 AS s FROM a) SELECT s FROM b",
        Dialect::Generic,
    );
    assert_eq!(mapping(&result, "s").transform, Transform::Aggregation);
    // conditional upstream, plain reference downstream
    let result = run(
        "WITH a AS (SELECT CASE WHEN x > 1 THEN y END AS s FROM t) SELECT s FROM a",
        Dialect::Generic,
    );
    assert_eq!(mapping(&result, "s").transform, Transform::Conditional);
}

#[test]
fn plain_pass_through_stays_direct() {
    let result = run(
        "SELECT s FROM (SELECT x AS s, y FROM t) d",
        Dialect::Generic,
    );
    let m = mapping(&result, "s");
    assert_eq!(m.transform, Transform::Direct);
    assert_eq!(concrete_sources(m), [pair("t", "x")]);
}

/// An unqualified reference in a scope with more than one relation reaches
/// its source through the one relation that publishes the column. It must be
/// classified, and must keep every source, exactly like the qualified form.
#[test]
fn unqualified_reference_in_a_join_is_classified_like_the_qualified_one() {
    for dialect in DIALECTS {
        for (unqualified, qualified) in [
            (
                "SELECT s FROM (SELECT SUM(x) AS s FROM t) d JOIN u ON d.k = u.k",
                "SELECT d.s FROM (SELECT SUM(x) AS s FROM t) d JOIN u ON d.k = u.k",
            ),
            (
                "WITH a AS (SELECT SUM(x) AS s FROM t) SELECT s FROM a, u",
                "WITH a AS (SELECT SUM(x) AS s FROM t) SELECT a.s AS s FROM a, u",
            ),
        ] {
            let left = run(unqualified, dialect);
            let right = run(qualified, dialect);
            assert_eq!(
                mapping(&left, "s").transform,
                Transform::Aggregation,
                "{dialect:?}: {unqualified}"
            );
            assert_eq!(
                mapping(&left, "s").transform,
                mapping(&right, "s").transform,
                "{dialect:?}: {unqualified}"
            );
            assert_eq!(
                concrete_sources(mapping(&left, "s")),
                concrete_sources(mapping(&right, "s")),
                "{dialect:?}: {unqualified}"
            );
        }
    }
}

#[test]
fn unqualified_source_free_aggregate_in_a_join_is_an_aggregation() {
    let result = run(
        "WITH a AS (SELECT COUNT(*) AS c FROM t) SELECT c FROM a JOIN u ON true",
        Dialect::Generic,
    );
    let m = mapping(&result, "c");
    assert_eq!(m.transform, Transform::Aggregation);
    assert!(matches!(&m.lineage, Lineage::Complete(sources) if sources.is_empty()));
}

#[test]
fn unqualified_reference_in_a_join_keeps_every_set_operation_branch() {
    for dialect in DIALECTS {
        let result = run(
            "SELECT s FROM (SELECT x AS s FROM t UNION ALL SELECT y FROM u2) d \
             JOIN v ON d.k = v.k",
            dialect,
        );
        assert_eq!(
            concrete_sources(mapping(&result, "s")),
            [pair("t", "x"), pair("u2", "y")],
            "{dialect:?}"
        );
    }
}

#[test]
fn unqualified_reference_between_two_providers_is_still_ambiguous() {
    let result = run(
        "SELECT s FROM (SELECT SUM(x) AS s FROM t) d1 JOIN (SELECT COUNT(y) AS s FROM u) d2 ON true",
        Dialect::Generic,
    );
    assert!(matches!(
        &mapping(&result, "s").lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(&unresolved[..], [Unresolved::Ambiguous { column, candidates }]
                if column.value == "s" && candidates.len() == 2
                    && candidates.iter().all(|candidate| matches!(candidate, Candidate::Derived(Some(_)))))
    ));
}
