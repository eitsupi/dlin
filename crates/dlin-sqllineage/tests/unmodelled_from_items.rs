//! FROM items whose output columns are not modelled: table functions,
//! JSON_TABLE and similar constructs, the wrappers that reshape a FROM item
//! (PIVOT, UNPIVOT, MATCH_RECOGNIZE), and VALUES lists.
//!
//! Each still binds its range variable with an output of unknown width. A
//! column read through it is unresolved with the construct as the reason, an
//! unqualified name it may publish is not settled on another relation, and a
//! star over it stays open. Its arguments are read, but are not the value of
//! any output column.

mod common;

use common::{KnownTables, analyze_one_with, render_output};
use dlin_sqllineage::Dialect;

const CATALOG: KnownTables = KnownTables(&[("t", &["x", "y"]), ("u", &["x"])]);

/// The rendered output of `sql` under `CATALOG`, and the relations it reads.
fn analyze(sql: &str, dialect: Dialect) -> (Vec<String>, Vec<String>) {
    let analysis = analyze_one_with(sql, dialect, Some(&CATALOG));
    assert!(
        analysis.diagnostics.is_empty(),
        "{sql}: {:?}",
        analysis.diagnostics
    );
    (
        render_output(&analysis.output),
        analysis
            .read
            .iter()
            .map(|relation| relation.name.value.clone())
            .collect(),
    )
}

