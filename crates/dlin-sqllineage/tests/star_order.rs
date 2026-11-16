//! Catalog star expansion keeps the projection order of the query.

mod common;

use common::{analyze_one_with, catalog_entry, output_name};
use dlin_sqllineage::{CatalogEntry, Dialect, Lineage, Relation, StatementAnalysis};

struct Catalog;

impl dlin_sqllineage::Catalog for Catalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        match relation.name.value.as_str() {
            "t" => Some(catalog_entry(relation, &["x", "y"])),
            "u" => Some(catalog_entry(relation, &["p", "q"])),
            _ => None,
        }
    }
}

fn run(sql: &str, dialect: Dialect) -> StatementAnalysis {
    analyze_one_with(sql, dialect, Some(&Catalog))
}

fn targets(result: &StatementAnalysis) -> Vec<&str> {
    result
        .output
        .columns
        .iter()
        .map(|mapping| output_name(mapping).unwrap())
        .collect()
}

/// `(target, source table, source column)` per mapping, for single-source
/// concrete mappings.
fn concrete(result: &StatementAnalysis) -> Vec<(String, String, String)> {
    result
        .output
        .columns
        .iter()
        .map(|mapping| match &mapping.lineage {
            Lineage::Complete(sources) if sources.len() == 1 => {
                let source = &sources[0];
                (
                    output_name(mapping).unwrap().to_string(),
                    source.relation.name.value.clone(),
                    source.column.value.clone(),
                )
            }
            other => panic!(
                "expected one complete source for {:?}, got {other:?}",
                mapping.name
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
        assert_eq!(result.output.columns.len(), 4);
    }
}

/// Dialects whose parser accepts a derived table without a range variable.
const ANONYMOUS_DERIVED_DIALECTS: [Dialect; 3] =
    [Dialect::Generic, Dialect::PostgreSql, Dialect::DuckDb];

fn ordinals(result: &StatementAnalysis) -> Vec<Option<usize>> {
    result
        .output
        .columns
        .iter()
        .map(|mapping| mapping.ordinal)
        .collect()
}

#[test]
fn derived_table_without_alias_expands_where_it_is_written() {
    for dialect in ANONYMOUS_DERIVED_DIALECTS {
        let result = run("SELECT * FROM (SELECT a AS z FROM t), u", dialect);
        assert_eq!(targets(&result), ["z", "p", "q"], "{dialect:?}");
        assert_eq!(
            concrete(&result),
            [row("z", "t", "a"), row("p", "u", "p"), row("q", "u", "q")],
            "{dialect:?}"
        );
        assert_eq!(
            ordinals(&result),
            [Some(0), Some(1), Some(2)],
            "{dialect:?}"
        );

        let result = run("SELECT * FROM u, (SELECT a AS z FROM t), t", dialect);
        assert_eq!(targets(&result), ["p", "q", "z", "x", "y"], "{dialect:?}");
        assert_eq!(
            concrete(&result),
            [
                row("p", "u", "p"),
                row("q", "u", "q"),
                row("z", "t", "a"),
                row("x", "t", "x"),
                row("y", "t", "y"),
            ],
            "{dialect:?}"
        );
        assert_eq!(
            ordinals(&result),
            [Some(0), Some(1), Some(2), Some(3), Some(4)],
            "{dialect:?}"
        );
    }
}

#[test]
fn derived_tables_without_alias_keep_their_order_around_joins() {
    for dialect in ANONYMOUS_DERIVED_DIALECTS {
        let result = run(
            "SELECT * FROM (SELECT a AS z FROM t) JOIN u ON true, (SELECT b AS w FROM t)",
            dialect,
        );
        assert_eq!(targets(&result), ["z", "p", "q", "w"], "{dialect:?}");
        assert_eq!(
            concrete(&result),
            [
                row("z", "t", "a"),
                row("p", "u", "p"),
                row("q", "u", "q"),
                row("w", "t", "b"),
            ],
            "{dialect:?}"
        );
    }
}

#[test]
fn positional_alias_over_derived_table_without_alias_follows_from_order() {
    for dialect in [Dialect::PostgreSql, Dialect::DuckDb] {
        let result = run(
            "WITH c(m, n, o) AS (SELECT * FROM (SELECT a AS z FROM t), u) SELECT m, n, o FROM c",
            dialect,
        );
        assert_eq!(
            concrete(&result),
            [row("m", "t", "a"), row("n", "u", "p"), row("o", "u", "q")],
            "{dialect:?}"
        );
    }
}

#[test]
fn join_merges_columns_of_derived_table_without_alias() {
    for dialect in [Dialect::PostgreSql, Dialect::DuckDb] {
        for (sql, expected) in [
            (
                "SELECT * FROM (SELECT a AS p FROM t) JOIN u USING (p)",
                &["p#0 C[t.a, u.p]", "q#1 C[u.q]", "Closed"][..],
            ),
            (
                "SELECT * FROM u JOIN (SELECT a AS p FROM t) USING (p)",
                &["p#0 C[t.a, u.p]", "q#1 C[u.q]", "Closed"],
            ),
            (
                "SELECT * FROM (SELECT a AS p FROM t) NATURAL JOIN u",
                &["p#0 C[t.a, u.p]", "q#1 C[u.q]", "Closed"],
            ),
            (
                "SELECT * FROM (SELECT a AS p FROM t) LEFT JOIN u USING (p)",
                &["p#0 C[t.a]", "q#1 C[u.q]", "Closed"],
            ),
            (
                "SELECT * FROM (SELECT a AS p FROM t) JOIN (SELECT b AS p FROM t) USING (p)",
                &["p#0 C[t.a, t.b]", "Closed"],
            ),
            (
                "SELECT p FROM (SELECT a AS p FROM t) JOIN u USING (p)",
                &["p#0 C[t.a, u.p]", "Closed"],
            ),
        ] {
            common::expect_rendered_with(sql, dialect, &Catalog, expected);
        }
    }
}

#[test]
fn lateral_derived_table_sees_only_derived_tables_without_alias_before_it() {
    for dialect in [Dialect::PostgreSql, Dialect::DuckDb] {
        for sql in [
            "SELECT q FROM (SELECT a AS p FROM t) CROSS JOIN LATERAL (SELECT p + 1 AS q) NATURAL JOIN (SELECT b AS p FROM u)",
            "SELECT q FROM (SELECT a AS p FROM t) CROSS JOIN LATERAL (SELECT p + 1 AS q) CROSS JOIN (SELECT b AS p FROM u)",
            "SELECT q FROM (SELECT a AS p FROM t), LATERAL (SELECT p + 1 AS q), (SELECT b AS p FROM u)",
        ] {
            common::expect_rendered_with(
                sql,
                dialect,
                &Catalog,
                &["q#0 C[t.a] Expression", "Closed"],
            );
        }
    }
}

#[test]
fn duckdb_implicit_lateral_sees_only_derived_tables_without_alias_before_it() {
    common::expect_rendered_with(
        "SELECT q FROM (SELECT a AS p FROM t), (SELECT p + 1 AS q), (SELECT b AS p FROM u)",
        Dialect::DuckDb,
        &Catalog,
        &["q#0 C[t.a] Expression", "Closed"],
    );
}
