use dlin_sqllineage::{
    Diagnostic, DiagnosticKind, Dialect, Lineage, Severity, Shape, UnexpandedStar, Unresolved,
};

use super::support::{
    Case, ExpectedColumn, col, duplicate_name, partial_col, q, relation, source, u,
};

fn open_star(table: &str, after_ordinal: Option<usize>) -> Shape {
    Shape::Open {
        unexpanded: vec![UnexpandedStar {
            relation: Some(relation(table)),
            after_ordinal,
            lineage: Lineage::Partial {
                known: vec![],
                unresolved: vec![Unresolved::BehindStar {
                    relation: relation(table),
                }],
            },
        }],
    }
}

fn physical(ordinal: usize, table: &str, column: &str) -> ExpectedColumn {
    col(ordinal, u(column), vec![source(relation(table), u(column))])
}

pub fn cases() -> Vec<Case> {
    let mut unknown_tail = col(0, u("tail"), vec![]);
    unknown_tail.ordinal = None;
    let mut unknown_middle = col(0, u("middle"), vec![]);
    unknown_middle.ordinal = None;
    let mut cases = vec![
        Case::new(
            "explicit_source_column_before_unknown_star",
            "SELECT t.id AS chosen, t.* FROM t",
            Dialect::Generic,
            vec![col(0, u("chosen"), vec![source(relation("t"), u("id"))])],
        )
        .shape(open_star("t", Some(0)))
        .unresolved_stars(true),
        Case::new(
            "explicit_source_column_before_catalog_star",
            "SELECT t.value AS chosen, t.* FROM t",
            Dialect::Generic,
            vec![
                col(0, u("chosen"), vec![source(relation("t"), u("value"))]),
                physical(1, "t", "value"),
                physical(2, "t", "id"),
            ],
        )
        .catalog(vec![(relation("t"), vec![u("value"), u("id")])]),
        Case::new(
            "named_column_behind_cte_star",
            "WITH c AS (SELECT * FROM t) SELECT id FROM c",
            Dialect::Generic,
            vec![partial_col(
                0,
                u("id"),
                vec![],
                vec![Unresolved::NamedBehindStar {
                    relation: relation("t"),
                    column: u("id"),
                }],
            )],
        )
        .unresolved_stars(true),
        Case::new(
            "named_column_behind_derived_star",
            "SELECT id FROM (SELECT * FROM t) AS d",
            Dialect::Generic,
            vec![partial_col(
                0,
                u("id"),
                vec![],
                vec![Unresolved::NamedBehindStar {
                    relation: relation("t"),
                    column: u("id"),
                }],
            )],
        )
        .unresolved_stars(true),
        Case::new(
            "multiple_unknown_stars_lose_later_ordinals",
            "SELECT 1 AS prefix, a.*, 2 AS middle, b.*, 3 AS tail FROM a CROSS JOIN b",
            Dialect::Generic,
            vec![
                col(0, u("prefix"), vec![]),
                unknown_middle,
                unknown_tail.clone(),
            ],
        )
        .shape(Shape::Open {
            unexpanded: vec![
                UnexpandedStar {
                    relation: Some(relation("a")),
                    after_ordinal: Some(0),
                    lineage: Lineage::Partial {
                        known: vec![],
                        unresolved: vec![Unresolved::BehindStar {
                            relation: relation("a"),
                        }],
                    },
                },
                UnexpandedStar {
                    relation: Some(relation("b")),
                    after_ordinal: None,
                    lineage: Lineage::Partial {
                        known: vec![],
                        unresolved: vec![Unresolved::BehindStar {
                            relation: relation("b"),
                        }],
                    },
                },
            ],
        })
        .unresolved_stars(true),
        Case::new(
            "nested_unknown_star_with_closed_final_output",
            "SELECT a.id FROM a JOIN (SELECT * FROM b) d ON TRUE",
            Dialect::Generic,
            vec![physical(0, "a", "id")],
        )
        .unresolved_stars(true),
        Case::new(
            "star_without_catalog",
            "SELECT * FROM t",
            Dialect::Generic,
            vec![],
        )
        .shape(open_star("t", None))
        .unresolved_stars(true),
        Case::new(
            "qualified_star_without_catalog",
            "SELECT x.* FROM t AS x",
            Dialect::Generic,
            vec![],
        )
        .shape(open_star("t", None))
        .unresolved_stars(true),
        Case::new(
            "open_star_between_literals",
            "SELECT 1 AS prefix, t.*, 2 AS tail FROM t",
            Dialect::Generic,
            vec![col(0, u("prefix"), vec![]), unknown_tail],
        )
        .shape(open_star("t", Some(0)))
        .unresolved_stars(true),
        Case::new(
            "catalog_star_uses_declared_order",
            "SELECT * FROM t",
            Dialect::Generic,
            vec![physical(0, "t", "z"), physical(1, "t", "a")],
        )
        .catalog(vec![(relation("t"), vec![u("z"), u("a")])]),
        Case::new(
            "catalog_star_keeps_quoted_names",
            "SELECT * FROM t",
            Dialect::Generic,
            vec![
                col(0, q("MiXeD"), vec![source(relation("t"), q("MiXeD"))]),
                physical(1, "t", "id"),
            ],
        )
        .catalog(vec![(relation("t"), vec![q("MiXeD"), u("id")])]),
        Case::new(
            "qualified_catalog_star_alias",
            "SELECT x.* FROM t AS x",
            Dialect::Generic,
            vec![physical(0, "t", "id"), physical(1, "t", "value")],
        )
        .catalog(vec![(relation("t"), vec![u("id"), u("value")])]),
        Case::new(
            "catalog_star_between_literals",
            "SELECT 1 AS prefix, t.*, 2 AS tail FROM t",
            Dialect::Generic,
            vec![
                col(0, u("prefix"), vec![]),
                physical(1, "t", "id"),
                physical(2, "t", "value"),
                col(3, u("tail"), vec![]),
            ],
        )
        .catalog(vec![(relation("t"), vec![u("id"), u("value")])]),
        Case::new(
            "qualified_stars_follow_projection_order",
            "SELECT b.*, a.* FROM a CROSS JOIN b",
            Dialect::Generic,
            vec![
                physical(0, "b", "right_value"),
                physical(1, "a", "left_value"),
            ],
        )
        .catalog(vec![
            (relation("a"), vec![u("left_value")]),
            (relation("b"), vec![u("right_value")]),
        ]),
        Case::new(
            "schema_qualified_catalog_star",
            "SELECT x.* FROM sales.orders AS x",
            Dialect::Generic,
            vec![
                physical(0, "sales.orders", "id"),
                physical(1, "sales.orders", "amount"),
            ],
        )
        .catalog(vec![(relation("sales.orders"), vec![u("id"), u("amount")])]),
        Case::new(
            "cte_star_uses_projection_names",
            "WITH c AS (SELECT id AS renamed, value FROM t) SELECT * FROM c",
            Dialect::Generic,
            vec![
                col(0, u("renamed"), vec![source(relation("t"), u("id"))]),
                physical(1, "t", "value"),
            ],
        ),
        Case::new(
            "derived_star_uses_projection_order",
            "SELECT d.* FROM (SELECT value, id FROM t) AS d",
            Dialect::Generic,
            vec![physical(0, "t", "value"), physical(1, "t", "id")],
        ),
        Case::new(
            "star_named_quoted_column_is_not_a_wildcard",
            "WITH c AS (SELECT 1 AS \"*\", 2 AS \"Mixed\") SELECT * FROM c",
            Dialect::Generic,
            vec![col(0, q("*"), vec![]), col(1, q("Mixed"), vec![])],
        ),
        Case::new(
            "set_arity_error_is_explicit",
            "SELECT id FROM a UNION ALL SELECT id, value FROM b",
            Dialect::Generic,
            vec![],
        )
        .diagnostics(vec![Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 },
            location: None,
        }]),
    ];

    for (name, operator) in [("union_all", "UNION ALL"), ("union_distinct", "UNION")] {
        cases.push(
            Case::new(
                &format!("catalog_stars_{name}"),
                &format!("SELECT * FROM a {operator} SELECT * FROM b"),
                Dialect::Generic,
                vec![
                    col(
                        0,
                        u("id"),
                        vec![
                            source(relation("a"), u("id")),
                            source(relation("b"), u("key")),
                        ],
                    ),
                    col(
                        1,
                        u("value"),
                        vec![
                            source(relation("a"), u("value")),
                            source(relation("b"), u("payload")),
                        ],
                    ),
                ],
            )
            .catalog(vec![
                (relation("a"), vec![u("id"), u("value")]),
                (relation("b"), vec![u("key"), u("payload")]),
            ]),
        );
    }
    cases.push(
        Case::new(
            "star_union_source_free_branch",
            "SELECT * FROM t UNION ALL SELECT 1 AS other, 2 AS ignored",
            Dialect::Generic,
            vec![physical(0, "t", "id"), physical(1, "t", "value")],
        )
        .catalog(vec![(relation("t"), vec![u("id"), u("value")])]),
    );

    for (name, join) in [
        ("using", "JOIN b USING (id)"),
        ("natural", "NATURAL JOIN b"),
    ] {
        let sides = vec![
            source(relation("a"), u("id")),
            source(relation("b"), u("id")),
        ];
        let columns = vec![
            col(0, u("id"), sides.clone()),
            physical(1, "a", "left_value"),
            physical(2, "b", "right_value"),
        ];
        cases.push(
            Case::new(
                &format!("merged_{name}_star"),
                &format!("SELECT * FROM a {join}"),
                Dialect::Generic,
                columns,
            )
            .catalog(vec![
                (relation("a"), vec![u("id"), u("left_value")]),
                (relation("b"), vec![u("id"), u("right_value")]),
            ]),
        );
    }
    for (name, sql, contributors) in [
        (
            "using_chain_left_then_right",
            "SELECT * FROM a LEFT JOIN b USING (id) RIGHT JOIN c USING (id)",
            vec!["c"],
        ),
        (
            "using_chain_right_then_full",
            "SELECT * FROM a RIGHT JOIN b USING (id) FULL JOIN c USING (id)",
            vec!["b", "c"],
        ),
        (
            "mixed_using_natural_chain",
            "SELECT * FROM a LEFT JOIN b USING (id) NATURAL JOIN c",
            vec!["a", "c"],
        ),
        (
            "mixed_natural_using_chain",
            "SELECT * FROM a NATURAL RIGHT JOIN b LEFT JOIN c USING (id)",
            vec!["b"],
        ),
        (
            "parenthesized_right_inner_group",
            "SELECT * FROM a LEFT JOIN (b RIGHT JOIN c USING (id)) USING (id)",
            vec!["a"],
        ),
        (
            "parenthesized_right_full_group",
            "SELECT * FROM a FULL JOIN (b RIGHT JOIN c USING (id)) USING (id)",
            vec!["a", "c"],
        ),
        (
            "parenthesized_right_natural_group",
            "SELECT * FROM a NATURAL RIGHT JOIN (b NATURAL LEFT JOIN c)",
            vec!["b"],
        ),
    ] {
        cases.push(
            Case::new(
                name,
                sql,
                Dialect::Generic,
                vec![
                    col(
                        0,
                        u("id"),
                        contributors
                            .iter()
                            .map(|table| source(relation(table), u("id")))
                            .collect(),
                    ),
                    physical(1, "a", "left_value"),
                    physical(2, "b", "middle_value"),
                    physical(3, "c", "right_value"),
                ],
            )
            .catalog(vec![
                (relation("a"), vec![u("left_value"), u("id")]),
                (relation("b"), vec![u("middle_value"), u("id")]),
                (relation("c"), vec![u("right_value"), u("id")]),
            ]),
        );
    }
    cases.push(
        Case::new(
            "qualified_and_merged_stars",
            "SELECT a.*, *, b.* FROM a LEFT JOIN b USING (id)",
            Dialect::Generic,
            vec![
                physical(0, "a", "left_value"),
                physical(1, "a", "id"),
                physical(2, "a", "id"),
                physical(3, "a", "left_value"),
                physical(4, "b", "right_value"),
                physical(5, "b", "right_value"),
                physical(6, "b", "id"),
            ],
        )
        .catalog(vec![
            (relation("a"), vec![u("left_value"), u("id")]),
            (relation("b"), vec![u("right_value"), u("id")]),
        ])
        .diagnostics(vec![
            duplicate_name(u("id"), vec![1, 2, 6]),
            duplicate_name(u("left_value"), vec![0, 3]),
            duplicate_name(u("right_value"), vec![4, 5]),
        ]),
    );
    for (name, join, keys) in [
        (
            "using_keys_follow_list_order",
            "JOIN b USING (second, first)",
            vec!["second", "first"],
        ),
        (
            "natural_keys_follow_left_order",
            "NATURAL JOIN b",
            vec!["first", "second"],
        ),
    ] {
        let mut columns = keys
            .iter()
            .enumerate()
            .map(|(ordinal, key)| {
                col(
                    ordinal,
                    u(key),
                    vec![source(relation("a"), u(key)), source(relation("b"), u(key))],
                )
            })
            .collect::<Vec<_>>();
        columns.extend([
            physical(2, "a", "left_value"),
            physical(3, "b", "right_value"),
        ]);
        cases.push(
            Case::new(
                name,
                &format!("SELECT * FROM a {join}"),
                Dialect::Generic,
                columns,
            )
            .catalog(vec![
                (
                    relation("a"),
                    vec![u("left_value"), u("first"), u("second")],
                ),
                (
                    relation("b"),
                    vec![u("second"), u("right_value"), u("first")],
                ),
            ]),
        );
    }
    for (name, sql) in [
        (
            "merged_star_union_width",
            "SELECT * FROM a JOIN b USING (id) UNION ALL SELECT id, left_value, 1 AS other FROM a",
        ),
        (
            "derived_merged_star_width",
            "SELECT * FROM (SELECT * FROM a JOIN b USING (id)) d UNION ALL SELECT id, left_value, 1 AS other FROM a",
        ),
    ] {
        cases.push(
            Case::new(
                name,
                sql,
                Dialect::Generic,
                vec![
                    col(
                        0,
                        u("id"),
                        vec![
                            source(relation("a"), u("id")),
                            source(relation("b"), u("id")),
                        ],
                    ),
                    physical(1, "a", "left_value"),
                    physical(2, "b", "right_value"),
                ],
            )
            .catalog(vec![
                (relation("a"), vec![u("left_value"), u("id")]),
                (relation("b"), vec![u("right_value"), u("id")]),
            ]),
        );
    }
    // The left catalog determines the merged label, even for a RIGHT join.
    cases.push(
        Case::new(
            "merged_key_uses_left_catalog_spelling",
            "SELECT * FROM a RIGHT JOIN b USING (id)",
            Dialect::Generic,
            vec![col(0, q("ID"), vec![source(relation("b"), u("id"))])],
        )
        .catalog(vec![
            (relation("a"), vec![q("ID")]),
            (relation("b"), vec![u("id")]),
        ]),
    );
    for (name, join) in [
        ("partial_catalog_using_star", "JOIN b USING (id)"),
        ("partial_catalog_natural_star", "NATURAL JOIN b"),
    ] {
        cases.push(
            Case::new(
                name,
                &format!("SELECT * FROM a {join}"),
                Dialect::Generic,
                vec![physical(0, "a", "left_value"), physical(1, "a", "id")],
            )
            .catalog(vec![(relation("a"), vec![u("left_value"), u("id")])])
            .shape(open_star("b", Some(1)))
            .unresolved_stars(true),
        );
    }
    cases.push(Case::new("renamed_derived_star_keeps_join_arity",
        "SELECT * FROM (SELECT * RENAME (a AS b, b AS c) FROM t) d JOIN u USING (b) UNION ALL SELECT b, b, tail FROM u",
        Dialect::Generic,
        vec![col(0, u("b"), vec![source(relation("t"), u("b")), source(relation("u"), u("b"))]),
             col(1, u("c"), vec![source(relation("t"), u("a")), source(relation("u"), u("b"))]),
             physical(2, "u", "tail")],
    ).catalog(vec![(relation("t"), vec![u("a"), u("b")]), (relation("u"), vec![u("b"), u("tail")])]));
    cases.push(Case::new("merged_star_rename_runs_on_whole_output",
        "SELECT * RENAME (id AS left_value, left_value AS renamed) FROM a JOIN b USING (id) UNION ALL SELECT id, left_value, 1 FROM a",
        Dialect::Generic,
        vec![col(0, u("renamed"), vec![source(relation("a"), u("id")), source(relation("b"), u("id"))]),
             physical(1, "a", "left_value"), physical(2, "b", "right_value")],
    ).catalog(vec![(relation("a"), vec![u("left_value"), u("id")]), (relation("b"), vec![u("right_value"), u("id")])]));
    for (name, join) in [
        ("unknown_using_star_is_not_guessed", "JOIN b USING (id)"),
        ("unknown_natural_star_is_not_guessed", "NATURAL JOIN b"),
    ] {
        cases.push(
            Case::new(
                name,
                &format!("SELECT * FROM a {join}"),
                Dialect::Generic,
                vec![],
            )
            .shape(Shape::Open {
                unexpanded: vec![
                    UnexpandedStar {
                        relation: Some(relation("a")),
                        after_ordinal: None,
                        lineage: Lineage::Partial {
                            known: vec![],
                            unresolved: vec![Unresolved::BehindStar {
                                relation: relation("a"),
                            }],
                        },
                    },
                    UnexpandedStar {
                        relation: Some(relation("b")),
                        after_ordinal: None,
                        lineage: Lineage::Partial {
                            known: vec![],
                            unresolved: vec![Unresolved::BehindStar {
                                relation: relation("b"),
                            }],
                        },
                    },
                ],
            })
            .unresolved_stars(true),
        );
    }
    for (name, sql, expected) in [
        (
            "aliased_merged_exclude_matches_union_width",
            "SELECT * EXCLUDE (x.id) FROM a AS x JOIN b AS y USING (id) UNION ALL SELECT left_value, right_value FROM c",
            vec![
                col(
                    0,
                    u("left_value"),
                    vec![
                        source(relation("a"), u("left_value")),
                        source(relation("c"), u("left_value")),
                    ],
                ),
                col(
                    1,
                    u("right_value"),
                    vec![
                        source(relation("b"), u("right_value")),
                        source(relation("c"), u("right_value")),
                    ],
                ),
            ],
        ),
        (
            "aliased_unmerged_exclude",
            "SELECT * EXCLUDE (x.id) FROM a AS x",
            vec![physical(0, "a", "left_value")],
        ),
        (
            "left_merged_exclude_right_alias",
            "SELECT * EXCLUDE (y.id) FROM a AS x LEFT JOIN b AS y USING (id) UNION ALL SELECT left_value, right_value FROM c",
            vec![
                col(
                    0,
                    u("left_value"),
                    vec![
                        source(relation("a"), u("left_value")),
                        source(relation("c"), u("left_value")),
                    ],
                ),
                col(
                    1,
                    u("right_value"),
                    vec![
                        source(relation("b"), u("right_value")),
                        source(relation("c"), u("right_value")),
                    ],
                ),
            ],
        ),
        (
            "exclude_does_not_match_hidden_physical_name",
            "SELECT * EXCLUDE (a.id) FROM a AS x",
            vec![physical(0, "a", "id"), physical(1, "a", "left_value")],
        ),
        (
            "exclude_implicit_binding_follows_column_key",
            "SELECT * EXCLUDE (A.id) FROM a",
            vec![physical(0, "a", "left_value")],
        ),
        (
            "exclude_derived_binding_not_base_source",
            "SELECT * EXCLUDE (x.id) FROM (SELECT id, left_value FROM a) x",
            vec![physical(0, "a", "left_value")],
        ),
    ] {
        cases.push(
            Case::new(name, sql, Dialect::Generic, expected).catalog(vec![
                (relation("a"), vec![u("id"), u("left_value")]),
                (relation("b"), vec![u("id"), u("right_value")]),
            ]),
        );
    }
    cases.push(
        Case::new(
            "unmerged_exclude_matches_union_width",
            "SELECT * EXCLUDE (x.id) FROM a AS x UNION ALL SELECT left_value FROM c",
            Dialect::Generic,
            vec![col(
                0,
                u("left_value"),
                vec![
                    source(relation("a"), u("left_value")),
                    source(relation("c"), u("left_value")),
                ],
            )],
        )
        .catalog(vec![(relation("a"), vec![u("id"), u("left_value")])]),
    );
    cases.push(
        Case::new(
            "exclude_self_join_keeps_other_range_variable",
            "SELECT * EXCLUDE (x.id) FROM a AS x CROSS JOIN a AS y",
            Dialect::Generic,
            vec![
                physical(0, "a", "left_value"),
                physical(1, "a", "id"),
                physical(2, "a", "left_value"),
            ],
        )
        .catalog(vec![(relation("a"), vec![u("id"), u("left_value")])])
        .diagnostics(vec![duplicate_name(u("left_value"), vec![0, 2])]),
    );
    cases.push(
        Case::new(
            "qualified_star_exclude_uses_column_key",
            "SELECT x.* EXCLUDE (X.id) FROM a AS x UNION ALL SELECT left_value FROM c",
            Dialect::Generic,
            vec![col(
                0,
                u("left_value"),
                vec![
                    source(relation("a"), u("left_value")),
                    source(relation("c"), u("left_value")),
                ],
            )],
        )
        .catalog(vec![(relation("a"), vec![u("id"), u("left_value")])]),
    );
    let mut schema_table = relation("a");
    schema_table.schema = Some(u("s"));
    cases.push(
        Case::new(
            "qualified_relation_star_keeps_implicit_range_variable",
            "SELECT s.a.* EXCLUDE (a.id) FROM s.a UNION ALL SELECT left_value FROM c",
            Dialect::Generic,
            vec![col(
                0,
                u("left_value"),
                vec![
                    source(schema_table.clone(), u("left_value")),
                    source(relation("c"), u("left_value")),
                ],
            )],
        )
        .catalog(vec![(schema_table, vec![u("id"), u("left_value")])]),
    );
    cases
}

#[test]
fn merged_star_inherits_only_contributing_transforms() {
    use dlin_sqllineage::{Analyzer, Transform};
    for (join, expected, tables) in [
        ("LEFT JOIN", Transform::Aggregation, vec!["a"]),
        ("RIGHT JOIN", Transform::Direct, vec!["b"]),
        ("FULL JOIN", Transform::Aggregation, vec!["a", "b"]),
        ("JOIN", Transform::Aggregation, vec!["a", "b"]),
    ] {
        let sql = format!(
            "SELECT * FROM (SELECT max(value) AS id FROM a) l {join} (SELECT id FROM b) r USING (id)"
        );
        let result = Analyzer::new(Dialect::Generic)
            .analyze(&sql)
            .unwrap()
            .into_single()
            .unwrap();
        assert!(
            result.diagnostics.is_empty(),
            "{sql}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.output.columns.len(), 1, "{sql}");
        let column = &result.output.columns[0];
        assert_eq!(column.transform, expected, "{sql}");
        assert_eq!(
            column.lineage,
            super::support::complete(
                tables
                    .into_iter()
                    .map(|table| source(
                        relation(table),
                        u(if table == "a" { "value" } else { "id" })
                    ))
                    .collect()
            ),
            "{sql}"
        );
    }
}
