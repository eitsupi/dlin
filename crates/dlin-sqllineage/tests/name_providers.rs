//! An unqualified name denotes a column only when exactly one provider can be
//! shown to publish it. Relations of unknown shape may publish any name, so
//! they compete with the relations that list it; a catalog that describes a
//! relation without the name takes it out of the competition. Every case is
//! read directly, through one `SELECT *` wrapper, and through two, and the
//! answer must not change.

mod common;

use common::KnownTables;
use dlin_sqllineage::{
    Analyzer, Candidate, Dialect, Ident, Lineage, Relation, SourceColumn, Unresolved,
};

fn relation(name: &str) -> Relation {
    Relation {
        catalog: None,
        schema: None,
        name: Ident::unquoted(name),
    }
}

fn physical(name: &str) -> Candidate {
    Candidate::Relation(relation(name))
}

fn source(table: &str, column: &str) -> SourceColumn {
    SourceColumn {
        relation: relation(table),
        column: Ident::unquoted(column),
    }
}

/// The three ways to read `id` from `from`: directly, through one star
/// wrapper, and through two.
fn readings(with: &str, from: &str) -> [String; 3] {
    [
        format!("{with}SELECT id FROM {from}"),
        format!("{with}SELECT id FROM (SELECT * FROM {from}) w1"),
        format!("{with}SELECT id FROM (SELECT * FROM (SELECT * FROM {from}) w1) w2"),
    ]
}

fn lineage(sql: &str, catalog: &'static [(&'static str, &'static [&'static str])]) -> Lineage {
    let catalog = KnownTables(catalog);
    let analysis = Analyzer::new(Dialect::Generic)
        .with_catalog(&catalog)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
        .into_single()
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"));
    assert_eq!(analysis.output.columns.len(), 1, "{sql}");
    analysis.output.columns.into_iter().next().unwrap().lineage
}

fn assert_ambiguous(
    with: &str,
    from: &str,
    catalog: &'static [(&'static str, &'static [&'static str])],
    expected: &[Candidate],
) {
    for sql in readings(with, from) {
        let Lineage::Partial { known, unresolved } = lineage(&sql, catalog) else {
            panic!("{sql}: expected an ambiguity");
        };
        assert!(known.is_empty(), "{sql}: {known:?}");
        let [Unresolved::Ambiguous { candidates, .. }] = unresolved.as_slice() else {
            panic!("{sql}: expected one ambiguity, got {unresolved:?}");
        };
        assert_eq!(candidates.len(), expected.len(), "{sql}: {candidates:?}");
        for candidate in expected {
            assert!(
                candidates.contains(candidate),
                "{sql}: missing {candidate:?} in {candidates:?}"
            );
        }
    }
}

fn assert_complete(
    with: &str,
    from: &str,
    catalog: &'static [(&'static str, &'static [&'static str])],
    expected: &[SourceColumn],
) {
    for sql in readings(with, from) {
        let Lineage::Complete(mut sources) = lineage(&sql, catalog) else {
            panic!("{sql}: expected complete lineage");
        };
        sources.sort_by(|left, right| left.relation.name.value.cmp(&right.relation.name.value));
        assert_eq!(sources, expected, "{sql}");
    }
}

/// `a` and `b` list `id` and USING merges it, but `c` is joined outside the
/// join: if `c` has an `id` too, the name is ambiguous. The catalog describes
/// only `a` and `b`, so `c` may have it.
#[test]
fn a_using_key_competes_with_a_relation_of_unknown_shape() {
    assert_ambiguous(
        "",
        "a JOIN b USING (id) CROSS JOIN c",
        &[("a", &["id", "x"]), ("b", &["id", "y"])],
        &[physical("a"), physical("b"), physical("c")],
    );
    assert_ambiguous(
        "",
        "a JOIN b USING (id) CROSS JOIN c",
        &[],
        &[physical("a"), physical("b"), physical("c")],
    );
}

/// The same query once the catalog says `c` has no `id`: the merged key is the
/// only provider.
#[test]
fn a_using_key_is_unique_when_the_catalog_rules_the_other_relation_out() {
    assert_complete(
        "",
        "a JOIN b USING (id) CROSS JOIN c",
        &[("a", &["id", "x"]), ("b", &["id", "y"]), ("c", &["z"])],
        &[source("a", "id"), source("b", "id")],
    );
}

/// A CTE lists `id`; the table beside it is not described, so it may have an
/// `id` as well.
#[test]
fn a_cte_column_competes_with_a_table_of_unknown_shape() {
    assert_ambiguous(
        "WITH c AS (SELECT x AS id FROM s) ",
        "c JOIN t ON TRUE",
        &[],
        &[Candidate::Cte(Ident::unquoted("c")), physical("t")],
    );
    // With `t` described and lacking `id`, the CTE alone provides it.
    assert_complete(
        "WITH c AS (SELECT x AS id FROM s) ",
        "c JOIN t ON TRUE",
        &[("t", &["k"])],
        &[source("s", "x")],
    );
}

/// A derived table over an unknown star competes the same way.
#[test]
fn a_derived_table_column_competes_with_a_table_of_unknown_shape() {
    assert_ambiguous(
        "",
        "(SELECT x AS id FROM s) d JOIN t ON TRUE",
        &[],
        &[
            Candidate::Derived(Some(Ident::unquoted("d"))),
            physical("t"),
        ],
    );
}

