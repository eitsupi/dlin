//! A table alias with a column list renames the relation's columns by
//! position, and an alias on a parenthesized join makes the join one range
//! variable that hides the relations inside it.

mod common;

use common::{catalog_entry, expect_rendered_with};
use dlin_sqllineage::{CatalogEntry, Dialect, Relation};

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

/// Dialects whose parser accepts a column alias list on a table.
const DIALECTS: [Dialect; 4] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::DuckDb,
    Dialect::Snowflake,
];

fn expect(sql: &str, expected: &[&str]) {
    for dialect in DIALECTS {
        expect_rendered_with(sql, dialect, &Catalog, expected);
    }
}

#[test]
fn table_column_aliases_rename_columns_by_position() {
    expect(
        "SELECT d.p, d.y FROM t AS d(p)",
        &["p#0 C[t.x]", "y#1 C[t.y]", "Closed"],
    );
    expect(
        "SELECT p, y FROM t AS d(p)",
        &["p#0 C[t.x]", "y#1 C[t.y]", "Closed"],
    );
    expect(
        "SELECT * FROM t AS d(p)",
        &["p#0 C[t.x]", "y#1 C[t.y]", "Closed"],
    );
    expect(
        "SELECT d.* FROM t AS d(p)",
        &["p#0 C[t.x]", "y#1 C[t.y]", "Closed"],
    );
    expect(
        "SELECT * FROM t AS d(p, r)",
        &["p#0 C[t.x]", "r#1 C[t.y]", "Closed"],
    );
}

#[test]
fn renamed_and_hidden_names_are_not_read_from_the_relation() {
    // The alias renames `x` away, and the range variable `d` hides `t`.
    expect(
        "SELECT d.x FROM t AS d(p)",
        &["x#0 P[ | Unbound(x)]", "Closed"],
    );
    expect(
        "SELECT x FROM t AS d(p)",
        &["x#0 P[ | Unbound(x)]", "Closed"],
    );
    expect(
        "SELECT t.x FROM t AS d(p)",
        &["x#0 P[ | Unbound(t)]", "Closed"],
    );
}

#[test]
fn each_range_of_one_table_has_its_own_column_aliases() {
    expect(
        "SELECT d.p, e.r, e.y FROM t AS d(p), t AS e(r)",
        &["p#0 C[t.x]", "r#1 C[t.x]", "y#2 C[t.y]", "Closed"],
    );
    expect(
        "SELECT d.r FROM t AS d(p), t AS e(r)",
        &["r#0 P[ | Unbound(r)]", "Closed"],
    );
}

#[test]
fn column_aliases_apply_to_a_cte_reference() {
    expect(
        "WITH c AS (SELECT x, y FROM t) SELECT d.p, d.y FROM c AS d(p)",
        &["p#0 C[t.x]", "y#1 C[t.y]", "Closed"],
    );
}

#[test]
fn column_aliases_without_a_proven_mapping_stay_unresolved() {
    // More aliases than columns.
    expect_rendered_with(
        "SELECT d.p FROM t AS d(p, r, s)",
        Dialect::PostgreSql,
        &Catalog,
        &[
            "p#0 P[ | Unsupported { what: \"column alias count mismatch\" }]",
            "Closed",
        ],
    );
    // Without a schema the first column is not known, so the alias is not
    // read as a column of that name, and the ambiguity says why.
    expect(
        "SELECT d.p FROM missing AS d(p)",
        &[
            "p#0 P[ | Ambiguous(p: missing), Unsupported { what: \"column alias position unknown\" }]",
            "Closed",
        ],
    );
    expect(
        "SELECT * FROM missing AS d(p)",
        &["Open[? after -; ? after -]"],
    );
}

#[test]
fn joins_merge_renamed_columns() {
    expect(
        "SELECT * FROM t AS d(p) JOIN u USING (p)",
        &["p#0 C[t.x, u.p]", "y#1 C[t.y]", "q#2 C[u.q]", "Closed"],
    );
}

#[test]
fn aliased_join_is_one_range_variable() {
    expect(
        "SELECT j.x, j.p FROM (t JOIN u ON true) AS j",
        &["x#0 C[t.x]", "p#1 C[u.p]", "Closed"],
    );
    expect(
        "SELECT * FROM (t JOIN u ON true) AS j",
        &[
            "x#0 C[t.x]",
            "y#1 C[t.y]",
            "p#2 C[u.p]",
            "q#3 C[u.q]",
            "Closed",
        ],
    );
    expect(
        "SELECT x FROM (t JOIN u ON true) AS j",
        &["x#0 C[t.x]", "Closed"],
    );
}

#[test]
fn aliased_join_hides_the_relations_inside_it() {
    expect(
        "SELECT t.x FROM (t JOIN u ON true) AS j",
        &["x#0 P[ | Unbound(t)]", "Closed"],
    );
    expect(
        "SELECT j1.p FROM ((t JOIN u ON true) AS j1 JOIN t AS t3 ON true) AS j2",
        &["p#0 P[ | Unbound(j1)]", "Closed"],
    );
    expect(
        "SELECT j2.p FROM ((t JOIN u ON true) AS j1 JOIN t AS t3 ON true) AS j2",
        &["p#0 C[u.p]", "Closed"],
    );
    // Without an alias the relations stay visible.
    expect(
        "SELECT t.x FROM (t JOIN u ON true)",
        &["x#0 C[t.x]", "Closed"],
    );
}

#[test]
fn aliased_join_applies_column_aliases_after_merging() {
    expect(
        "SELECT * FROM (t JOIN u ON true) AS j(a, b)",
        &[
            "a#0 C[t.x]",
            "b#1 C[t.y]",
            "p#2 C[u.p]",
            "q#3 C[u.q]",
            "Closed",
        ],
    );
    expect(
        "SELECT * FROM (t JOIN t AS t2 USING (x)) AS j(a)",
        &["a#0 C[t.x]", "y#1 C[t.y]", "y#2 C[t.y]", "Closed"],
    );
}

#[test]
fn outer_join_merges_with_an_aliased_join() {
    expect(
        "SELECT * FROM (t JOIN u ON true) AS j NATURAL JOIN u",
        &[
            "p#0 C[u.p]",
            "q#1 C[u.q]",
            "x#2 C[t.x]",
            "y#3 C[t.y]",
            "Closed",
        ],
    );
}

#[test]
fn lateral_item_inside_an_aliased_join_sees_earlier_items() {
    for dialect in [Dialect::PostgreSql, Dialect::DuckDb] {
        expect_rendered_with(
            "SELECT w FROM t, (u CROSS JOIN LATERAL (SELECT t.x AS w) l) AS j",
            dialect,
            &Catalog,
            &["w#0 C[t.x]", "Closed"],
        );
    }
}

#[test]
fn ordinary_derived_table_inside_an_aliased_join_does_not_see_earlier_items() {
    let sql = "SELECT w FROM t, (u CROSS JOIN (SELECT t.x AS w) l) AS j";
    expect_rendered_with(
        sql,
        Dialect::PostgreSql,
        &Catalog,
        &["w#0 P[ | Unbound(t)]", "Closed"],
    );
    // DuckDB lets an ordinary derived table see the items before it.
    expect_rendered_with(sql, Dialect::DuckDb, &Catalog, &["w#0 C[t.x]", "Closed"]);
}
