//! Set-operation outputs keep one left-ordered slot identity from shape
//! construction through value-lineage evaluation.

use std::borrow::Cow;

use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, CatalogUnknownSegment, Dialect, Ident, Lineage,
    OutputName, Relation, SchemaCoverage, SchemaOrder, SetCorrespondence, SetOperand,
    SetOperationKind, SetOperator, SetQuantifier, Shape, SourceColumn, Unresolved,
};

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

fn analyze(sql: &str) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(Dialect::Generic)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("failed to analyze {sql}: {error}"))
        .into_single()
        .expect("one statement")
}

fn analyze_with<C: Catalog>(sql: &str, catalog: &C) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(Dialect::Generic)
        .with_catalog(catalog)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("failed to analyze {sql}: {error}"))
        .into_single()
        .expect("one statement")
}

fn assert_sources(column: &dlin_sqllineage::OutputColumn, expected: &[SourceColumn]) {
    let actual = match &column.lineage {
        Lineage::Complete(sources) => sources,
        other => panic!("expected complete lineage, got {other:?}"),
    };
    assert_eq!(actual, expected);
}

fn known_sources(lineage: &Lineage) -> &[SourceColumn] {
    match lineage {
        Lineage::Complete(sources) => sources,
        Lineage::Partial { known, .. } => known,
    }
}

#[derive(Clone)]
struct Schemas(Vec<(Relation, Vec<CatalogColumn>, SchemaCoverage, SchemaOrder)>);

impl Schemas {
    fn ordered(entries: &[(&str, &[&str])]) -> Self {
        Self(
            entries
                .iter()
                .map(|(table, columns)| {
                    (
                        relation(table),
                        columns
                            .iter()
                            .enumerate()
                            .map(|(ordinal, column)| CatalogColumn {
                                name: Ident::unquoted(*column),
                                ordinal: Some(ordinal),
                                data_type: None,
                                fields: None,
                            })
                            .collect(),
                        SchemaCoverage::Exhaustive,
                        SchemaOrder::Proven,
                    )
                })
                .collect(),
        )
    }

    fn unknown_order(table: &str, columns: &[&str]) -> Self {
        Self(vec![(
            relation(table),
            columns
                .iter()
                .map(|column| CatalogColumn {
                    name: Ident::unquoted(*column),
                    ordinal: None,
                    data_type: None,
                    fields: None,
                })
                .collect(),
            SchemaCoverage::Exhaustive,
            SchemaOrder::Unknown,
        )])
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
                evidence: Cow::Owned(vec![dlin_sqllineage::EvidenceRef(format!(
                    "catalog:{}",
                    name.name.value
                ))]),
            })
    }
}

#[test]
fn positional_slots_keep_left_names_and_pair_different_right_names() {
    let result =
        analyze("SELECT t.a AS left_name FROM t UNION ALL SELECT u.b AS right_name FROM u");
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 1);
    assert!(matches!(
        &result.output.columns[0].name,
        OutputName::Explicit(name) if name.value == "left_name"
    ));
    assert_eq!(result.output.columns[0].ordinal, Some(0));
    assert_sources(
        &result.output.columns[0],
        &[source("t", "a"), source("u", "b")],
    );
}

#[test]
fn duplicate_and_anonymous_outputs_keep_distinct_positional_slots() {
    let duplicate =
        analyze("SELECT t.a AS x, t.b AS x FROM t UNION ALL SELECT u.c AS y, u.d AS y FROM u");
    assert_eq!(duplicate.output.shape, Shape::Closed);
    assert_eq!(duplicate.output.columns.len(), 2);
    for (index, expected_sources) in [
        vec![source("t", "a"), source("u", "c")],
        vec![source("t", "b"), source("u", "d")],
    ]
    .iter()
    .enumerate()
    {
        assert!(matches!(
            &duplicate.output.columns[index].name,
            OutputName::Explicit(name) if name.value == "x"
        ));
        assert_eq!(duplicate.output.columns[index].ordinal, Some(index));
        assert_sources(&duplicate.output.columns[index], expected_sources);
    }

    let anonymous =
        analyze("SELECT t.a + 1, t.b + 1 FROM t UNION ALL SELECT u.c + 1, u.d + 1 FROM u");
    assert_eq!(anonymous.output.shape, Shape::Closed);
    assert_eq!(anonymous.output.columns.len(), 2);
    for (index, expected_sources) in [
        vec![source("t", "a"), source("u", "c")],
        vec![source("t", "b"), source("u", "d")],
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(anonymous.output.columns[index].name, OutputName::Anonymous);
        assert_eq!(anonymous.output.columns[index].ordinal, Some(index));
        assert_sources(&anonymous.output.columns[index], expected_sources);
    }
}

