//! Unqualified names in a FROM clause: which relations may own them, read
//! directly and through a star over the same clause.

use std::borrow::Cow;

use dlin_sqllineage::{
    Analyzer, Candidate, Catalog, CatalogColumn, CatalogEntry, Dialect, Ident, Lineage, Relation,
    Severity, Unresolved,
};

fn relation(name: &str) -> Relation {
    Relation {
        catalog: None,
        schema: None,
        name: Ident::unquoted(name),
    }
}

struct FixtureCatalog;

impl Catalog for FixtureCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        let columns: &[&str] = match relation.name.value.as_str() {
            "a" | "b" | "c_source" | "d_source" => &["id", "value"],
            _ => return None,
        };
        Some(CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(
                columns
                    .iter()
                    .map(|name| CatalogColumn {
                        name: Ident::unquoted(*name),
                        data_type: None,
                        fields: None,
                    })
                    .collect(),
            ),
        })
    }
}

fn output_lineage(sql: &str, dialect: Dialect, catalog: bool) -> Lineage {
    let analyzer = Analyzer::new(dialect);
    let result = if catalog {
        analyzer.with_catalog(&FixtureCatalog).analyze(sql)
    } else {
        analyzer.analyze(sql)
    }
    .unwrap_or_else(|error| panic!("{sql}: {error}"))
    .into_single()
    .unwrap_or_else(|error| panic!("{sql}: {error}"));
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error),
        "{sql}: {:?}",
        result.diagnostics
    );
    assert_eq!(result.output.columns.len(), 1, "{sql}");
    result.output.columns.into_iter().next().unwrap().lineage
}

fn assert_candidates(sql: &str, dialect: Dialect, catalog: bool, expected: &[Candidate]) {
    let actual = output_lineage(sql, dialect, catalog);
    let Lineage::Partial { known, unresolved } = actual else {
        panic!("{sql}: expected partial ambiguity, got {actual:?}");
    };
    assert!(
        known.is_empty(),
        "{sql}: unexpected known sources {known:?}"
    );
    let [Unresolved::Ambiguous { candidates, .. }] = unresolved.as_slice() else {
        panic!("{sql}: no typed ambiguous candidate set in {unresolved:?}");
    };
    assert_eq!(candidates.len(), expected.len(), "{sql}: {candidates:?}");
    for candidate in expected {
        assert!(
            candidates.contains(candidate),
            "{sql}: missing {candidate:?} from {candidates:?}"
        );
    }
}

fn physical(names: &[&str]) -> Vec<Candidate> {
    names
        .iter()
        .map(|name| Candidate::Relation(relation(name)))
        .collect()
}

fn cases_through_star() -> Vec<(String, Vec<Candidate>)> {
    vec![
        (
            "SELECT id FROM (SELECT * FROM a JOIN (SELECT * FROM b) d ON TRUE) w".to_owned(),
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Derived(Some(Ident::unquoted("d"))),
            ],
        ),
        (
            "WITH c AS (SELECT * FROM b) SELECT id FROM (SELECT * FROM a JOIN c ON TRUE) w"
                .to_owned(),
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Cte(Ident::unquoted("c")),
            ],
        ),
    ]
}

/// A derived table or CTE over an unknown star may publish any name, exactly
/// as an unknown physical table may, so it is a possible owner of a name that
/// nothing else lists.
#[test]
fn unknown_derived_and_cte_ranges_are_possible_providers() {
    let mut cases = vec![
        (
            "SELECT id FROM a JOIN (SELECT * FROM b) d ON TRUE",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Derived(Some(Ident::unquoted("d"))),
            ],
        ),
        (
            "WITH c AS (SELECT * FROM b) SELECT id FROM a JOIN c ON TRUE",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Cte(Ident::unquoted("c")),
            ],
        ),
        (
            "SELECT id FROM (SELECT * FROM a) x JOIN (SELECT * FROM b) y ON TRUE",
            vec![
                Candidate::Derived(Some(Ident::unquoted("x"))),
                Candidate::Derived(Some(Ident::unquoted("y"))),
            ],
        ),
    ];
    cases.push((
        "SELECT id FROM a JOIN b USING (id) CROSS JOIN (SELECT * FROM c) d",
        vec![
            Candidate::Relation(relation("a")),
            Candidate::Relation(relation("b")),
            Candidate::Derived(Some(Ident::unquoted("d"))),
        ],
    ));
    for (sql, candidates) in cases {
        assert_candidates(sql, Dialect::Generic, false, &candidates);
    }
    // Read through a star over the same clause, the owners are the same.
    for (sql, candidates) in cases_through_star() {
        assert_candidates(&sql, Dialect::Generic, false, &candidates);
    }
}

