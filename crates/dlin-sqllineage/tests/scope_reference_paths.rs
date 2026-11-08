//! References into a CTE or derived table, and stars over relations a catalog
//! describes.
//!
//! A reference into a scope is answered from the scope's published columns. A
//! name the scope does not publish, or a scope that cannot be evaluated because
//! it depends on itself, is reported as unresolved and never as a source-free
//! column. A star over a relation the catalog lists is expanded where it
//! stands, whatever else the projection contains.

mod common;

use common::{catalog_entry, expect_rendered_in, expect_rendered_with};
use dlin_sqllineage::{
    Catalog, CatalogEntry, CatalogUnknownSegment, Dialect, Ident, Quote, Relation, SchemaCoverage,
    SchemaOrder,
};

fn expect(sql: &str, expected: &[&str]) {
    expect_rendered_in(sql, Dialect::Generic, expected);
}

// References into a scope

/// A column the scope publishes is read the same way qualified and unqualified.
#[test]
fn published_columns_resolve_qualified_and_unqualified() {
    for sql in [
        "WITH c AS (SELECT t.a FROM t) SELECT c.a, a FROM c",
        "SELECT d.a, a FROM (SELECT t.a FROM t) d",
    ] {
        expect(sql, &["a#0 C[t.a]", "a#1 C[t.a]", "Closed"]);
    }
}

/// A name the scope does not publish is unbound, qualified or not.
#[test]
fn a_name_the_scope_does_not_publish_is_unbound() {
    for sql in [
        "WITH c AS (SELECT t.a FROM t) SELECT c.zzz FROM c",
        "WITH c AS (SELECT t.a FROM t) SELECT zzz FROM c",
        "SELECT d.zzz FROM (SELECT t.a FROM t) d",
        "SELECT zzz FROM (SELECT t.a FROM t) d",
    ] {
        expect(sql, &["zzz#0 P[ | Unbound(zzz)]", "Closed"]);
    }
}

/// A scope's own name is not a column of it.
#[test]
fn the_name_of_a_scope_is_not_one_of_its_columns() {
    expect(
        "WITH c AS (SELECT t.a FROM t) SELECT c FROM c",
        &["c#0 P[ | Unbound(c)]", "Closed"],
    );
}

/// A field of a published column follows the column, and a field of an
/// unpublished one is unbound.
#[test]
fn field_paths_follow_the_published_column() {
    expect(
        "WITH c AS (SELECT t.a FROM t) SELECT c.a.f FROM c",
        &["f#0 C[t.a]", "Closed"],
    );
    expect(
        "WITH c AS (SELECT t.a FROM t) SELECT c.zzz.f FROM c",
        &["f#0 P[ | Unbound(zzz)]", "Closed"],
    );
}

/// A quoted column is not found by a folded name.
#[test]
fn a_folded_name_does_not_find_a_quoted_column() {
    expect_rendered_in(
        "WITH c AS (SELECT \"id\" FROM t) SELECT id FROM c",
        Dialect::Snowflake,
        &["id#0 P[ | Unbound(id)]", "Closed"],
    );
}

/// A recursive CTE keeps its base sources and marks the recursion. A name it
/// does not publish is unbound.
#[test]
fn a_recursive_scope_reports_the_recursion_and_unbound_names() {
    expect(
        "WITH RECURSIVE r AS (SELECT t.a FROM t UNION ALL SELECT r.a FROM r) SELECT a FROM r",
        &["a#0 P[t.a | Recursive]", "Closed"],
    );
    expect(
        "WITH RECURSIVE r AS (SELECT t.a FROM t UNION ALL SELECT r.a FROM r) \
         SELECT r.a, r.zzz, zzz FROM r",
        &[
            "a#0 P[t.a | Recursive]",
            "zzz#1 P[ | Unbound(zzz)]",
            "zzz#2 P[ | Unbound(zzz)]",
            "Closed",
        ],
    );
    expect(
        "WITH RECURSIVE r AS (SELECT 1 AS n UNION ALL SELECT n + 1 FROM r) SELECT n FROM r",
        &["n#0 P[ | Recursive]", "Closed"],
    );
}