#[test]
fn three_operands_and_scope_wrappers_preserve_slot_sources() {
    let sql =
        "SELECT t.a AS x FROM t UNION ALL SELECT u.b AS y FROM u UNION ALL SELECT v.c AS z FROM v";
    let expected = [source("t", "a"), source("u", "b"), source("v", "c")];
    let direct = analyze(sql);
    assert_eq!(direct.output.shape, Shape::Closed);
    assert_eq!(direct.output.columns.len(), 1);
    assert_sources(&direct.output.columns[0], &expected);

    for wrapped in [
        format!("WITH c AS ({sql}) SELECT c.x FROM c"),
        format!("SELECT d.x FROM ({sql}) AS d"),
        format!("WITH c AS ({sql}), d AS (SELECT * FROM c) SELECT d.x FROM d"),
        format!("SELECT renamed FROM (SELECT * RENAME (x AS renamed) FROM ({sql}) d) r"),
    ] {
        let result = analyze(&wrapped);
        assert_eq!(result.output.shape, Shape::Closed, "{wrapped}");
        assert_eq!(result.output.columns.len(), 1, "{wrapped}");
        assert_sources(&result.output.columns[0], &expected);
    }
}

#[test]
fn schema_refinement_does_not_replace_a_previously_proven_source() {
    let sql = "SELECT b FROM (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) AS s";
    let without_catalog = analyze(sql);
    assert_eq!(without_catalog.output.shape, Shape::Closed);
    assert!(matches!(
        &without_catalog.output.columns[0].lineage,
        Lineage::Partial { known, .. } if known.is_empty()
    ));

    let catalog = Schemas::ordered(&[("t1", &["b", "a"]), ("t2", &["a", "b"])]);
    let refined = analyze_with(sql, &catalog);
    assert_eq!(refined.output.shape, Shape::Closed);
    assert_sources(
        &refined.output.columns[0],
        &[source("t1", "b"), source("t2", "a")],
    );
}

#[test]
fn i1_alignment_refinement_is_monotone_for_nonempty_known_sources() {
    let sql = "SELECT t.a AS p, t.*, t.z AS suffix FROM t UNION ALL SELECT u.b AS q, u.x AS r, u.y AS s, u.w AS v FROM u";
    let before = analyze(sql);
    let after = analyze_with(sql, &Schemas::ordered(&[("t", &["a", "z"])]));
    assert!(matches!(before.output.shape, Shape::Open { .. }));

    let before_p = before
        .output
        .columns
        .iter()
        .find(|column| matches!(&column.name, OutputName::Explicit(name) if name.value == "p"))
        .expect("explicit prefix output p");
    assert_sources(before_p, &[source("t", "a"), source("u", "b")]);
    let before_suffix = before
        .output
        .columns
        .iter()
        .find(|column| matches!(&column.name, OutputName::Explicit(name) if name.value == "suffix"))
        .expect("explicit suffix output");
    assert_eq!(known_sources(&before_suffix.lineage), &[source("t", "z")]);

    for name in ["p", "suffix"] {
        let before_column = before
            .output
            .columns
            .iter()
            .find(|column| matches!(&column.name, OutputName::Explicit(output) if output.value == name))
            .expect("before column");
        let after_column = after
            .output
            .columns
            .iter()
            .find(|column| matches!(&column.name, OutputName::Explicit(output) if output.value == name))
            .expect("after column");
        let before_known = known_sources(&before_column.lineage);
        let after_known = known_sources(&after_column.lineage);
        assert!(
            !before_known.is_empty(),
            "I1 must exercise a nonempty source set"
        );
        assert!(
            before_known.iter().all(|known| after_known.contains(known)),
            "refinement retracted a known source for {name}: before={before_known:?}, after={after_known:?}"
        );
    }
    let after_suffix = after
        .output
        .columns
        .iter()
        .find(|column| matches!(&column.name, OutputName::Explicit(name) if name.value == "suffix"))
        .expect("refined suffix output");
    assert_eq!(
        known_sources(&after_suffix.lineage),
        &[source("t", "z"), source("u", "w")]
    );
}

