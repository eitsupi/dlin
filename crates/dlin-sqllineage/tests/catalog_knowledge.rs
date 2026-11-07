//! Public schema contract cases. Expectations follow SQL semantics.
use std::borrow::Cow;

use dlin_sqllineage::*;

struct Schemas(Vec<(Relation, Vec<CatalogColumn>, SchemaCoverage, SchemaOrder)>);

fn relation(name: &str) -> Relation {
    Relation {
        catalog: None,
        schema: None,
        name: Ident::unquoted(name),
    }
}

fn source(table: &str, column: &str) -> SourceColumn {
    SourceColumn {
        relation: relation(table),
        column: Ident::unquoted(column),
    }
}

impl Schemas {
    fn new(entries: &[(&str, &[&str], SchemaCoverage, SchemaOrder)]) -> Self {
        Self(
            entries
                .iter()
                .map(|(name, columns, coverage, order)| {
                    (
                        relation(name),
                        columns
                            .iter()
                            .enumerate()
                            .map(|(index, name)| CatalogColumn {
                                name: Ident::unquoted(*name),
                                ordinal: (*order == SchemaOrder::Proven).then_some(index),
                                data_type: None,
                                fields: None,
                            })
                            .collect(),
                        *coverage,
                        *order,
                    )
                })
                .collect(),
        )
    }
}

impl Catalog for Schemas {
    fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
        self.0
            .iter()
            .find(|(name, ..)| name == query)
            .map(|(name, columns, coverage, order)| CatalogEntry {
                relation: name.clone(),
                columns: Cow::Borrowed(columns),
                coverage: *coverage,
                order: *order,
                unknown_segments: Cow::Borrowed(&[]),
                evidence: Cow::Owned(vec![EvidenceRef(format!("schema:{}", name.name.value))]),
            })
    }
}

fn analyze(sql: &str, catalog: &dyn Catalog) -> StatementAnalysis {
    let statement = Analyzer::new(Dialect::PostgreSql)
        .with_catalog(catalog)
        .analyze(sql)
        .unwrap()
        .into_single()
        .unwrap();
    assert!(
        statement
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Error),
        "{sql}: {:?}",
        statement.diagnostics
    );
    statement
}

#[test]
fn p1_partial_schema_proves_a_listed_explicit_column() {
    let catalog = Schemas::new(&[("t", &["x"], SchemaCoverage::Partial, SchemaOrder::Unknown)]);
    let statement = analyze("SELECT t.x FROM t", &catalog);
    assert_eq!(statement.output.shape, Shape::Closed);
    assert_eq!(statement.output.columns[0].ordinal, Some(0));
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "x")])
    );
}

#[test]
fn p2_partial_star_keeps_the_listed_column_and_unknown_remainder() {
    let catalog = Schemas::new(&[("t", &["x"], SchemaCoverage::Partial, SchemaOrder::Unknown)]);
    let statement = analyze("SELECT * FROM t", &catalog);
    assert!(
        matches!(&statement.output.shape, Shape::Open { unexpanded } if unexpanded.len() == 1 && unexpanded[0].relation == Some(relation("t")))
    );
    assert_eq!(statement.output.columns.len(), 1);
    assert_eq!(statement.output.columns[0].ordinal, None);
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "x")])
    );
    // The unknown remainder cannot compete with the same provider's listed x.
    let forwarded = analyze("SELECT x FROM (SELECT * FROM t) d", &catalog);
    assert_eq!(
        forwarded.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "x")])
    );
}

#[test]
fn p3_partial_nonmembership_cannot_disambiguate_another_provider() {
    let catalog = Schemas::new(&[
        ("a", &["x"], SchemaCoverage::Exhaustive, SchemaOrder::Proven),
        ("b", &["y"], SchemaCoverage::Partial, SchemaOrder::Unknown),
    ]);
    for sql in [
        "SELECT x FROM a CROSS JOIN b",
        "SELECT x FROM (SELECT * FROM a CROSS JOIN b) d",
    ] {
        let statement = analyze(sql, &catalog);
        assert_eq!(
            statement.output.columns[0].lineage,
            Lineage::Partial {
                known: vec![],
                unresolved: vec![Unresolved::Ambiguous {
                    column: Ident::unquoted("x"),
                    candidates: vec![
                        Candidate::Relation(relation("a")),
                        Candidate::Relation(relation("b"))
                    ]
                }],
            },
            "{sql}"
        );
    }
}