/// Scopes that read each other have no evaluable output. The reference is
/// unresolved, not source-free.
#[test]
fn scopes_that_read_each_other_are_unresolved() {
    expect(
        "WITH RECURSIVE a AS (SELECT * FROM b), b AS (SELECT * FROM a) SELECT x FROM a",
        &["x#0 P[ | Unbound(x)]", "Closed"],
    );
}

// Catalogs

/// A catalog with complete, partial and unordered schemas.
struct Tables;

impl Catalog for Tables {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        match relation.name.value.as_str() {
            "t" => Some(catalog_entry(relation, &["x", "y"])),
            "u" => Some(catalog_entry(relation, &["p", "q"])),
            "w" => Some(catalog_entry(relation, &["p"])),
            "cased" => {
                let mut entry = catalog_entry(relation, &["Id", "Val"]);
                entry.columns.to_mut()[0].name = Ident {
                    value: "Id".to_string(),
                    quote: Some(Quote::Double),
                };
                Some(entry)
            }
            "part" => {
                let mut entry = catalog_entry(relation, &["k", "m"]);
                entry.coverage = SchemaCoverage::Partial;
                entry.unknown_segments = vec![CatalogUnknownSegment {
                    after_ordinal: Some(0),
                }]
                .into();
                Some(entry)
            }
            "loose" => {
                let mut entry = catalog_entry(relation, &["b", "a"]);
                entry.order = SchemaOrder::Unknown;
                for column in entry.columns.to_mut() {
                    column.ordinal = None;
                }
                Some(entry)
            }
            _ => None,
        }
    }
}

fn expect_catalog(sql: &str, expected: &[&str]) {
    expect_catalog_in(sql, Dialect::Generic, expected);
}

fn expect_catalog_in(sql: &str, dialect: Dialect, expected: &[&str]) {
    expect_rendered_with(sql, dialect, &Tables, expected);
}

/// Every star expands where it stands, around columns written before, between
/// and after it.
#[test]
fn stars_over_listed_relations_expand_in_place() {
    expect_catalog(
        "SELECT t.x AS a, *, u.p AS b FROM t, u",
        &[
            "a#0 C[t.x]",
            "x#1 C[t.x]",
            "y#2 C[t.y]",
            "p#3 C[u.p]",
            "q#4 C[u.q]",
            "b#5 C[u.p]",
            "Closed",
        ],
    );
    expect_catalog(
        "SELECT *, t.x AS a, * FROM t",
        &[
            "x#0 C[t.x]",
            "y#1 C[t.y]",
            "a#2 C[t.x]",
            "x#3 C[t.x]",
            "y#4 C[t.y]",
            "Closed",
        ],
    );
    expect_catalog(
        "SELECT t.*, u.*, t.* FROM t, u",
        &[
            "x#0 C[t.x]",
            "y#1 C[t.y]",
            "p#2 C[u.p]",
            "q#3 C[u.q]",
            "x#4 C[t.x]",
            "y#5 C[t.y]",
            "Closed",
        ],
    );
}

/// A star over a relation without a schema stays unexpanded next to the
/// expanded ones. Positions before it are proven and positions after it are
/// not.
#[test]
fn an_unlisted_relation_keeps_its_star_unexpanded_beside_expanded_ones() {
    expect_catalog(
        "SELECT t.x AS first, unknown_rel.*, t.* FROM t, unknown_rel",
        &[
            "first#0 C[t.x]",
            "x#- C[t.x]",
            "y#- C[t.y]",
            "Open[unknown_rel after 0]",
        ],
    );
    expect_catalog(
        "SELECT unknown_rel.*, t.* FROM t, unknown_rel",
        &["x#- C[t.x]", "y#- C[t.y]", "Open[unknown_rel after -]"],
    );
    expect_catalog(
        "SELECT * FROM t, unknown_rel",
        &["x#0 C[t.x]", "y#1 C[t.y]", "Open[unknown_rel after 1]"],
    );
}

