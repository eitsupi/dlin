//! The analysis result must not depend on per-process hash seeds. Scope
//! bindings used to live in a hash map, so the order of mappings, the order
//! of `Ambiguous` candidates, and even which CTE a shared column resolved to
//! changed from run to run. Every hash map is built with its own random seed,
//! so repeating the analysis inside one process exposes the problem.

use dlin_sqllineage::{AnalyzeOptions, AnalyzeResult, ColumnOrigin, Dialect, TableRef, analyze};

const RUNS: usize = 50;

fn run(sql: &str, dialect: Dialect) -> Vec<AnalyzeResult> {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            ..AnalyzeOptions::default()
        },
    )
    .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
}

/// Analyze `sql` repeatedly and require byte-identical output every time.
fn stable(sql: &str, dialect: Dialect) -> AnalyzeResult {
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

fn tables(names: &[&str]) -> Vec<TableRef> {
    names.iter().map(|name| TableRef::new(*name)).collect()
}

fn wildcard_tables(result: &AnalyzeResult) -> Vec<TableRef> {
    result
        .columns
        .mappings
        .iter()
        .flat_map(|mapping| mapping.sources.iter())
        .map(|source| match source {
            ColumnOrigin::Wildcard { table } => table.clone(),
            other => panic!("expected Wildcard, got {other:?}"),
        })
        .collect()
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
        .columns
        .mappings
        .iter()
        .map(|mapping| mapping.target.column.as_str())
        .collect();
    assert_eq!(targets, ["*", "b"]);
}

#[test]
fn unnest_join_output_is_stable() {
    let result = stable(
        "SELECT x, y FROM t, UNNEST(t.arr) AS u(x), UNNEST(t.arr2) AS v(y)",
        Dialect::DuckDb,
    );
    assert_eq!(result.columns.mappings.len(), 2);
    let result = stable("SELECT * FROM t, UNNEST(t.arr) AS u(x)", Dialect::DuckDb);
    assert_eq!(result.columns.mappings.len(), 2);
}

#[test]
fn ambiguous_candidates_follow_from_clause_order() {
    for dialect in [Dialect::Generic, Dialect::MySql, Dialect::PostgreSql] {
        let result = stable(
            "SELECT x FROM t1 JOIN t2 ON t1.id = t2.id JOIN t3 ON t1.id = t3.id",
            dialect,
        );
        match result.columns.mappings[0].sources.as_slice() {
            [ColumnOrigin::Ambiguous { column, candidates }] => {
                assert_eq!(column, "x");
                assert_eq!(candidates, &tables(&["t1", "t2", "t3"]), "{dialect:?}");
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
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
    match result.columns.mappings[0].sources.as_slice() {
        [ColumnOrigin::Ambiguous { column, candidates }] => {
            assert_eq!(column, "x");
            assert_eq!(candidates, &tables(&["a", "b"]));
        }
        other => panic!("expected Ambiguous between the CTEs, got {other:?}"),
    }
}

#[test]
fn column_shared_by_two_derived_tables_is_ambiguous() {
    let result = stable(
        "SELECT x FROM (SELECT x FROM s1) d1 JOIN (SELECT x FROM s2) d2 ON d1.x = d2.x",
        Dialect::Generic,
    );
    match result.columns.mappings[0].sources.as_slice() {
        [ColumnOrigin::Ambiguous { column, candidates }] => {
            assert_eq!(column, "x");
            assert_eq!(candidates, &tables(&["d1", "d2"]));
        }
        other => panic!("expected Ambiguous between the derived tables, got {other:?}"),
    }
}

/// `USING` merges the joined columns into one, so the unqualified name is
/// legal SQL and not ambiguous. The leftmost relation is reported.
#[test]
fn using_join_column_is_not_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a JOIN b USING (x)",
        Dialect::Generic,
    );
    match result.columns.mappings[0].sources.as_slice() {
        [ColumnOrigin::Concrete { table, column }] => {
            assert_eq!(table, &TableRef::new("s1"));
            assert_eq!(column, "x");
        }
        other => panic!("expected the leftmost USING source, got {other:?}"),
    }
}

#[test]
fn natural_join_column_is_not_ambiguous() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a NATURAL JOIN b",
        Dialect::Generic,
    );
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Concrete { .. }]
    ));
}

/// A column that only one CTE publishes still resolves through that CTE, even
/// when a physical table of unknown shape is joined as well.
#[test]
fn single_cte_provider_still_wins_over_tables() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1) SELECT x FROM a JOIN t2 ON a.id = t2.id",
        Dialect::Generic,
    );
    match result.columns.mappings[0].sources.as_slice() {
        [ColumnOrigin::Concrete { table, column }] => {
            assert_eq!(table, &TableRef::new("s1"));
            assert_eq!(column, "x");
        }
        other => panic!("expected Concrete s1.x, got {other:?}"),
    }
}

/// Joining one CTE twice does not create a second provider.
#[test]
fn self_join_of_one_cte_resolves_through_it() {
    let result = stable(
        "WITH a AS (SELECT x FROM s1) SELECT x FROM a AS a1 JOIN a AS a2 ON a1.x = a2.x",
        Dialect::Generic,
    );
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Concrete { .. }]
    ));
}
