//! Where the outputs of a projection sit when stars follow listed columns.
//!
//! An unexpanded star reports the ordinal it follows, and a listed column
//! after it reports no ordinal. Both are derived from how many columns precede
//! the star, so each case reads the same stars behind a prefix, through a
//! wrapper and through a set operation.

mod common;

use std::borrow::Cow;

use common::{analyze_rendered, expect_rendered_in};
use dlin_sqllineage::{
    Catalog, CatalogColumn, CatalogEntry, CatalogUnknownSegment, Dialect, EvidenceRef, Ident,
    Relation, SchemaCoverage, SchemaOrder,
};

fn expect(sql: &str, expected: &[&str]) {
    expect_rendered_in(sql, Dialect::Generic, expected);
}

/// `p1` lists `id` at position 0 and has unknown columns after it, `p2` lists
/// `a` and `b` the same way, and `t1` and `t2` list `a` and `b` exhaustively.
struct Schemas;

impl Catalog for Schemas {
    fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
        let (columns, coverage, segments): (&[&str], _, &[Option<usize>]) =
            match query.name.value.as_str() {
                "p1" => (&["id"], SchemaCoverage::Partial, &[Some(0)]),
                "p2" => (&["a", "b"], SchemaCoverage::Partial, &[Some(1)]),
                "t1" | "t2" => (&["a", "b"], SchemaCoverage::Exhaustive, &[]),
                _ => return None,
            };
        Some(CatalogEntry {
            relation: query.clone(),
            columns: Cow::Owned(
                columns
                    .iter()
                    .enumerate()
                    .map(|(ordinal, name)| CatalogColumn {
                        name: Ident::unquoted(*name),
                        ordinal: Some(ordinal),
                        data_type: None,
                        fields: None,
                    })
                    .collect(),
            ),
            coverage,
            order: SchemaOrder::Proven,
            unknown_segments: Cow::Owned(
                segments
                    .iter()
                    .map(|after| CatalogUnknownSegment {
                        after_ordinal: *after,
                    })
                    .collect(),
            ),
            evidence: Cow::Owned(vec![EvidenceRef(query.name.value.clone())]),
        })
    }
}

fn expect_known(sql: &str, expected: &[&str]) {
    assert_eq!(
        analyze_rendered(sql, Dialect::Generic, Some(&Schemas)),
        expected,
        "{sql}"
    );
}

/// Plain stars after listed columns.
#[test]
fn stars_after_a_prefix_report_the_prefix_width() {
    expect("SELECT 1 AS p, * FROM u", &["p#0 C[]", "Open[u after 0]"]);
    expect(
        "SELECT 1 AS p, 2 AS q, u.*, 3 AS r, v.* FROM u, v",
        &[
            "p#0 C[]",
            "q#1 C[]",
            "r#- C[]",
            "Open[u after 1; v after -]",
        ],
    );
}

/// Positions the catalog proves keep counting from the prefix.
#[test]
fn proven_positions_follow_the_prefix() {
    expect_known(
        "SELECT 1 AS p, * FROM p1",
        &["p#0 C[]", "id#1 C[p1.id]", "Open[p1 after 1]"],
    );
    expect_known(
        "SELECT 1 AS p, 2 AS q, * FROM p2",
        &[
            "p#0 C[]",
            "q#1 C[]",
            "a#2 C[p2.a]",
            "b#3 C[p2.b]",
            "Open[p2 after 3]",
        ],
    );
}