/// Two relations of unknown shape may both have the column.
#[test]
fn two_relations_of_unknown_shape_are_both_providers() {
    assert_ambiguous("", "a JOIN b ON TRUE", &[], &[physical("a"), physical("b")]);
    // One of them described without the column leaves the other.
    let readings = readings("", "a JOIN b ON TRUE");
    let catalog: &[(&str, &[&str])] = &[("b", &["k"])];
    assert_eq!(
        lineage(&readings[0], catalog),
        Lineage::Complete(vec![source("a", "id")])
    );
    // Through a star the relation behind it is still unknown, so the name
    // stays a named column behind a star.
    for sql in &readings[1..] {
        assert!(
            matches!(lineage(sql, catalog), Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && matches!(&unresolved[..], [Unresolved::NamedBehindStar { relation, .. }]
                        if *relation == self::relation("a"))),
            "{sql}"
        );
    }
}

/// A column the projection lists and a star that may publish the same name are
/// two providers of that name.
#[test]
fn a_listed_column_and_an_unknown_star_both_publish_the_name() {
    let from = "(SELECT x AS id, * FROM t) d";
    for sql in readings("", from) {
        assert!(
            matches!(lineage(&sql, &[]), Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && matches!(&unresolved[..], [Unresolved::Ambiguous { column, .. }]
                        if column.value == "id")),
            "{sql}"
        );
    }
}

/// The columns of `from` read as `kept`, directly and through one and two
/// `SELECT *` wrappers.
fn kept_readings(with: &str, from: &str) -> [String; 3] {
    [
        format!("{with}SELECT kept FROM {from}"),
        format!("{with}SELECT kept FROM (SELECT * FROM {from}) w1"),
        format!("{with}SELECT kept FROM (SELECT * FROM (SELECT * FROM {from}) w1) w2"),
    ]
}

fn lineage_in(
    sql: &str,
    dialect: Dialect,
    catalog: &'static [(&'static str, &'static [&'static str])],
) -> Lineage {
    let catalog = KnownTables(catalog);
    Analyzer::new(dialect)
        .with_catalog(&catalog)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
        .into_single()
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
        .output
        .columns
        .remove(0)
        .lineage
}

/// A column that REPLACE or RENAME creates over a star of unknown columns is a
/// column of the derived table that holds the star. It is the only provider of
/// its name unless another relation of the clause may publish it too: a
/// relation the catalog describes without the name does not, an undescribed one
/// does. The star hides further, behind a CTE, and the answer is the same.
#[test]
fn a_replaced_or_renamed_column_over_a_cte_star_is_a_provider_of_its_own() {
    const E_LACKS_IT: &[(&str, &[&str])] = &[("e", &["other"])];
    const NO_CATALOG: &[(&str, &[&str])] = &[];
    let with = "WITH c AS (SELECT * FROM t) ";
    for (dialect, modifier, expected) in [
        (
            Dialect::DuckDb,
            "REPLACE (1 AS kept)",
            Lineage::Complete(vec![]),
        ),
        (
            Dialect::Snowflake,
            "RENAME (id AS kept)",
            Lineage::Partial {
                known: vec![],
                unresolved: vec![Unresolved::NamedBehindStar {
                    relation: relation("t"),
                    column: Ident::unquoted("id"),
                }],
            },
        ),
    ] {
        let derived = format!("(SELECT * {modifier} FROM c) AS d");
        // Alone, and beside a relation known not to have the name.
        for (from, catalog) in [
            (derived.clone(), NO_CATALOG),
            (format!("{derived} CROSS JOIN e"), E_LACKS_IT),
        ] {
            for sql in kept_readings(with, &from) {
                assert_eq!(lineage_in(&sql, dialect, catalog), expected, "{sql}");
            }
        }
        // Beside a relation that may have the name too.
        for sql in kept_readings(with, &format!("{derived} CROSS JOIN e")) {
            let Lineage::Partial { known, unresolved } = lineage_in(&sql, dialect, NO_CATALOG)
            else {
                panic!("{sql}: expected an ambiguity");
            };
            assert!(known.is_empty(), "{sql}");
            let [Unresolved::Ambiguous { candidates, .. }] = unresolved.as_slice() else {
                panic!("{sql}: expected one ambiguity, got {unresolved:?}");
            };
            assert_eq!(candidates.len(), 2, "{sql}: {candidates:?}");
            assert!(candidates.contains(&Candidate::Derived(Some(Ident::unquoted("d")))));
            assert!(candidates.contains(&physical("e")));
        }
    }
}

/// Without a modifier the star over unknown columns is still no single
/// provider when another unknown relation competes.
#[test]
fn a_plain_cte_star_beside_an_unknown_relation_stays_ambiguous() {
    for sql in kept_readings(
        "WITH c AS (SELECT * FROM t) ",
        "(SELECT * FROM c) AS d CROSS JOIN e",
    ) {
        assert!(
            matches!(lineage(&sql, &[]), Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && matches!(&unresolved[..], [Unresolved::Ambiguous { candidates, .. }]
                        if candidates.len() == 2)),
            "{sql}"
        );
    }
}
