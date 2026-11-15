//! A subquery is the same query wherever it is written: in the projection, in a
//! filter, in a join condition or in ORDER BY it gets its own scope and reads
//! its relations, and it never adds outputs or names to the query around it.
//! An expression the engine does not model is unresolved, never source-free.

mod common;

use common::{analyze_one_with, analyze_rendered};
use dlin_sqllineage::Dialect;

fn read_names(sql: &str, dialect: Dialect) -> Vec<String> {
    let mut names: Vec<String> = analyze_one_with(sql, dialect, None)
        .read
        .iter()
        .map(|relation| relation.name.value.clone())
        .collect();
    names.sort();
    names
}

#[test]
fn every_clause_reads_the_relations_of_its_subqueries() {
    for (dialect, sql) in [
        (
            Dialect::Generic,
            "SELECT a FROM t GROUP BY a HAVING a IN (SELECT b FROM u)",
        ),
        (
            Dialect::Generic,
            "SELECT a FROM t GROUP BY a, (SELECT max(b) FROM u)",
        ),
        (
            Dialect::Snowflake,
            "SELECT a FROM t QUALIFY a = (SELECT max(b) FROM u)",
        ),
        (
            Dialect::Generic,
            "SELECT t.a FROM t JOIN v ON v.id IN (SELECT b FROM u)",
        ),
        (
            Dialect::Generic,
            "SELECT a FROM t ORDER BY (SELECT max(b) FROM u)",
        ),
        // Filters the old table walk did not look into.
        (
            Dialect::Generic,
            "SELECT a FROM t WHERE a LIKE (SELECT max(b) FROM u)",
        ),
        (
            Dialect::PostgreSql,
            "SELECT a FROM t WHERE a = ANY (SELECT b FROM u)",
        ),
    ] {
        let names = read_names(sql, dialect);
        assert!(names.contains(&"u".to_string()), "{sql}: {names:?}");
    }
}

#[test]
fn a_subquery_in_a_filter_adds_nothing_to_the_query_around_it() {
    // E6: the inner projection and FROM must not leak into the outer query.
    let filtered = analyze_rendered(
        "SELECT a FROM t WHERE ARRAY_LENGTH(ARRAY(SELECT b FROM u)) > 0",
        Dialect::BigQuery,
        None,
    );
    assert_eq!(filtered, ["a#0 C[t.a]", "Closed"]);
    // Moving the condition between clauses does not change the other output.
    for sql in [
        "SELECT a FROM t WHERE EXISTS (SELECT b FROM u WHERE u.b = t.a)",
        "SELECT a FROM t GROUP BY a HAVING EXISTS (SELECT b FROM u WHERE u.b = t.a)",
        "SELECT a FROM t ORDER BY (SELECT max(b) FROM u WHERE u.b = t.a)",
        "SELECT a, EXISTS (SELECT b FROM u WHERE u.b = t.a) AS e FROM t",
    ] {
        let rendered = analyze_rendered(sql, Dialect::Generic, None);
        assert_eq!(rendered[0], "a#0 C[t.a]", "{sql}: {rendered:?}");
    }
}

#[test]
fn an_unmodelled_expression_is_unresolved() {
    for (dialect, sql, expected) in [
        (
            Dialect::DuckDb,
            "SELECT list_transform(items, x -> x + 1) AS r FROM t",
            "r#0 P[t.items | Unsupported { what: \"lambda\" }] Expression",
        ),
        (
            Dialect::MySql,
            "SELECT MATCH (title) AGAINST ('x') AS m FROM t",
            "m#0 P[ | Unsupported { what: \"MATCH ... AGAINST\" }] Expression",
        ),
    ] {
        assert_eq!(
            analyze_rendered(sql, dialect, None),
            [expected, "Closed"],
            "{sql}"
        );
    }
}

#[test]
fn values_inside_dictionaries_and_intervals_are_inputs() {
    for (sql, expected) in [
        ("SELECT {'k': a} AS d FROM t", "d#0 C[t.a] Expression"),
        (
            "SELECT INTERVAL (a) DAY AS i FROM t",
            "i#0 C[t.a] Expression",
        ),
    ] {
        assert_eq!(
            analyze_rendered(sql, Dialect::DuckDb, None),
            [expected, "Closed"],
            "{sql}"
        );
    }
}
