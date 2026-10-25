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
