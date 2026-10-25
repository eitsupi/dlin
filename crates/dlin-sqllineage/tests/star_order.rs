//! Catalog star expansion keeps the projection order of the query.

use dlin_sqllineage::{
    AnalyzeOptions, AnalyzeResult, CatalogProvider, ColumnOrigin, Dialect, TableRef, analyze,
};

struct Catalog;

impl CatalogProvider for Catalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        match table.table.as_str() {
            "t" => Some(vec!["x".into(), "y".into()]),
            "u" => Some(vec!["p".into(), "q".into()]),
            _ => None,
        }
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        None
    }
}

fn run(sql: &str, dialect: Dialect) -> AnalyzeResult {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            catalog: Some(Box::new(Catalog)),
            ..AnalyzeOptions::default()
        },
    )
    .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
    .remove(0)
}

fn targets(result: &AnalyzeResult) -> Vec<&str> {
    result
        .columns
        .mappings
        .iter()
        .map(|mapping| mapping.target.column.as_str())
        .collect()
}

/// `(target, source table, source column)` per mapping, for single-source
/// concrete mappings.
fn concrete(result: &AnalyzeResult) -> Vec<(String, String, String)> {
    result
        .columns
        .mappings
        .iter()
        .map(|mapping| match mapping.sources.as_slice() {
            [ColumnOrigin::Concrete { table, column }] => (
                mapping.target.column.clone(),
                table.table.clone(),
                column.clone(),
            ),
            other => panic!(
                "expected one Concrete for {}, got {other:?}",
                mapping.target.column
            ),
        })
        .collect()
}

fn row(target: &str, table: &str, column: &str) -> (String, String, String) {
    (target.into(), table.into(), column.into())
}

const DIALECTS: [Dialect; 3] = [Dialect::Generic, Dialect::PostgreSql, Dialect::BigQuery];

#[test]
fn star_between_named_columns_keeps_position() {
    for dialect in DIALECTS {
        let result = run("SELECT a, *, b FROM t", dialect);
        assert_eq!(targets(&result), ["a", "x", "y", "b"], "{dialect:?}");
        assert_eq!(
            concrete(&result),
            [
                row("a", "t", "a"),
                row("x", "t", "x"),
                row("y", "t", "y"),
                row("b", "t", "b"),
            ]
        );
    }
}

#[test]
fn qualified_star_keeps_position() {
    for dialect in DIALECTS {
        let result = run("SELECT a, t.*, b FROM t", dialect);
        assert_eq!(targets(&result), ["a", "x", "y", "b"], "{dialect:?}");
        let result = run("SELECT t.*, b FROM t", dialect);
        assert_eq!(targets(&result), ["x", "y", "b"], "{dialect:?}");
        let result = run("SELECT a, t.* FROM t", dialect);
        assert_eq!(targets(&result), ["a", "x", "y"], "{dialect:?}");
    }
}

#[test]
fn several_stars_each_stay_where_they_were_written() {
    for dialect in DIALECTS {
        let result = run(
            "SELECT t.*, t.a, u.*, u.b FROM t JOIN u ON t.x = u.p",
            dialect,
        );
        assert_eq!(
            targets(&result),
            ["x", "y", "a", "p", "q", "b"],
            "{dialect:?}"
        );
        assert_eq!(
            concrete(&result),
            [
                row("x", "t", "x"),
                row("y", "t", "y"),
                row("a", "t", "a"),
                row("p", "u", "p"),
                row("q", "u", "q"),
                row("b", "u", "b"),
            ]
        );
    }
}

#[test]
fn repeated_unqualified_stars_expand_in_place() {
    for dialect in DIALECTS {
        let result = run("SELECT a, *, b, * FROM t", dialect);
        assert_eq!(
            targets(&result),
            ["a", "x", "y", "b", "x", "y"],
            "{dialect:?}"
        );
    }
}

#[test]
fn star_inside_cte_and_derived_table_keeps_position() {
    for dialect in DIALECTS {
        let result = run("WITH c AS (SELECT a, *, b FROM t) SELECT * FROM c", dialect);
        assert_eq!(targets(&result), ["a", "x", "y", "b"], "{dialect:?}");
        let result = run("SELECT * FROM (SELECT a, *, b FROM t) AS d", dialect);
        assert_eq!(targets(&result), ["a", "x", "y", "b"], "{dialect:?}");
    }
}

#[test]
fn star_in_each_set_operation_branch_keeps_position() {
    for dialect in DIALECTS {
        let result = run(
            "SELECT a, *, b FROM t UNION ALL SELECT 1, p, q, 2 FROM u",
            dialect,
        );
        assert_eq!(targets(&result), ["a", "x", "y", "b"], "{dialect:?}");
        assert_eq!(result.columns.mappings.len(), 4);
    }
}