#[test]
fn k3_reports_unproven_alignment_with_finite_candidates() {
    let sql = "SELECT b FROM (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) AS s";
    let result = analyze(sql);
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 1);
    let operation = SetOperationKind {
        operator: SetOperator::Union,
        quantifier: SetQuantifier::All,
        correspondence: SetCorrespondence::Positional,
    };
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![
                Unresolved::NamedBehindStar {
                    relation: relation("t1"),
                    column: Ident::unquoted("b"),
                },
                Unresolved::AlignmentUnknown {
                    operation,
                    possible_sources: vec![source("t2", "a"), source("t2", "b")],
                    unknown_operands: vec![dlin_sqllineage::SetOperand::Left],
                },
            ],
        }
    );
}

#[test]
fn e4_adds_order_without_retracting_listed_star_sources() {
    let sql = "SELECT * FROM t";
    let previous = Schemas::unknown_order("t", &["z", "a"]);
    let refined = Schemas::ordered(&[("t", &["z", "a"])]);

    let before = analyze_with(sql, &previous);
    let after = analyze_with(sql, &refined);
    assert_eq!(before.output.shape, Shape::Closed);
    assert_eq!(after.output.shape, Shape::Closed);
    assert_eq!(before.output.columns.len(), 2);
    assert_eq!(after.output.columns.len(), 2);
    for (index, name) in ["z", "a"].iter().enumerate() {
        assert_eq!(before.output.columns[index].ordinal, None);
        assert_eq!(after.output.columns[index].ordinal, Some(index));
        assert_sources(&before.output.columns[index], &[source("t", name)]);
        assert_sources(&after.output.columns[index], &[source("t", name)]);
    }
}

#[test]
fn e4_refines_a_set_operation_alignment_without_losing_its_proven_prefix() {
    let sql = "SELECT * FROM (SELECT t.z AS k, t.* FROM t) d UNION ALL SELECT p, q, r FROM u";
    let before = analyze_with(sql, &Schemas::unknown_order("t", &["z", "a"]));
    assert_eq!(before.output.shape, Shape::Closed);
    assert_eq!(before.output.columns.len(), 3);
    assert_eq!(before.output.columns[0].ordinal, Some(0));
    assert_sources(
        &before.output.columns[0],
        &[source("t", "z"), source("u", "p")],
    );
    assert!(
        before.output.columns[1..]
            .iter()
            .all(|column| { matches!(&column.lineage, Lineage::Partial { .. }) })
    );

    let after = analyze_with(sql, &Schemas::ordered(&[("t", &["z", "a"])]));
    assert_eq!(after.output.shape, Shape::Closed);
    assert_eq!(after.output.columns.len(), 3);
    assert_eq!(after.output.columns[0].ordinal, Some(0));
    assert_sources(
        &after.output.columns[0],
        &[source("t", "z"), source("u", "p")],
    );
    assert_sources(
        &after.output.columns[1],
        &[source("t", "z"), source("u", "q")],
    );
    assert_sources(
        &after.output.columns[2],
        &[source("t", "a"), source("u", "r")],
    );
}

