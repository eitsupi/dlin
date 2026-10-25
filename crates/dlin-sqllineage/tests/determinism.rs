//! The analysis result must not depend on per-process hash seeds. Scope
//! bindings used to live in a hash map, so the order of mappings, the order
//! of `Ambiguous` candidates, and even which CTE a shared column resolved to
//! changed from run to run. Every hash map is built with its own random seed,
//! so repeating the analysis inside one process exposes the problem.

use common::{output_name, table};
use dlin_sqllineage::{
    Analyzer, Candidate, Dialect, Lineage, Relation, StatementAnalysis, Unresolved,
};
mod common;

const RUNS: usize = 50;

fn run(sql: &str, dialect: Dialect) -> Vec<StatementAnalysis> {
    Analyzer::new(dialect)
        .analyze(sql)
        .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
        .statements
}

/// Analyze `sql` repeatedly and require byte-identical output every time.
fn stable(sql: &str, dialect: Dialect) -> StatementAnalysis {
    let first = run(sql, dialect);
    let expected = format!("{first:#?}");
    for attempt in 1..RUNS {
        let again = format!("{:#?}", run(sql, dialect));
        assert_eq!(
            expected, again,
            "{dialect:?} run {attempt} differs from run 0 for: {sql}"
        );
    }
    first.into_iter().next().expect("one statement")
}

fn tables(names: &[&str]) -> Vec<Relation> {
    names.iter().map(|name| table(name)).collect()
}

fn wildcard_tables(result: &StatementAnalysis) -> Vec<Relation> {
    match &result.output.shape {
        dlin_sqllineage::Shape::Open { unexpanded } => unexpanded
            .iter()
            .filter_map(|star| star.relation.clone())
            .collect(),
        dlin_sqllineage::Shape::Closed => Vec::new(),
    }
}

fn assert_ambiguous(result: &StatementAnalysis, names: &[&str]) {
    assert!(matches!(&result.output.columns[0].lineage,
    Lineage::Partial { unresolved, .. }
        if matches!(&unresolved[..], [Unresolved::Ambiguous { column, candidates }]
            if column.value == "x" && candidates.len() == names.len()
                && names.iter().all(|name| candidates.iter().any(|candidate| match candidate {
                    Candidate::Relation(r) => r.name.value == *name,
                    Candidate::Cte(id) | Candidate::Derived(Some(id)) => id.value == *name,
                    Candidate::Derived(None) => false,
                })))));
}

fn assert_complete_sources(result: &StatementAnalysis, expected: &[(&str, &str)]) {
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(sources)
        if sources.iter().map(|source| (source.relation.name.value.as_str(), source.column.value.as_str())).collect::<Vec<_>>() == expected)
    );
}

#[test]
fn star_expansion_follows_from_clause_order() {
    let result = stable(
        "SELECT * FROM t1 JOIN t2 ON t1.id = t2.id JOIN t3 ON t1.id = t3.id",
        Dialect::Generic,
    );
    assert_eq!(wildcard_tables(&result), tables(&["t1", "t2", "t3"]));
}

#[test]
fn mysql_chained_left_joins_expand_in_from_order() {
    let result = stable(
        "SELECT * FROM a LEFT JOIN b ON a.id = b.id LEFT JOIN c ON b.id = c.id",
        Dialect::MySql,
    );
    assert_eq!(wildcard_tables(&result), tables(&["a", "b", "c"]));
}

#[test]
fn lateral_join_output_is_stable() {
    let result = stable(
        "SELECT * FROM t1, LATERAL (SELECT b FROM t2 WHERE t2.id = t1.id) AS lat",
        Dialect::Generic,
    );
    let targets: Vec<_> = result
        .output
        .columns
        .iter()
        .map(|column| output_name(column))
        .collect();
    assert_eq!(targets, [Some("b")]);
    assert!(result.output.columns[0].ordinal.is_none());
}

#[test]
fn unnest_join_output_is_stable() {
    let result = stable(
        "SELECT x, y FROM t, UNNEST(t.arr) AS u(x), UNNEST(t.arr2) AS v(y)",
        Dialect::DuckDb,
    );
    assert_eq!(result.output.columns.len(), 2);
    let result = stable("SELECT * FROM t, UNNEST(t.arr) AS u(x)", Dialect::DuckDb);
    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    assert!(result.output.columns.is_empty());
    assert!(
        matches!(&result.output.shape, dlin_sqllineage::Shape::Open { unexpanded } if unexpanded.len() == 2)
    );
}

#[test]
fn ambiguous_candidates_follow_from_clause_order() {
    for dialect in [Dialect::Generic, Dialect::MySql, Dialect::PostgreSql] {
        let result = stable(
            "SELECT x FROM t1 JOIN t2 ON t1.id = t2.id JOIN t3 ON t1.id = t3.id",
            dialect,
        );
        assert_ambiguous(&result, &["t1", "t2", "t3"]);
    }
}

/// A column published by two CTEs in the same FROM clause cannot be attributed
/// to either of them. The old code returned whichever CTE the hash map yielded
/// first, as `Concrete`.
#[test]
fn column_shared_by_two_ctes_is_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a JOIN b ON a.x = b.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &["a", "b"]);
}

#[test]
fn column_shared_by_two_derived_tables_is_ambiguous() {
    let result = stable(
        "SELECT x FROM (SELECT x FROM s1) d1 JOIN (SELECT x FROM s2) d2 ON d1.x = d2.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &["d1", "d2"]);
}

/// `USING` merges the joined columns into one, so the unqualified name is
/// legal SQL and not ambiguous. The merged column takes its value from both
/// relations, so both are sources, in FROM order.
#[test]
fn using_join_column_is_not_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a JOIN b USING (x)",
        Dialect::Generic,
    );
    assert_complete_sources(&result, &[("s1", "x"), ("s2", "x")]);
}

#[test]
fn natural_join_column_is_not_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a NATURAL JOIN b",
        Dialect::Generic,
    );
    assert_complete_sources(&result, &[("s1", "x"), ("s2", "x")]);
}

/// A column that only one CTE publishes still resolves through that CTE, even
/// when a physical table of unknown shape is joined as well.
#[test]
fn single_cte_provider_still_wins_over_tables() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1) SELECT x FROM a JOIN t2 ON a.id = t2.id",
        Dialect::Generic,
    );
    assert_complete_sources(&result, &[("s1", "x")]);
}

/// Each alias is a separate relation, even when both read the same CTE.
#[test]
fn self_join_of_one_cte_is_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1) SELECT x FROM a AS a1 JOIN a AS a2 ON a1.x = a2.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &["a1", "a2"]);
}

#[test]
fn self_join_of_one_cte_merges_using_and_natural_columns() {
    for join in [
        "JOIN a AS a2 USING (x)",
        "LEFT JOIN a AS a2 USING (x)",
        "RIGHT JOIN a AS a2 USING (x)",
        "FULL JOIN a AS a2 USING (x)",
        "NATURAL JOIN a AS a2",
        "NATURAL LEFT JOIN a AS a2",
        "NATURAL RIGHT JOIN a AS a2",
        "NATURAL FULL JOIN a AS a2",
    ] {
        let result = stable(
            &format!("WITH a AS (SELECT x FROM s1) SELECT x FROM a AS a1 {join}"),
            Dialect::Generic,
        );
        assert_complete_sources(&result, &[("s1", "x")]);
    }
}

#[test]
fn self_join_alias_outside_a_merge_keeps_the_column_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1) \
         SELECT x FROM a AS a1 LEFT JOIN a AS a2 USING (x), a AS a3",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &["a1", "a2", "a3"]);
}