/// A column that a CTE lists does not settle the name when an unknown derived
/// table is joined: the derived table may publish the name as well.
#[test]
fn a_listed_column_competes_with_an_unknown_derived_table() {
    let sql = "WITH c AS (SELECT x AS id FROM s) \
               SELECT id FROM c JOIN (SELECT * FROM b) d ON TRUE";
    assert_candidates(
        sql,
        Dialect::Generic,
        false,
        &[
            Candidate::Cte(Ident::unquoted("c")),
            Candidate::Derived(Some(Ident::unquoted("d"))),
        ],
    );
}

/// Two ranges that share a name (after the dialect's case folding) leave a
/// merged name without a clear owner, so every range stays a candidate whether
/// the name is read directly or through a star. The FROM clauses are invalid
/// SQL in these dialects; the analysis must not pick one range.
#[test]
fn duplicate_range_names_keep_every_range_as_a_candidate() {
    let cases = [
        (
            "",
            "a AS t JOIN b AS T USING (id)",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Relation(relation("b")),
            ],
        ),
        (
            "WITH c AS (SELECT id, value FROM c_source) ",
            "a AS t JOIN c AS T USING (id)",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Cte(Ident::unquoted("T")),
            ],
        ),
        (
            "",
            "a AS t JOIN (SELECT id, value FROM d_source) AS T USING (id)",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Derived(Some(Ident::unquoted("T"))),
            ],
        ),
        (
            "",
            "a AS t NATURAL JOIN b AS T",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Relation(relation("b")),
            ],
        ),
        (
            "WITH c AS (SELECT id, value FROM c_source) ",
            "a AS t NATURAL JOIN c AS T",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Cte(Ident::unquoted("T")),
            ],
        ),
        (
            "",
            "a AS t NATURAL JOIN (SELECT id, value FROM d_source) AS T",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Derived(Some(Ident::unquoted("T"))),
            ],
        ),
    ];
    for (with, from, candidates) in cases {
        let direct = format!("{with}SELECT id FROM {from}");
        let through_star = format!("{with}SELECT id FROM (SELECT * FROM {from}) AS boundary");
        for catalog in [false, true] {
            assert_candidates(&direct, Dialect::PostgreSql, catalog, &candidates);
            assert_candidates(&through_star, Dialect::PostgreSql, catalog, &candidates);
        }
    }
}

/// A name the join does not merge is still listed by both relations when the
/// catalog describes them, and the range names are not what tells them apart.
#[test]
fn duplicate_range_names_keep_both_listed_columns() {
    let with = "WITH c AS (SELECT id, value FROM c_source) ";
    for (prefix, from, candidates) in [
        ("", "a AS t JOIN b AS T USING (id)", physical(&["a", "b"])),
        (
            with,
            "a AS t JOIN c AS T USING (id)",
            vec![
                Candidate::Relation(relation("a")),
                Candidate::Cte(Ident::unquoted("T")),
            ],
        ),
    ] {
        let direct = format!("{prefix}SELECT value FROM {from}");
        let through_star = format!("{prefix}SELECT value FROM (SELECT * FROM {from}) AS boundary");
        assert_candidates(&direct, Dialect::PostgreSql, true, &candidates);
        assert_candidates(&through_star, Dialect::PostgreSql, true, &candidates);
    }
}