#[test]
fn sparse_unknown_intervals_match_only_positions_within_their_proven_bounds() {
    struct SparsePartial;
    impl Catalog for SparsePartial {
        fn lookup(&self, query: &Relation) -> Option<CatalogEntry<'_>> {
            (query == &relation("t")).then(|| CatalogEntry {
                relation: query.clone(),
                columns: Cow::Owned(
                    [("first", 0), ("middle", 2), ("last", 4)]
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
                unknown_segments: Cow::Owned(vec![
                    CatalogUnknownSegment {
                        after_ordinal: Some(0),
                    },
                    CatalogUnknownSegment {
                        after_ordinal: Some(2),
                    },
                ]),
                evidence: Cow::Owned(vec![dlin_sqllineage::EvidenceRef("catalog:t".to_owned())]),
            })
        }
    }

    let result = analyze_with(
        "SELECT * FROM t UNION ALL SELECT a, b, c, d, e FROM u",
        &SparsePartial,
    );
    assert_eq!(result.output.columns.len(), 3);
    for (index, (name, ordinal, right_name)) in
        [("first", 0, "a"), ("middle", 2, "c"), ("last", 4, "e")]
            .iter()
            .enumerate()
    {
        assert!(matches!(
            &result.output.columns[index].name,
            OutputName::Explicit(output) if output.value == *name
        ));
        assert_eq!(result.output.columns[index].ordinal, Some(*ordinal));
        assert_sources(
            &result.output.columns[index],
            &[source("t", name), source("u", right_name)],
        );
    }

    let Shape::Open { unexpanded } = result.output.shape else {
        panic!("the partial left schema must retain its two intervals");
    };
    assert_eq!(unexpanded.len(), 2);
    for (marker, (after, candidate)) in unexpanded.iter().zip([(Some(0), "b"), (Some(2), "d")]) {
        assert_eq!(marker.relation, Some(relation("t")));
        assert_eq!(marker.after_ordinal, after);
        let Lineage::Partial { unresolved, .. } = &marker.lineage else {
            panic!("unknown intervals are partial");
        };
        assert!(unresolved.iter().any(|reason| matches!(
            reason,
            Unresolved::AlignmentUnknown {
                possible_sources,
                unknown_operands,
                ..
            } if possible_sources == &[source("u", candidate)]
                && unknown_operands == &[SetOperand::Left]
        )));
    }

    // The last established ordinal also bounds the width of the star. A
    // following explicit slot cannot be positioned by counting listed names.
    let result = analyze_with(
        "SELECT t.*, t.last AS suffix FROM t UNION ALL SELECT a,b,c,d,e,f FROM u",
        &SparsePartial,
    );
    let suffix = result
        .output
        .columns
        .iter()
        .find(|column| {
            matches!(
                &column.name, OutputName::Explicit(name) if name.value == "suffix"
            )
        })
        .expect("the left operand publishes its explicit suffix");
    assert_eq!(suffix.ordinal, None);
    assert_eq!(
        suffix.lineage,
        Lineage::Partial {
            known: vec![source("t", "last")],
            unresolved: vec![Unresolved::AlignmentUnknown {
                operation: SetOperationKind {
                    operator: SetOperator::Union,
                    quantifier: SetQuantifier::All,
                    correspondence: SetCorrespondence::Positional,
                },
                possible_sources: vec![source("u", "f")],
                unknown_operands: vec![],
            }],
        }
    );
}

#[test]
fn e5_replacement_can_add_a_column_when_old_exhaustiveness_was_contradicted() {
    let sql = "SELECT * FROM t";
    let previous = Schemas::ordered(&[("t", &["a", "z"])]);
    let replacement = Schemas::ordered(&[("t", &["a", "z", "extra"])]);

    let before = analyze_with(sql, &previous);
    let after = analyze_with(sql, &replacement);
    assert_eq!(before.output.shape, Shape::Closed);
    assert_eq!(after.output.shape, Shape::Closed);
    assert_eq!(before.output.columns.len(), 2);
    assert_eq!(after.output.columns.len(), 3);
    assert_sources(&before.output.columns[0], &[source("t", "a")]);
    assert_sources(&before.output.columns[1], &[source("t", "z")]);
    assert_sources(&after.output.columns[0], &[source("t", "a")]);
    assert_sources(&after.output.columns[1], &[source("t", "z")]);
    assert_sources(&after.output.columns[2], &[source("t", "extra")]);
}