#[test]
fn partial_inner_scope_competes_with_an_outer_listed_column() {
    // P3 also applies at a correlated reference site: inner b may carry x.
    let catalog = Schemas::new(&[
        ("a", &["x"], SchemaCoverage::Exhaustive, SchemaOrder::Proven),
        ("b", &["y"], SchemaCoverage::Partial, SchemaOrder::Unknown),
    ]);
    let statement = analyze("SELECT (SELECT x FROM b) AS v FROM a", &catalog);
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::Ambiguous {
                column: Ident::unquoted("x"),
                candidates: vec![
                    Candidate::Relation(relation("a")),
                    Candidate::Relation(relation("b"))
                ]
            }],
        }
    );
}

#[test]
fn p4_partial_natural_join_retains_schema_dependent_contributions() {
    let catalog = Schemas::new(&[
        ("a", &["x"], SchemaCoverage::Exhaustive, SchemaOrder::Proven),
        ("b", &["y"], SchemaCoverage::Partial, SchemaOrder::Unknown),
    ]);
    let statement = analyze("SELECT * FROM a NATURAL JOIN b", &catalog);
    assert!(matches!(statement.output.shape, Shape::Open { .. }));
    let x = statement
        .output
        .columns
        .iter()
        .find(|column| column.name == OutputName::Explicit(Ident::unquoted("x")))
        .unwrap();
    assert!(
        matches!(&x.lineage, Lineage::Partial { unresolved, .. } if unresolved.iter().any(|u| matches!(u, Unresolved::SchemaDependent { what: SchemaDependencyKind::NaturalJoinColumns, .. })))
    );
}

#[test]
fn partial_using_keys_keep_only_the_join_kinds_contributing_sides() {
    let catalog = Schemas::new(&[
        ("a", &["x"], SchemaCoverage::Exhaustive, SchemaOrder::Proven),
        ("b", &["y"], SchemaCoverage::Partial, SchemaOrder::Unknown),
    ]);
    for (join, sources) in [
        ("JOIN", vec![source("a", "x"), source("b", "x")]),
        ("LEFT JOIN", vec![source("a", "x")]),
        ("RIGHT JOIN", vec![source("b", "x")]),
        ("FULL JOIN", vec![source("a", "x"), source("b", "x")]),
    ] {
        for sql in [
            format!("SELECT x FROM a {join} b USING (x)"),
            format!("SELECT x FROM (SELECT * FROM a {join} b USING (x)) d"),
        ] {
            let statement = analyze(&sql, &catalog);
            assert_eq!(
                statement.output.columns[0].lineage,
                Lineage::Complete(sources.clone()),
                "{sql}"
            );
        }
    }
}

#[test]
fn exhaustive_unknown_order_does_not_open_or_order_the_star() {
    // E1 at the engine boundary: exhaustive names are not proven positions.
    let catalog = Schemas::new(&[(
        "t",
        &["z", "a"],
        SchemaCoverage::Exhaustive,
        SchemaOrder::Unknown,
    )]);
    let statement = analyze("SELECT * FROM t", &catalog);
    assert_eq!(statement.output.shape, Shape::Closed);
    assert_eq!(
        statement
            .output
            .columns
            .iter()
            .map(|column| column.ordinal)
            .collect::<Vec<_>>(),
        vec![None, None]
    );
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "z")])
    );
    assert_eq!(
        statement.output.columns[1].lineage,
        Lineage::Complete(vec![source("t", "a")])
    );
}

#[test]
fn unknown_order_cannot_supply_a_guessed_positional_source() {
    let catalog = Schemas::new(&[(
        "t",
        &["z", "a"],
        SchemaCoverage::Exhaustive,
        SchemaOrder::Unknown,
    )]);
    let statement = analyze("SELECT p, q FROM s UNION ALL SELECT * FROM t", &catalog);
    assert_eq!(statement.output.shape, Shape::Closed);
    for (index, name) in ["p", "q"].iter().enumerate() {
        assert_eq!(
            statement.output.columns[index].lineage,
            Lineage::Partial {
                known: vec![source("s", name)],
                unresolved: vec![Unresolved::AlignmentUnknown],
            }
        );
        assert_eq!(statement.output.columns[index].ordinal, Some(index));
    }
}

#[test]
fn explicit_prefix_survives_an_unknown_order_star_and_a_wrapper() {
    let catalog = Schemas::new(&[(
        "t",
        &["z", "a"],
        SchemaCoverage::Exhaustive,
        SchemaOrder::Unknown,
    )]);
    let statement = analyze(
        "SELECT * FROM (SELECT t.z AS k, t.* FROM t) d UNION ALL SELECT p, q, r FROM u",
        &catalog,
    );
    assert_eq!(statement.output.shape, Shape::Closed);
    assert_eq!(statement.output.columns[0].ordinal, Some(0));
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "z"), source("u", "p")])
    );
    for column in &statement.output.columns[1..] {
        assert_eq!(column.ordinal, None);
        assert!(
            matches!(&column.lineage, Lineage::Partial { known, unresolved } if known.iter().all(|s| s.relation == relation("t")) && unresolved == &[Unresolved::AlignmentUnknown])
        );
    }
}

