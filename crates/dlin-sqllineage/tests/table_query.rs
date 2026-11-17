//! `TABLE t` is `SELECT * FROM t`.

mod common;

use common::{KnownTables, analyze_one_with, analyze_rendered, table};
use dlin_sqllineage::Dialect;

const CATALOG: KnownTables = KnownTables(&[("t", &["x", "y"])]);

/// `TABLE t` and `SELECT * FROM t` give the same output and reads.
fn assert_same_as_select_star(table_form: &str, select_form: &str) {
    for catalog in [Some(&CATALOG as &dyn dlin_sqllineage::Catalog), None] {
        let expected = analyze_one_with(select_form, Dialect::PostgreSql, catalog);
        let actual = analyze_one_with(table_form, Dialect::PostgreSql, catalog);
        assert_eq!(actual.output, expected.output, "{table_form}");
        assert_eq!(actual.read, expected.read, "{table_form}");
    }
}

#[test]
fn a_table_query_reads_like_a_star_over_the_relation() {
    for (table_form, select_form) in [
        (
            "SELECT x, y FROM t UNION ALL TABLE t",
            "SELECT x, y FROM t UNION ALL SELECT * FROM t",
        ),
        (
            "SELECT x, y FROM t UNION ALL TABLE u",
            "SELECT x, y FROM t UNION ALL SELECT * FROM u",
        ),
        (
            "INSERT INTO target (a, b) TABLE t",
            "INSERT INTO target (a, b) SELECT * FROM t",
        ),
        (
            "WITH c AS (SELECT x FROM t) SELECT x FROM t UNION ALL TABLE c",
            "WITH c AS (SELECT x FROM t) SELECT x FROM t UNION ALL SELECT * FROM c",
        ),
    ] {
        assert_same_as_select_star(table_form, select_form);
    }
}

#[test]
fn a_table_query_expands_catalog_columns() {
    assert_eq!(
        analyze_rendered(
            "SELECT x, y FROM t UNION ALL TABLE t",
            Dialect::PostgreSql,
            Some(&CATALOG)
        ),
        ["x#0 C[t.x]", "y#1 C[t.y]", "Closed"]
    );
    let analysis = analyze_one_with(
        "INSERT INTO target (a, b) TABLE t",
        Dialect::PostgreSql,
        Some(&CATALOG),
    );
    assert_eq!(
        common::render_output(&analysis.output),
        ["a#0 C[t.x]", "b#1 C[t.y]", "Closed"]
    );
    assert_eq!(analysis.read, vec![table("t")]);
}

/// The parser keeps the name of `TABLE name` without its quoting. Where the
/// quoting would change what the name refers to, neither the relation it reads
/// nor its output can be told, so the statement is refused.
#[test]
fn a_name_whose_quoting_matters_is_refused() {
    for (dialect, sql) in [
        (
            Dialect::PostgreSql,
            "WITH \"T\" AS (SELECT 1 AS x) SELECT x FROM t UNION ALL TABLE \"T\"",
        ),
        (Dialect::Snowflake, "SELECT x FROM t UNION ALL TABLE t"),
        (Dialect::Generic, "SELECT x FROM t UNION ALL TABLE t"),
    ] {
        let analysis = analyze_one_with(sql, dialect, None);
        assert!(analysis.output.columns.is_empty(), "{dialect:?} {sql}");
        assert!(analysis.read.is_empty(), "{dialect:?} {sql}");
        assert_eq!(
            analysis
                .diagnostics
                .iter()
                .map(|diagnostic| &diagnostic.kind)
                .collect::<Vec<_>>(),
            [&dlin_sqllineage::DiagnosticKind::UnsupportedSyntax {
                what: "TABLE with a name whose meaning depends on quoting"
            }],
            "{dialect:?} {sql}"
        );
    }
    // Lower case names are the same quoted or not in PostgreSQL.
    assert_eq!(
        analyze_rendered(
            "SELECT x, y FROM t UNION ALL TABLE t",
            Dialect::PostgreSql,
            Some(&CATALOG)
        ),
        ["x#0 C[t.x]", "y#1 C[t.y]", "Closed"]
    );
}