fn expect(dialect: Dialect, sql: &str, output: &[&str], read: &[&str]) {
    assert_eq!(
        analyze(sql, dialect),
        (strings(output), strings(read)),
        "{sql}"
    );
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn unsupported(what: &str) -> String {
    format!("Unsupported {{ what: \"{what}\" }}")
}

#[test]
fn a_column_of_an_unmodelled_item_is_unsupported() {
    for (dialect, sql, what) in [
        (
            Dialect::PostgreSql,
            "SELECT j.z FROM t, JSON_TABLE(t.y, '$' COLUMNS (z INT PATH '$.z')) AS j",
            "JSON_TABLE",
        ),
        (
            Dialect::PostgreSql,
            "SELECT j.z FROM t, XMLTABLE('/r' PASSING t.y COLUMNS z INT PATH 'z') AS j",
            "XMLTABLE",
        ),
        (
            Dialect::MsSql,
            "SELECT j.z FROM t CROSS APPLY OPENJSON(t.y) WITH (z INT) AS j",
            "OPENJSON",
        ),
        (
            Dialect::Snowflake,
            "SELECT j.z FROM t, TABLE(FLATTEN(t.y)) j",
            "table function",
        ),
        (
            Dialect::PostgreSql,
            "SELECT j.z FROM t, generate_series(1, 3) AS j",
            "table function",
        ),
        (
            Dialect::PostgreSql,
            "SELECT j.z FROM t, LATERAL generate_series(1, t.x) AS j",
            "table function",
        ),
        (
            Dialect::Snowflake,
            "SELECT j.z FROM t PIVOT (SUM(x) FOR y IN ('a', 'b')) AS j",
            "PIVOT",
        ),
        (
            Dialect::Snowflake,
            "SELECT j.z FROM t UNPIVOT (v FOR k IN (x, y)) AS j",
            "UNPIVOT",
        ),
        (
            Dialect::Snowflake,
            "SELECT j.z FROM t MATCH_RECOGNIZE (PARTITION BY x ORDER BY y PATTERN (a) \
             DEFINE a AS y > 0) AS j",
            "MATCH_RECOGNIZE",
        ),
        (
            Dialect::PostgreSql,
            "SELECT j.z FROM t, (VALUES (1)) AS j",
            "VALUES source",
        ),
    ] {
        // The arguments read `t`, but `t.y` is not the value of `z`.
        expect(
            dialect,
            sql,
            &[&format!("z#0 P[ | {}]", unsupported(what)), "Closed"],
            &["t"],
        );
    }
}

#[test]
fn an_unqualified_name_is_not_settled_on_another_relation() {
    for (dialect, sql, range, what) in [
        (
            Dialect::PostgreSql,
            "SELECT x FROM t, JSON_TABLE(t.y, '$' COLUMNS (z INT PATH '$.z')) AS j",
            "j",
            "JSON_TABLE",
        ),
        (
            Dialect::Generic,
            "SELECT x FROM t, generate_series(1, 3) AS j",
            "j",
            "table function",
        ),
        (
            Dialect::PostgreSql,
            "SELECT x FROM t, (VALUES (1)) AS j",
            "j",
            "VALUES source",
        ),
    ] {
        expect(
            dialect,
            sql,
            &[
                &format!(
                    "x#0 P[ | Ambiguous(x: t derived {range}), {}]",
                    unsupported(what)
                ),
                "Closed",
            ],
            &["t"],
        );
    }
    // A qualified reference to the other relation is unaffected.
    expect(
        Dialect::PostgreSql,
        "SELECT t.x FROM t, generate_series(1, 3) AS j",
        &["x#0 C[t.x]", "Closed"],
        &["t"],
    );
}

#[test]
fn a_star_over_an_unmodelled_item_stays_open() {
    for (dialect, sql) in [
        (
            Dialect::Generic,
            "SELECT * FROM t, generate_series(1, 3) AS g",
        ),
        (
            Dialect::PostgreSql,
            "SELECT * FROM t, LATERAL (VALUES (t.x)) AS g",
        ),
    ] {
        expect(
            dialect,
            sql,
            &["x#0 C[t.x]", "y#1 C[t.y]", "Open[? after 1]"],
            &["t"],
        );
    }
    for (dialect, sql, read) in [
        (
            Dialect::PostgreSql,
            "SELECT * FROM (VALUES (1)) AS j",
            &[][..],
        ),
        (
            Dialect::PostgreSql,
            "SELECT j.* FROM t, LATERAL (VALUES (t.x)) AS j",
            &["t"][..],
        ),
        (
            Dialect::Snowflake,
            "SELECT * FROM TABLE(FLATTEN(t.y))",
            &[][..],
        ),
        (
            Dialect::Snowflake,
            "SELECT * FROM t PIVOT (SUM(x) FOR y IN ('a', 'b')) AS p",
            &["t"][..],
        ),
        (
            Dialect::Snowflake,
            "SELECT * FROM SEMANTIC_VIEW(sv DIMENSIONS c.region METRICS o.total) AS s",
            &["sv"][..],
        ),
    ] {
        expect(dialect, sql, &["Open[? after -]"], read);
    }
}

#[test]
fn a_function_without_an_alias_is_named_after_the_function() {
    // The function name is the range variable, not a relation that is read.
    expect(
        Dialect::PostgreSql,
        "SELECT generate_series.z FROM generate_series(1, 3)",
        &[
            &format!("z#0 P[ | {}]", unsupported("table function")),
            "Closed",
        ],
        &[],
    );
    // An item without any name still takes part in unqualified lookups.
    expect(
        Dialect::Snowflake,
        "SELECT value FROM t, TABLE(FLATTEN(t.y))",
        &[
            &format!("value#0 P[ | {}]", unsupported("table function")),
            "Closed",
        ],
        &["t"],
    );
}

#[test]
fn a_column_alias_list_keeps_the_unmodelled_reason() {
    for (sql, column, what) in [
        (
            "SELECT g.n FROM generate_series(1, 3) AS g(n)",
            "n",
            "table function",
        ),
        (
            "SELECT j.a FROM (VALUES (1, 2)) AS j(a, b)",
            "a",
            "VALUES source",
        ),
    ] {
        let (output, _) = analyze(sql, Dialect::PostgreSql);
        assert!(
            output[0].starts_with(&format!("{column}#0 P[ |"))
                && output[0].contains(&unsupported(what)),
            "{sql}: {output:?}"
        );
    }
}

#[test]
fn relations_read_by_arguments_are_recorded() {
    expect(
        Dialect::PostgreSql,
        "SELECT * FROM generate_series(1, (SELECT max(x) FROM u)) AS g",
        &["Open[? after -]"],
        &["u"],
    );
    expect(
        Dialect::PostgreSql,
        "SELECT * FROM (VALUES ((SELECT max(x) FROM u))) AS v",
        &["Open[? after -]"],
        &["u"],
    );
}

#[test]
fn a_wrapper_hides_the_item_it_reshapes() {
    // `t` is not a range variable after PIVOT; only `p` is.
    let (output, read) = analyze(
        "SELECT t.x FROM t PIVOT (SUM(x) FOR y IN ('a', 'b')) AS p",
        Dialect::Snowflake,
    );
    assert!(!output[0].contains("C["), "{output:?}");
    assert_eq!(read, ["t"]);
}

/// The unsupported reasons of a statement output, in its columns and its
/// open segments.
fn reasons(sql: &str) -> Vec<String> {
    let analysis = analyze_one_with(sql, Dialect::PostgreSql, Some(&CATALOG));
    let mut lineages: Vec<_> = analysis.output.columns.iter().map(|c| &c.lineage).collect();
    if let dlin_sqllineage::Shape::Open { unexpanded } = &analysis.output.shape {
        lineages.extend(unexpanded.iter().map(|star| &star.lineage));
    }
    let mut reasons = Vec::new();
    for lineage in lineages {
        if let dlin_sqllineage::Lineage::Partial { unresolved, .. } = lineage {
            for reason in unresolved {
                if let dlin_sqllineage::Unresolved::Unsupported { what } = reason
                    && !reasons.contains(&what.to_string())
                {
                    reasons.push(what.to_string());
                }
            }
        }
    }
    reasons
}

#[test]
fn a_set_operation_keeps_the_reasons_of_the_operands_that_supply_values() {
    for (sql, expected) in [
        (
            "SELECT t.x FROM t UNION ALL SELECT * FROM generate_series(1, 3) g",
            &["table function"][..],
        ),
        (
            "SELECT * FROM generate_series(1, 3) g UNION ALL SELECT * FROM (VALUES (1)) v",
            &["table function", "VALUES source"][..],
        ),
        (
            "SELECT * FROM (VALUES (1)) v UNION ALL SELECT * FROM generate_series(1, 3) g",
            &["VALUES source", "table function"][..],
        ),
        (
            "SELECT x FROM (SELECT * FROM generate_series(1, 3) g \
             UNION ALL SELECT * FROM (VALUES (1)) v) d, t",
            &["table function", "VALUES source"][..],
        ),
        // The right operand of EXCEPT supplies no values.
        (
            "SELECT t.x FROM t EXCEPT SELECT * FROM generate_series(1, 3) g",
            &[][..],
        ),
        (
            "SELECT x FROM (SELECT * FROM generate_series(1, 3) g \
             EXCEPT SELECT * FROM (VALUES (1)) v) d, t",
            &["table function"][..],
        ),
    ] {
        assert_eq!(reasons(sql), expected, "{sql}");
    }
}