/// Whether two relations joined with NATURAL share a column is unknown when
/// they list none. A later USING key on the joined pair then comes from one of
/// them, not from both, and a LEFT join leaves the right side out of it.
#[test]
fn unknown_natural_then_using_names_one_of_the_natural_relations() {
    let source = "a NATURAL RIGHT JOIN b LEFT JOIN c USING (id)";
    let expected = physical(&["a", "b"]);
    assert_candidates(
        &format!("SELECT id FROM {source}"),
        Dialect::Generic,
        false,
        &expected,
    );
    assert_candidates(
        &format!("SELECT id FROM (SELECT * FROM {source}) AS natural_boundary"),
        Dialect::Generic,
        false,
        &expected,
    );
    // An inner join keeps the right side as a source of the key as well.
    let inner = "a NATURAL JOIN b JOIN c USING (id)";
    assert_candidates(
        &format!("SELECT id FROM {inner}"),
        Dialect::Generic,
        false,
        &physical(&["a", "b", "c"]),
    );
    assert_candidates(
        &format!("SELECT id FROM (SELECT * FROM {inner}) AS natural_boundary"),
        Dialect::Generic,
        false,
        &physical(&["a", "b", "c"]),
    );
    // Replacing the uncertain value with a literal removes the value
    // uncertainty without removing the column.
    assert_eq!(
        output_lineage(
            &format!("SELECT id FROM (SELECT * REPLACE (1 AS id) FROM {source}) AS replaced"),
            Dialect::Generic,
            false,
        ),
        Lineage::Complete(vec![]),
    );
}

/// A CTE or derived table whose own FROM clause joins two ranges of one name
/// has no single owner for the key it merges. That ambiguity stands in for the
/// one range that reads it: the other relations of the outer clause can own the
/// name too, so they stay candidates, read directly or through a star.
#[test]
fn inherited_duplicate_range_ambiguity_keeps_the_outer_candidates() {
    let inner = "SELECT * FROM a AS x JOIN b AS x USING (id)";
    let all = [
        Candidate::Relation(relation("a")),
        Candidate::Relation(relation("b")),
        Candidate::Relation(relation("c_source")),
    ];
    let cases = [
        ("", format!("({inner}) d JOIN c_source USING (id)")),
        ("", format!("({inner}) d JOIN c_source ON TRUE")),
        ("", format!("({inner}) d, c_source")),
        ("", format!("c_source JOIN ({inner}) d USING (id)")),
        ("", format!("({inner}) d FULL JOIN c_source USING (id)")),
        (
            &format!("WITH d AS ({inner}) ") as &str,
            "d JOIN c_source USING (id)".to_owned(),
        ),
        (
            &format!("WITH d AS ({inner}) ") as &str,
            "d JOIN c_source ON TRUE".to_owned(),
        ),
    ];
    for (with, from) in cases {
        let direct = format!("{with}SELECT id FROM {from}");
        let through_star = format!("{with}SELECT id FROM (SELECT * FROM {from}) AS boundary");
        for catalog in [false, true] {
            assert_candidates(&direct, Dialect::PostgreSql, catalog, &all);
            assert_candidates(&through_star, Dialect::PostgreSql, catalog, &all);
        }
    }
}

/// Only the relations whose value the outer join keeps join the inherited
/// candidates: a LEFT JOIN keeps the duplicated side alone, a RIGHT JOIN keeps
/// the other relation, which then owns the name outright.
#[test]
fn inherited_duplicate_range_ambiguity_follows_the_outer_join_kind() {
    let inner = "SELECT * FROM a AS x JOIN b AS x USING (id)";
    let left = format!("({inner}) d LEFT JOIN c_source USING (id)");
    for sql in [
        format!("SELECT id FROM {left}"),
        format!("SELECT id FROM (SELECT * FROM {left}) AS boundary"),
    ] {
        assert_candidates(&sql, Dialect::PostgreSql, false, &physical(&["a", "b"]));
    }
    let right = format!("({inner}) d RIGHT JOIN c_source USING (id)");
    for sql in [
        format!("SELECT id FROM {right}"),
        format!("SELECT id FROM (SELECT * FROM {right}) AS boundary"),
    ] {
        assert_eq!(
            output_lineage(&sql, Dialect::PostgreSql, false),
            Lineage::Complete(vec![dlin_sqllineage::SourceColumn {
                relation: relation("c_source"),
                column: Ident::unquoted("id"),
            }]),
            "{sql}"
        );
    }
}
