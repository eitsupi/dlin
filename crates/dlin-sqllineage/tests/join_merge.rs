//! A `USING` or `NATURAL` join merges a column only among the relations it
//! joins. A relation outside that join which also publishes the name still
//! makes an unqualified reference a real choice, so the merge must not be
//! treated as a property of the whole FROM clause.

use common::known_sources;
use dlin_sqllineage::{Analyzer, Candidate, Dialect, Lineage, StatementAnalysis, Unresolved};
mod common;

const RUNS: usize = 50;

fn run(sql: &str) -> Vec<StatementAnalysis> {
    Analyzer::new(Dialect::Generic)
        .analyze(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .statements
}

/// Analyze `sql` repeatedly and require identical output every time.
fn stable(sql: &str) -> StatementAnalysis {
    let first = run(sql);
    let expected = format!("{first:#?}");
    for attempt in 1..RUNS {
        let again = format!("{:#?}", run(sql));
        assert_eq!(
            expected, again,
            "run {attempt} differs from run 0 for: {sql}"
        );
    }
    first.into_iter().next().expect("one statement")
}

fn assert_ambiguous(sql: &str, expected_candidates: &[&str]) {
    let result = stable(sql);
    assert!(
        matches!(&result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(&unresolved[..], [Unresolved::Ambiguous { column, candidates }]
                if column.value == "x" && candidates.len() == expected_candidates.len()
                    && expected_candidates.iter().all(|name| candidates.iter().any(|candidate| match candidate {
                        Candidate::Cte(ident) | Candidate::Derived(Some(ident)) => ident.value == *name,
                        Candidate::Relation(relation) => relation.name.value == *name,
                        Candidate::Derived(None) => false,
                    })))),
        "expected the same ambiguous providers for `{sql}`: {:?}",
        result.output.columns[0].lineage
    );
}

/// The sources must be exactly `expected_tables`' `x` columns, sorted by source.
fn assert_sources(sql: &str, expected_tables: &[&str]) {
    assert_sources_in(&stable(sql), sql, expected_tables);
}

fn assert_sources_in(result: &StatementAnalysis, sql: &str, expected_tables: &[&str]) {
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(_)),
        "expected complete lineage for `{sql}`: {:?}",
        result.output.columns[0].lineage
    );
    let actual: Vec<_> = known_sources(&result.output.columns[0])
        .iter()
        .map(|source| {
            (
                source.relation.name.value.as_str(),
                source.column.value.as_str(),
            )
        })
        .collect();
    let mut expected: Vec<_> = expected_tables.iter().map(|table| (*table, "x")).collect();
    expected.sort();
    assert_eq!(actual, expected, "{sql}");
}

const THREE_CTES: &str = "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2), \
                          c AS (SELECT x FROM s3) ";

#[test]
fn using_join_does_not_merge_a_comma_joined_relation() {
    assert_ambiguous(
        &format!("{THREE_CTES} SELECT x FROM a JOIN b USING (x), c"),
        &["a", "b", "c"],
    );
}

#[test]
fn literal_ctes_using_join_with_comma_joined_relation_are_ambiguous() {
    assert_ambiguous(
        "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS x), c AS (SELECT 3 AS x) \
         SELECT x FROM a JOIN b USING (x), c",
        &["a", "b", "c"],
    );
}

#[test]
fn natural_join_does_not_merge_a_cross_joined_relation() {
    assert_ambiguous(
        &format!("{THREE_CTES} SELECT x FROM a NATURAL JOIN b CROSS JOIN c"),
        &["a", "b", "c"],
    );
}

#[test]
fn literal_ctes_natural_join_with_cross_joined_relation_are_ambiguous() {
    assert_ambiguous(
        "WITH a AS (SELECT 1 AS x), b AS (SELECT 2 AS x), c AS (SELECT 3 AS x) \
         SELECT x FROM a NATURAL JOIN b CROSS JOIN c",
        &["a", "b", "c"],
    );
}