/// The same stars, read through a CTE.
#[test]
fn expanded_stars_read_through_a_cte_keep_their_columns() {
    expect_catalog(
        "WITH c AS (SELECT * FROM t, u) SELECT * FROM c",
        &[
            "x#0 C[t.x]",
            "y#1 C[t.y]",
            "p#2 C[u.p]",
            "q#3 C[u.q]",
            "Closed",
        ],
    );
    expect_catalog(
        "WITH c AS (SELECT * FROM t, unknown_rel) SELECT * FROM c",
        &["x#0 C[t.x]", "y#1 C[t.y]", "Open[unknown_rel after 1]"],
    );
    expect_catalog(
        "WITH c AS (SELECT *, 1 AS z FROM t) SELECT z, x FROM c",
        &["z#0 C[]", "x#1 C[t.x]", "Closed"],
    );
}

/// A partial schema expands its listed columns and leaves the rest open.
#[test]
fn a_partial_schema_expands_its_listed_columns_and_stays_open() {
    expect_catalog(
        "SELECT * FROM part",
        &["k#0 C[part.k]", "m#1 C[part.m]", "Open[part after 0]"],
    );
    expect_catalog(
        "SELECT *, m AS z FROM part",
        &[
            "k#0 C[part.k]",
            "m#1 C[part.m]",
            "z#- C[part.m]",
            "Open[part after 0]",
        ],
    );
    expect_catalog(
        "WITH c AS (SELECT * FROM part) SELECT * FROM c",
        &["k#0 C[part.k]", "m#1 C[part.m]", "Open[part after 0]"],
    );
}

/// A complete schema whose order is unknown lists its columns without
/// ordinals, and a column written after the star does not get a guessed one.
#[test]
fn a_schema_without_a_proven_order_expands_without_ordinals() {
    expect_catalog(
        "SELECT * FROM loose",
        &["b#- C[loose.b]", "a#- C[loose.a]", "Closed"],
    );
    expect_catalog(
        "SELECT *, loose.a AS z FROM loose",
        &[
            "b#- C[loose.b]",
            "a#- C[loose.a]",
            "z#2 C[loose.a]",
            "Closed",
        ],
    );
}

/// Expanded columns are spelled as the catalog spells them.
#[test]
fn expanded_columns_use_the_catalog_spelling() {
    expect_catalog(
        "SELECT * FROM cased",
        &["Id#0 C[cased.Id]", "Val#1 C[cased.Val]", "Closed"],
    );
    expect_catalog(
        "SELECT id, \"Id\", val FROM cased",
        &[
            "id#0 C[cased.id]",
            "Id#1 C[cased.Id]",
            "val#2 C[cased.Val]",
            "Closed",
        ],
    );
    expect_catalog_in(
        "SELECT id, \"Id\", val FROM cased",
        Dialect::PostgreSql,
        &[
            "id#0 C[cased.id]",
            "Id#1 C[cased.Id]",
            "val#2 C[cased.Val]",
            "Closed",
        ],
    );
}

/// A name that two relations may supply is ambiguous, and one that the catalog
/// shows only one of them has is not.
#[test]
fn the_catalog_narrows_which_relation_supplies_a_name() {
    expect_catalog(
        "SELECT p FROM u, w",
        &["p#0 P[ | Ambiguous(p: u w)]", "Closed"],
    );
    expect_catalog("SELECT q FROM u, w", &["q#0 C[u.q]", "Closed"]);
    expect_catalog("SELECT x FROM t, u", &["x#0 C[t.x]", "Closed"]);
    expect_catalog(
        "SELECT p FROM u, unknown_rel",
        &["p#0 P[ | Ambiguous(p: u unknown_rel)]", "Closed"],
    );
}

/// Catalog-expanded stars pair by position across a set operation.
#[test]
fn expanded_stars_pair_by_position_across_a_set_operation() {
    expect_catalog(
        "SELECT * FROM t UNION ALL SELECT * FROM u",
        &["x#0 C[t.x, u.p]", "y#1 C[t.y, u.q]", "Closed"],
    );
    expect_catalog(
        "SELECT * FROM (SELECT * FROM t) d UNION ALL SELECT * FROM u",
        &["x#0 C[t.x, u.p]", "y#1 C[t.y, u.q]", "Closed"],
    );
    expect_catalog(
        "SELECT t.x AS a, t.* FROM t UNION ALL SELECT u.p, u.* FROM u",
        &[
            "a#0 C[t.x, u.p]",
            "x#1 C[t.x, u.p]",
            "y#2 C[t.y, u.q]",
            "Closed",
        ],
    );
}