/// The columns a set operation adds beyond its unexpanded left operand sit
/// behind a star that follows a prefix.
#[test]
fn set_operation_tails_follow_the_prefix_of_the_star_that_reads_them() {
    let union = "SELECT * FROM u UNION ALL SELECT a, b FROM v";
    let behind_u = [
        "p#0 C[]",
        "a#- P[v.a | BehindStar(u)]",
        "b#- P[v.b | BehindStar(u)]",
        "Open[u after 0]",
    ];
    expect(&format!("SELECT 1 AS p, s.* FROM ({union}) s"), &behind_u);
    expect(
        &format!("SELECT * FROM (SELECT 1 AS p, s.* FROM ({union}) s) o"),
        &behind_u,
    );
    expect(
        &format!("WITH c AS ({union}) SELECT 1 AS p, * FROM c"),
        &behind_u,
    );
    expect(
        &format!("SELECT 1 AS p, s.*, 2 AS q FROM ({union}) s"),
        &[
            "p#0 C[]",
            "a#- P[v.a | BehindStar(u)]",
            "b#- P[v.b | BehindStar(u)]",
            "q#- C[]",
            "Open[u after 0]",
        ],
    );
    let behind_u_after_two = [
        "z#0 C[]",
        "p#1 C[]",
        "a#- P[v.a | BehindStar(u)]",
        "b#- P[v.b | BehindStar(u)]",
        "Open[u after 1]",
    ];
    expect(
        &format!("SELECT 0 AS z, o.* FROM (SELECT 1 AS p, s.* FROM ({union}) s) o"),
        &behind_u_after_two,
    );
    expect(
        &format!(
            "WITH c AS ({union}), d AS (SELECT 1 AS p, c.* FROM c) \
             SELECT 0 AS z, d.* FROM d"
        ),
        &behind_u_after_two,
    );
    expect(
        "SELECT 1 AS p, s.* FROM (SELECT * FROM u UNION ALL SELECT * FROM v) s",
        &[
            "p#0 C[]",
            "Open[u after 0; v after 0; v after 0; u after 0]",
        ],
    );
}

/// With the left operand's schema known, the added columns are aligned and
/// take their positions from the prefix.
#[test]
fn aligned_set_operation_columns_follow_the_prefix() {
    let union = "SELECT * FROM t1 UNION ALL SELECT a, b FROM t2";
    let aligned = [
        "p#0 C[]",
        "a#1 C[t1.a, t2.a]",
        "b#2 C[t1.b, t2.b]",
        "Closed",
    ];
    expect_known(&format!("SELECT 1 AS p, s.* FROM ({union}) s"), &aligned);
    expect_known(
        &format!("SELECT * FROM (SELECT 1 AS p, s.* FROM ({union}) s) o"),
        &aligned,
    );
    expect_known(
        &format!("SELECT 0 AS z, o.* FROM (SELECT 1 AS p, s.* FROM ({union}) s) o"),
        &[
            "z#0 C[]",
            "p#1 C[]",
            "a#2 C[t1.a, t2.a]",
            "b#3 C[t1.b, t2.b]",
            "Closed",
        ],
    );
}

/// An interval the catalog places after a position moves with the prefix, for
/// the operand that lists it and for the one that is merged in.
#[test]
fn unknown_intervals_of_set_operations_move_with_the_prefix() {
    expect_known(
        "SELECT 1 AS p, s.* FROM (SELECT * FROM p1 UNION ALL SELECT id FROM u) s",
        &["p#0 C[]", "id#1 C[p1.id, u.id]", "Open[p1 after 1]"],
    );
    expect_known(
        "SELECT 1 AS p, s.* FROM (SELECT * FROM p2 UNION ALL SELECT a, b FROM u) s",
        &[
            "p#0 C[]",
            "a#1 C[p2.a, u.a]",
            "b#2 C[p2.b, u.b]",
            "Open[p2 after 2]",
        ],
    );
    expect_known(
        "SELECT 0 AS z, o.* FROM (SELECT 1 AS p, s.* FROM \
         (SELECT * FROM p2 UNION ALL SELECT a, b FROM u) s) o",
        &[
            "z#0 C[]",
            "p#1 C[]",
            "a#2 C[p2.a, u.a]",
            "b#3 C[p2.b, u.b]",
            "Open[p2 after 3]",
        ],
    );
    expect_known(
        "SELECT 1 AS p, s.* FROM (SELECT 1 AS k, * FROM p2 UNION ALL SELECT 2, a, b FROM u) s",
        &[
            "p#0 C[]",
            "k#1 C[]",
            "a#2 C[p2.a, u.a]",
            "b#3 C[p2.b, u.b]",
            "Open[p2 after 3]",
        ],
    );
    expect_known(
        "SELECT 1 AS p, s.* FROM (SELECT a, b FROM u UNION ALL SELECT * FROM p2) s",
        &[
            "p#0 C[]",
            "a#1 C[p2.a, u.a]",
            "b#2 C[p2.b, u.b]",
            "Open[p2 after 2]",
        ],
    );
}