#[test]
fn using_join_followed_by_on_join_of_another_provider_is_ambiguous() {
    assert_ambiguous(
        &format!("{THREE_CTES} SELECT x FROM a JOIN b USING (x) JOIN c ON a.x = c.x"),
        &["a", "b", "c"],
    );
}

#[test]
fn using_join_on_other_column_does_not_merge_the_name() {
    assert_ambiguous(
        "WITH a AS (SELECT x, y FROM s1), b AS (SELECT x, y FROM s2) \
         SELECT x FROM a JOIN b USING (y)",
        &["a", "b"],
    );
}

#[test]
fn using_join_after_unmerged_join_stays_ambiguous() {
    assert_ambiguous(
        &format!("{THREE_CTES} SELECT x FROM a JOIN b ON a.x = b.x JOIN c USING (x)"),
        &["a", "b", "c"],
    );
}

#[test]
fn parenthesized_using_join_does_not_merge_an_outer_relation() {
    assert_ambiguous(
        &format!("{THREE_CTES} SELECT x FROM a JOIN (b JOIN c USING (x)) ON a.id = b.id"),
        &["a", "b", "c"],
    );
    assert_ambiguous(
        &format!("{THREE_CTES} SELECT x FROM (a JOIN b USING (x)) JOIN c ON a.id = c.id"),
        &["a", "b", "c"],
    );
}

#[test]
fn derived_tables_merged_by_using_do_not_cover_a_third_provider() {
    assert_ambiguous(
        "SELECT x FROM (SELECT x FROM s1) d1 JOIN (SELECT x FROM s2) d2 USING (x), \
         (SELECT x FROM s3) d3",
        &["d1", "d2", "d3"],
    );
}

#[test]
fn using_join_of_two_relations_is_not_ambiguous() {
    assert_sources(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) SELECT x FROM a JOIN b USING (x)",
        &["s1", "s2"],
    );
}

#[test]
fn natural_join_of_two_relations_is_not_ambiguous() {
    assert_sources(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) SELECT x FROM a NATURAL JOIN b",
        &["s1", "s2"],
    );
}

#[test]
fn chained_using_joins_merge_all_relations() {
    assert_sources(
        &format!("{THREE_CTES} SELECT x FROM a JOIN b USING (x) JOIN c USING (x)"),
        &["s1", "s2", "s3"],
    );
}

#[test]
fn chained_natural_joins_merge_all_relations() {
    assert_sources(
        &format!("{THREE_CTES} SELECT x FROM a NATURAL JOIN b NATURAL JOIN c"),
        &["s1", "s2", "s3"],
    );
}

#[test]
fn parenthesized_joins_merge_across_the_nesting() {
    assert_sources(
        &format!("{THREE_CTES} SELECT x FROM a JOIN (b JOIN c USING (x)) USING (x)"),
        &["s1", "s2", "s3"],
    );
    assert_sources(
        &format!("{THREE_CTES} SELECT x FROM (a JOIN b USING (x)) JOIN c USING (x)"),
        &["s1", "s2", "s3"],
    );
}

#[test]
fn physical_table_joined_to_a_cte_by_using_is_not_ambiguous() {
    assert_sources(
        "WITH a AS (SELECT x FROM s1) SELECT x FROM t JOIN a USING (x)",
        &["t", "s1"],
    );
    assert_sources(
        "WITH a AS (SELECT x FROM s1) SELECT x FROM a JOIN t USING (x)",
        &["s1", "t"],
    );
}

#[test]
fn physical_table_merged_with_two_ctes_by_using_chain_is_not_ambiguous() {
    assert_sources(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM t JOIN a USING (x) JOIN b USING (x)",
        &["t", "s1", "s2"],
    );
}

#[test]
fn merged_join_columns_keep_the_transform_of_the_provider() {
    let result = stable(
        "WITH a AS (SELECT SUM(v) AS x FROM s1), b AS (SELECT SUM(v) AS x FROM s2) \
         SELECT x FROM a JOIN b USING (x), (SELECT 1 AS y) d",
    );
    assert_eq!(
        result.output.columns[0].transform,
        dlin_sqllineage::Transform::Aggregation
    );
}