#[test]
fn p2_partial_catalog_can_preserve_a_proven_prefix_across_a_model_boundary() {
    struct Prefix;
    impl Catalog for Prefix {
        fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
            (query == &relation("t")).then(|| CatalogEntry {
                relation: query.clone(),
                columns: Cow::Owned(vec![CatalogColumn {
                    name: Ident::unquoted("id"),
                    ordinal: Some(0),
                    data_type: None,
                    fields: None,
                }]),
                coverage: SchemaCoverage::Partial,
                order: SchemaOrder::Unknown,
                unknown_segments: Cow::Owned(vec![CatalogUnknownSegment {
                    after_ordinal: Some(0),
                }]),
                evidence: Cow::Borrowed(&[]),
            })
        }
    }
    let statement = analyze(
        "SELECT * FROM (SELECT * FROM t) d UNION ALL SELECT p, q FROM u",
        &Prefix,
    );
    assert!(matches!(statement.output.shape, Shape::Open { .. }));
    assert_eq!(statement.output.columns[0].ordinal, Some(0));
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "id"), source("u", "p")])
    );
}

#[test]
fn unknown_order_keeps_the_known_width_and_the_following_projection_position() {
    // E1: exhaustive coverage proves width even though interior order is unknown.
    let catalog = Schemas::new(&[(
        "t",
        &["z", "a"],
        SchemaCoverage::Exhaustive,
        SchemaOrder::Unknown,
    )]);
    for sql in [
        "SELECT 1 AS prefix, t.*, 2 AS suffix FROM t",
        "SELECT * FROM (SELECT 1 AS prefix, t.*, 2 AS suffix FROM t) d",
    ] {
        let statement = analyze(sql, &catalog);
        assert_eq!(statement.output.shape, Shape::Closed);
        assert_eq!(
            statement
                .output
                .columns
                .iter()
                .map(|column| column.ordinal)
                .collect::<Vec<_>>(),
            vec![Some(0), None, None, Some(3)],
            "{sql}"
        );
    }
}

#[test]
fn p4_natural_outer_join_does_not_assert_a_conditionally_suppressed_source() {
    let catalog = Schemas::new(&[
        ("a", &["x"], SchemaCoverage::Exhaustive, SchemaOrder::Proven),
        ("b", &["y"], SchemaCoverage::Partial, SchemaOrder::Unknown),
    ]);
    for (join, expected) in [
        ("LEFT", Lineage::Complete(vec![source("a", "x")])),
        (
            "RIGHT",
            Lineage::Partial {
                known: vec![],
                unresolved: vec![Unresolved::SchemaDependent {
                    what: SchemaDependencyKind::NaturalJoinColumns,
                    relations: vec![relation("b")],
                }],
            },
        ),
    ] {
        for sql in [
            format!("SELECT x FROM a NATURAL {join} JOIN b"),
            format!("SELECT x FROM (SELECT * FROM a NATURAL {join} JOIN b) d"),
        ] {
            assert_eq!(
                analyze(&sql, &catalog).output.columns[0].lineage,
                expected,
                "{sql}"
            );
        }
    }
}

#[test]
fn partial_catalog_preserves_an_individually_proven_nonprefix_position() {
    struct Middle;
    impl Catalog for Middle {
        fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
            (query == &relation("t")).then(|| CatalogEntry {
                relation: query.clone(),
                columns: Cow::Owned(vec![CatalogColumn {
                    name: Ident::unquoted("x"),
                    ordinal: Some(2),
                    data_type: None,
                    fields: None,
                }]),
                coverage: SchemaCoverage::Partial,
                order: SchemaOrder::Unknown,
                unknown_segments: Cow::Owned(vec![CatalogUnknownSegment {
                    after_ordinal: None,
                }]),
                evidence: Cow::Borrowed(&[]),
            })
        }
    }
    // P2: column position and unknown-interval position are separate proofs.
    for sql in ["SELECT * FROM t", "SELECT * FROM (SELECT * FROM t) d"] {
        let statement = analyze(sql, &Middle);
        assert_eq!(statement.output.columns[0].ordinal, Some(2), "{sql}");
        assert!(
            matches!(&statement.output.shape, Shape::Open { unexpanded }
            if unexpanded.len() == 1 && unexpanded[0].after_ordinal.is_none()),
            "{sql}"
        );
    }
}

