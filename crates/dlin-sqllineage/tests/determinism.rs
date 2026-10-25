//! The analysis result must not depend on per-process hash seeds. Scope
//! bindings used to live in a hash map, so the order of mappings, the order
//! of `Ambiguous` candidates, and even which CTE a shared column resolved to
//! changed from run to run. Every hash map is built with its own random seed,
//! so repeating the analysis inside one process exposes the problem.

use common::{output_name, table};
use dlin_sqllineage::{
    Analyzer, Candidate, Dialect, Ident, Lineage, Relation, Shape, StatementAnalysis, Unresolved,
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

/// The relation behind every unexpanded star, in projection order. Every star
/// of these queries belongs to a relation.
fn wildcard_tables(result: &StatementAnalysis) -> Vec<Relation> {
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("expected unexpanded stars, got {:?}", result.output.shape);
    };
    unexpanded
        .iter()
        .map(|star| {
            let relation = star.relation.clone().expect("star without a relation");
            assert_eq!(
                star.lineage,
                Lineage::Partial {
                    known: Vec::new(),
                    unresolved: vec![Unresolved::BehindStar {
                        relation: relation.clone()
                    }],
                }
            );
            relation
        })
        .collect()
}

fn relation(name: &str) -> Candidate {
    Candidate::Relation(table(name))
}

fn cte(name: &str) -> Candidate {
    Candidate::Cte(Ident::unquoted(name))
}

fn derived(name: &str) -> Candidate {
    Candidate::Derived(Some(Ident::unquoted(name)))
}

/// The only unresolved item of the first output is an ambiguous `x` whose
/// candidates are exactly `expected`, in this order. The engine sorts
/// candidates (relations, then CTEs, then derived tables, each by name), so
/// they never follow the FROM clause.
fn assert_ambiguous(result: &StatementAnalysis, expected: &[Candidate]) {
    let Lineage::Partial { known, unresolved } = &result.output.columns[0].lineage else {
        panic!(
            "expected partial lineage, got {:?}",
            result.output.columns[0]
        );
    };
    assert!(known.is_empty());
    let [Unresolved::Ambiguous { column, candidates }] = &unresolved[..] else {
        panic!("expected one ambiguous column, got {unresolved:?}");
    };
    assert_eq!(column.value, "x");
    assert_eq!(candidates, expected);
}

/// The first output is complete and its sources are exactly `expected`, sorted
/// by relation and column rather than in FROM order.
fn assert_complete_sources(result: &StatementAnalysis, expected: &[(&str, &str)]) {
    let Lineage::Complete(sources) = &result.output.columns[0].lineage else {
        panic!(
            "expected complete lineage, got {:?}",
            result.output.columns[0]
        );
    };
    let actual: Vec<_> = sources
        .iter()
        .map(|source| {
            (
                source.relation.name.value.as_str(),
                source.column.value.as_str(),
            )
        })
        .collect();
    assert_eq!(actual, expected);
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
fn star_expansion_keeps_from_order_rather_than_name_order() {
    let result = stable(
        "SELECT * FROM t3 JOIN t1 ON t1.id = t3.id JOIN t2 ON t1.id = t2.id",
        Dialect::Generic,
    );
    assert_eq!(wildcard_tables(&result), tables(&["t3", "t1", "t2"]));
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
    // The unexpanded `t1` star comes first, so the lateral column's position
    // is unknown.
    let targets: Vec<_> = result
        .output
        .columns
        .iter()
        .map(|column| output_name(column))
        .collect();
    assert_eq!(targets, [Some("b")]);
    assert!(result.output.columns[0].ordinal.is_none());
    assert_eq!(wildcard_tables(&result), tables(&["t1"]));
}

#[test]
fn unnest_join_output_is_stable() {
    let result = stable(
        "SELECT x, y FROM t, UNNEST(t.arr) AS u(x), UNNEST(t.arr2) AS v(y)",
        Dialect::DuckDb,
    );
    assert_eq!(result.output.columns.len(), 2);
    let result = stable("SELECT * FROM t, UNNEST(t.arr) AS u(x)", Dialect::DuckDb);
    assert!(result.output.columns.is_empty());
    assert_eq!(wildcard_tables(&result), tables(&["t", "UNNEST"]));
}

#[test]
fn ambiguous_candidates_are_sorted_rather_than_in_from_clause_order() {
    for dialect in [Dialect::Generic, Dialect::MySql, Dialect::PostgreSql] {
        let result = stable(
            "SELECT x FROM t2 JOIN t3 ON t2.id = t3.id JOIN t1 ON t2.id = t1.id",
            dialect,
        );
        assert_ambiguous(&result, &[relation("t1"), relation("t2"), relation("t3")]);
    }
}

/// A column published by two CTEs in the same FROM clause cannot be attributed
/// to either of them. The old code returned whichever CTE the hash map yielded
/// first, as `Concrete`.
#[test]
fn column_shared_by_two_ctes_is_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM b JOIN a ON a.x = b.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &[cte("a"), cte("b")]);
}

#[test]
fn column_shared_by_two_derived_tables_is_ambiguous() {
    let result = stable(
        "SELECT x FROM (SELECT x FROM s1) d2 JOIN (SELECT x FROM s2) d1 ON d1.x = d2.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &[derived("d1"), derived("d2")]);
}

/// Candidates of different kinds are ordered by kind first: CTEs before
/// derived tables, wherever they appear in the FROM clause.
#[test]
fn column_shared_by_a_derived_table_and_a_cte_is_ambiguous() {
    let result = stable(
        "WITH c AS (SELECT x FROM s1) \
         SELECT x FROM (SELECT x FROM s2) AS d JOIN c ON c.x = d.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &[cte("c"), derived("d")]);
}

/// `USING` merges the joined columns into one, so the unqualified name is
/// legal SQL and not ambiguous. The merged column takes its value from both
/// relations, so both are sources, sorted by relation rather than in FROM order.
#[test]
fn using_join_column_is_not_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s2), b AS (SELECT x FROM s1) \
         SELECT x FROM a JOIN b USING (x)",
        Dialect::Generic,
    );
    assert_complete_sources(&result, &[("s1", "x"), ("s2", "x")]);
}

#[test]
fn natural_join_column_is_not_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s2), b AS (SELECT x FROM s1) \
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
        "WITH a AS (SELECT x FROM s1) SELECT x FROM a AS a2 JOIN a AS a1 ON a1.x = a2.x",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &[cte("a1"), cte("a2")]);
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
         SELECT x FROM a AS a2 LEFT JOIN a AS a3 USING (x), a AS a1",
        Dialect::Generic,
    );
    assert_ambiguous(&result, &[cte("a1"), cte("a2"), cte("a3")]);
}