#[test]
fn unknown_intervals_do_not_duplicate_the_same_operand_marker() {
    let result = analyze("SELECT t.id AS k, t.* FROM t UNION ALL SELECT u.id AS k, u.* FROM u");
    assert_eq!(result.output.columns.len(), 1);
    assert_sources(
        &result.output.columns[0],
        &[source("t", "id"), source("u", "id")],
    );
    let Shape::Open { unexpanded } = result.output.shape else {
        panic!("the two unexpanded operands must keep the output open");
    };
    assert_eq!(unexpanded.len(), 2, "each operand contributes one interval");
    assert_eq!(
        unexpanded
            .iter()
            .filter(|marker| marker.relation == Some(relation("t")))
            .count(),
        1
    );
    assert_eq!(
        unexpanded
            .iter()
            .filter(|marker| marker.relation == Some(relation("u")))
            .count(),
        1
    );
}

#[test]
fn three_operands_keep_one_unknown_interval_per_input() {
    let result = analyze(
        "SELECT t.id AS k, t.* FROM t UNION ALL SELECT u.id AS k, u.* FROM u UNION ALL SELECT v.id AS k, v.* FROM v",
    );
    assert_eq!(result.output.columns.len(), 1);
    assert_sources(
        &result.output.columns[0],
        &[source("t", "id"), source("u", "id"), source("v", "id")],
    );
    let Shape::Open { unexpanded } = result.output.shape else {
        panic!("all three unknown intervals must remain open");
    };
    assert_eq!(unexpanded.len(), 3);
    for table in ["t", "u", "v"] {
        assert_eq!(
            unexpanded
                .iter()
                .filter(|marker| marker.relation == Some(relation(table)))
                .count(),
            1,
            "{table} must occur once"
        );
    }
}

#[test]
fn an_unbound_operand_source_is_retained_after_positional_merge() {
    let result = analyze("SELECT missing AS x UNION ALL SELECT u.y AS y FROM u");
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![source("u", "y")],
            unresolved: vec![Unresolved::Unbound {
                column: Ident::unquoted("missing"),
            }],
        }
    );
}

#[test]
fn a_right_open_operand_is_reported_as_unenumerated() {
    let result = analyze("SELECT t.a AS x FROM t UNION ALL SELECT *, u.y AS y FROM u");
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![source("t", "a")],
            unresolved: vec![Unresolved::AlignmentUnknown {
                operation: SetOperationKind {
                    operator: SetOperator::Union,
                    quantifier: SetQuantifier::All,
                    correspondence: SetCorrespondence::Positional,
                },
                possible_sources: vec![source("u", "y")],
                unknown_operands: vec![dlin_sqllineage::SetOperand::Right],
            }],
        }
    );
}

#[test]
fn unknown_order_keeps_the_named_left_slots_without_guessing_right_positions() {
    let catalog = Schemas::unknown_order("t", &["z", "a"]);
    let result = analyze_with("SELECT p, q FROM s UNION ALL SELECT * FROM t", &catalog);
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 2);
    for (index, name) in ["p", "q"].iter().enumerate() {
        assert_eq!(result.output.columns[index].ordinal, Some(index));
        assert_eq!(
            result.output.columns[index].lineage,
            Lineage::Partial {
                known: vec![source("s", name)],
                unresolved: vec![Unresolved::AlignmentUnknown {
                    operation: SetOperationKind {
                        operator: SetOperator::Union,
                        quantifier: SetQuantifier::All,
                        correspondence: SetCorrespondence::Positional,
                    },
                    possible_sources: vec![source("t", "a"), source("t", "z")],
                    unknown_operands: vec![],
                }],
            }
        );
    }
}