#[test]
fn p4_natural_schema_uncertainty_keeps_its_physical_subject_through_a_wrapper() {
    let catalog = Schemas::new(&[
        ("a", &["x"], SchemaCoverage::Exhaustive, SchemaOrder::Proven),
        ("b", &["y"], SchemaCoverage::Partial, SchemaOrder::Unknown),
    ]);
    for sql in [
        "SELECT x FROM a NATURAL JOIN b",
        "SELECT x FROM a NATURAL JOIN (SELECT * FROM b) d",
    ] {
        assert_eq!(
            analyze(sql, &catalog).output.columns[0].lineage,
            Lineage::Partial {
                known: vec![source("a", "x")],
                unresolved: vec![Unresolved::SchemaDependent {
                    what: SchemaDependencyKind::NaturalJoinColumns,
                    relations: vec![relation("b")],
                }],
            },
            "{sql}"
        );
    }
}

#[test]
fn p2_proven_positions_order_the_known_part_of_an_unordered_catalog() {
    struct Positions;
    impl Catalog for Positions {
        fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
            (query == &relation("t")).then(|| CatalogEntry {
                relation: query.clone(),
                columns: Cow::Owned(
                    [("a", 2), ("b", 0)]
                        .into_iter()
                        .map(|(name, ordinal)| CatalogColumn {
                            name: Ident::unquoted(name),
                            ordinal: Some(ordinal),
                            data_type: None,
                            fields: None,
                        })
                        .collect(),
                ),
                coverage: SchemaCoverage::Partial,
                order: SchemaOrder::Unknown,
                unknown_segments: Cow::Owned(vec![CatalogUnknownSegment {
                    after_ordinal: Some(0),
                }]),
                evidence: Cow::Borrowed(&[]),
            })
        }
    }
    let statement = analyze("SELECT * FROM t", &Positions);
    assert_eq!(
        statement
            .output
            .columns
            .iter()
            .map(|column| (&column.name, column.ordinal))
            .collect::<Vec<_>>(),
        vec![
            (&OutputName::Explicit(Ident::unquoted("b")), Some(0)),
            (&OutputName::Explicit(Ident::unquoted("a")), Some(2))
        ]
    );
    assert!(
        matches!(statement.output.shape, Shape::Open { unexpanded } if unexpanded[0].after_ordinal == Some(0))
    );
    let union = analyze(
        "SELECT * FROM t UNION ALL SELECT p, q, r FROM u",
        &Positions,
    );
    for (name, right) in [("b", "p"), ("a", "r")] {
        let column = union
            .output
            .columns
            .iter()
            .find(|column| column.name == OutputName::Explicit(Ident::unquoted(name)))
            .unwrap();
        assert_eq!(
            column.lineage,
            Lineage::Complete(vec![source("t", name), source("u", right)])
        );
    }
}

#[test]
fn partial_exclude_does_not_guess_where_an_unpositioned_column_was_removed() {
    struct Unpositioned;
    impl Catalog for Unpositioned {
        fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
            (query == &relation("t")).then(|| CatalogEntry {
                relation: query.clone(),
                columns: Cow::Owned(
                    [("x", Some(2)), ("y", None)]
                        .into_iter()
                        .map(|(name, ordinal)| CatalogColumn {
                            name: Ident::unquoted(name),
                            ordinal,
                            data_type: None,
                            fields: None,
                        })
                        .collect(),
                ),
                coverage: SchemaCoverage::Partial,
                order: SchemaOrder::Unknown,
                unknown_segments: Cow::Borrowed(&[]),
                evidence: Cow::Borrowed(&[]),
            })
        }
    }
    // P2: y can precede or follow x, so filtering it invalidates x's ordinal.
    let statement = Analyzer::new(Dialect::Generic)
        .with_catalog(&Unpositioned)
        .analyze("SELECT * EXCLUDE (y) FROM t")
        .unwrap()
        .into_single()
        .unwrap();
    assert_eq!(statement.output.columns.len(), 1);
    assert_eq!(statement.output.columns[0].ordinal, None);
    assert_eq!(
        statement.output.columns[0].lineage,
        Lineage::Complete(vec![source("t", "x")])
    );
}

#[test]
fn exhaustive_unordered_natural_keys_have_complete_values_without_proven_positions() {
    let catalog = Schemas::new(&[
        (
            "a",
            &["x", "y"],
            SchemaCoverage::Exhaustive,
            SchemaOrder::Unknown,
        ),
        (
            "b",
            &["y", "x"],
            SchemaCoverage::Exhaustive,
            SchemaOrder::Unknown,
        ),
    ]);
    // E1/P4: the key set is known; its order does not follow display order.
    let statement = analyze("SELECT * FROM a NATURAL JOIN b", &catalog);
    assert_eq!(statement.output.shape, Shape::Closed);
    for (column, name) in statement.output.columns.iter().zip(["x", "y"]) {
        assert_eq!(column.ordinal, None);
        assert_eq!(
            column.lineage,
            Lineage::Complete(vec![source("a", name), source("b", name)])
        );
    }
}