#[test]
fn alignment_reason_retains_the_intersect_operator_and_distinct_quantifier() {
    let catalog = Schemas::unknown_order("t", &["z", "a"]);
    let result = analyze_with(
        "SELECT p, q FROM s INTERSECT DISTINCT SELECT * FROM t",
        &catalog,
    );
    for (index, name) in ["p", "q"].iter().enumerate() {
        assert_eq!(
            result.output.columns[index].lineage,
            Lineage::Partial {
                known: vec![source("s", name)],
                unresolved: vec![Unresolved::AlignmentUnknown {
                    operation: SetOperationKind {
                        operator: SetOperator::Intersect,
                        quantifier: SetQuantifier::Distinct,
                        correspondence: SetCorrespondence::Positional,
                    },
                    possible_sources: vec![source("t", "a"), source("t", "z")],
                    unknown_operands: vec![],
                }],
            }
        );
    }
}

#[test]
fn alignment_reason_retains_the_intersect_operator_and_all_quantifier() {
    let catalog = Schemas::unknown_order("t", &["z", "a"]);
    let result = analyze_with("SELECT p, q FROM s INTERSECT ALL SELECT * FROM t", &catalog);
    for column in &result.output.columns {
        assert!(matches!(
            &column.lineage,
            Lineage::Partial { unresolved, .. }
                if unresolved.iter().any(|reason| matches!(
                    reason,
                    Unresolved::AlignmentUnknown {
                        operation: SetOperationKind {
                            operator: SetOperator::Intersect,
                            quantifier: SetQuantifier::All,
                            correspondence: SetCorrespondence::Positional,
                        },
                        ..
                    }
                ))
        ));
    }
}

#[test]
fn a_known_suffix_after_an_unknown_interval_keeps_its_left_value_and_candidates() {
    let sql = "SELECT * FROM (SELECT 1 AS p, t.*, t.z AS suffix FROM t UNION ALL SELECT u.a, u.b, u.c FROM u) s";
    let result = analyze(sql);
    assert!(matches!(result.output.shape, Shape::Open { .. }));
    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(result.output.columns[0].ordinal, Some(0));
    assert_sources(&result.output.columns[0], &[source("u", "a")]);
    assert!(matches!(
        &result.output.columns[1].name,
        OutputName::Explicit(name) if name.value == "suffix"
    ));
    assert_eq!(
        result.output.columns[1].lineage,
        Lineage::Partial {
            known: vec![source("t", "z")],
            unresolved: vec![Unresolved::AlignmentUnknown {
                operation: SetOperationKind {
                    operator: SetOperator::Union,
                    quantifier: SetQuantifier::All,
                    correspondence: SetCorrespondence::Positional,
                },
                possible_sources: vec![source("u", "b"), source("u", "c")],
                unknown_operands: vec![],
            }],
        }
    );
}

#[test]
fn finite_nested_alignment_candidates_do_not_become_unknown_operands() {
    let catalog = Schemas::unknown_order("t", &["a", "b"]);
    let result = analyze_with(
        "SELECT p, q FROM s UNION ALL SELECT * FROM (SELECT * FROM t UNION ALL SELECT x, y FROM u) n",
        &catalog,
    );
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 2);
    let all_candidates = [
        source("t", "a"),
        source("t", "b"),
        source("u", "x"),
        source("u", "y"),
    ];
    for (column, left) in result.output.columns.iter().zip(["p", "q"]) {
        assert!(
            matches!(
                &column.lineage,
                Lineage::Partial { known, unresolved }
                    if known == &[source("s", left)]
                        && unresolved.iter().any(|reason| matches!(
                            reason,
                            Unresolved::AlignmentUnknown {
                                operation: SetOperationKind {
                                    operator: SetOperator::Union,
                                    quantifier: SetQuantifier::All,
                                    correspondence: SetCorrespondence::Positional,
                                },
                                possible_sources,
                                unknown_operands,
                            } if possible_sources == &all_candidates && unknown_operands.is_empty()
                        ))
            ),
            "unexpected nested alignment lineage: {:?}",
            column.lineage
        );
    }
}
