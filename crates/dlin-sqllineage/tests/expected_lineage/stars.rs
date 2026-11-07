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
    // USING guarantees that `b` has `id`, so the merged key also comes from
    // `b`. NATURAL cannot name a shared column without knowing `b`.
    for (name, join, id) in [
        (
            "partial_catalog_using_star",
            "JOIN b USING (id)",
            col(
                1,
                u("id"),
                vec![
                    source(relation("a"), u("id")),
                    source(relation("b"), u("id")),
                ],
            ),
        ),
        (
            "partial_catalog_natural_star",
            "NATURAL JOIN b",
            physical(1, "a", "id"),
        ),
    ] {
        cases.push(
            Case::new(
                name,
                &format!("SELECT * FROM a {join}"),
                Dialect::Generic,
                vec![physical(0, "a", "left_value"), id],
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
    for (name, from, excluded, denied) in [
        (
            "unknown_exclude_derived_range_variable",
            "(SELECT * FROM a) x",
            "x.id",
            true,
        ),
        (
            "unknown_exclude_hidden_physical_name",
            "a AS x",
            "a.id",
            false,
        ),
        ("unknown_exclude_implicit_range_variable", "a", "a.id", true),
    ] {
        let unresolved = if denied {
            Unresolved::Unbound { column: u("id") }
        } else {
            Unresolved::NamedBehindStar {
                relation: relation("a"),
                column: u("id"),
            }
        };
        cases.push(
            Case::new(
                name,
                &format!("SELECT id FROM (SELECT * EXCLUDE ({excluded}) FROM {from}) d"),
                Dialect::Generic,
                vec![partial_col(0, u("id"), vec![], vec![unresolved.clone()])],
            )
            .unresolved_stars(true),
        );
        let mut columns = Vec::new();
        if !denied {
            let mut renamed = partial_col(0, u("kept"), vec![], vec![unresolved]);
            renamed.ordinal = None;
            columns.push(renamed);
        }
        cases.push(
            Case::new(
                &format!("{name}_rename"),
                &format!("SELECT * EXCLUDE ({excluded}) RENAME (id AS kept) FROM {from}"),
                Dialect::Generic,
                columns,
            )
            .shape(open_star("a", None))
            .unresolved_stars(true),
        );
    }
    for (name, join) in [
        ("inner", "JOIN"),
        ("left", "LEFT JOIN"),
        ("right", "RIGHT JOIN"),
        ("full", "FULL JOIN"),
    ] {
        for qualifier in ["x", "y"] {
            cases.push(Case::new(&format!("unknown_using_{name}_exclude_{qualifier}"),
                &format!("SELECT id FROM (SELECT * EXCLUDE ({qualifier}.id) FROM a x {join} b y USING (id)) d"),
                Dialect::Generic,
                vec![partial_col(0, u("id"), vec![], vec![Unresolved::Unbound { column: u("id") }])],
            ).unresolved_stars(true));
            cases.push(Case::new(&format!("unknown_using_{name}_exclude_{qualifier}_rename"),
                &format!("SELECT * EXCLUDE ({qualifier}.id) RENAME (id AS kept) FROM a x {join} b y USING (id)"),
                Dialect::Generic, vec![],
            ).shape(Shape::Open { unexpanded: ["a", "b"].into_iter().map(|table| UnexpandedStar {
                relation: Some(relation(table)), after_ordinal: None,
                lineage: Lineage::Partial { known: vec![], unresolved: vec![Unresolved::BehindStar { relation: relation(table) }] },
            }).collect() }).unresolved_stars(true));
        }
    }
    let mut other = physical(0, "c", "other");
    other.ordinal = None;
    cases.push(Case::new("unknown_using_chain_skips_known_non_owner",
        "SELECT * EXCLUDE (x.id) RENAME (id AS kept) FROM a x JOIN b y USING (id) JOIN c z ON TRUE JOIN d w USING (id)",
        Dialect::Generic, vec![other],
    ).catalog(vec![(relation("c"), vec![u("other")])])
     .shape(Shape::Open { unexpanded: ["a", "b", "d"].into_iter().map(|table| UnexpandedStar {
        relation: Some(relation(table)), after_ordinal: None,
        lineage: Lineage::Partial { known: vec![], unresolved: vec![Unresolved::BehindStar { relation: relation(table) }] },
     }).collect() }).unresolved_stars(true));
    cases.push(Case::new("unknown_using_chain_named_read_skips_known_non_owner",
        "SELECT id FROM (SELECT * EXCLUDE (x.id) FROM a x JOIN b y USING (id) JOIN c z ON TRUE JOIN d w USING (id)) result",
        Dialect::Generic, vec![partial_col(0, u("id"), vec![], vec![Unresolved::Unbound { column: u("id") }])],
    ).catalog(vec![(relation("c"), vec![u("other")])]).unresolved_stars(true));
    for qualifier in ["x", "y"] {
        cases.push(Case::new(&format!("partial_catalog_using_exclude_{qualifier}"),
            &format!("SELECT id FROM (SELECT * EXCLUDE ({qualifier}.id) FROM a AS x JOIN b AS y USING (id)) AS d"),
            Dialect::Generic,
            vec![partial_col(0, u("id"), vec![], vec![Unresolved::Unbound { column: u("id") }])],
        ).catalog(vec![(relation("a"), vec![u("id"), u("left_value")])]).unresolved_stars(true));
    }
    cases.push(Case::new("partial_catalog_cross_exclude_known_column",
        "SELECT left_value FROM (SELECT * EXCLUDE (x.left_value) FROM a AS x CROSS JOIN b AS y) AS d",
        Dialect::Generic,
        vec![partial_col(0, u("left_value"), vec![], vec![Unresolved::NamedBehindStar { relation: relation("b"), column: u("left_value") }])],
    ).catalog(vec![(relation("a"), vec![u("id"), u("left_value")])]).unresolved_stars(true));
    cases.push(
        Case::new(
            "partial_catalog_ilike_filters_known_column",
            "SELECT left_value FROM (SELECT * ILIKE 'id' FROM a AS x CROSS JOIN b AS y) AS d",
            Dialect::Snowflake,
            vec![partial_col(
                0,
                u("left_value"),
                vec![],
                vec![Unresolved::Unbound {
                    column: u("left_value"),
                }],
            )],
        )
        .catalog(vec![(relation("a"), vec![u("id"), u("left_value")])])
        .unresolved_stars(true),
    );
    for modifier in ["REPLACE (1 AS id)", "RENAME (id AS kept)"] {
        cases.push(
            Case::new(
                &format!("partial_catalog_exclude_prevents_{modifier}"),
                &format!("SELECT * EXCLUDE (y.id) {modifier} FROM a AS x JOIN b AS y USING (id)"),
                Dialect::Generic,
                vec![physical(0, "a", "left_value")],
            )
            .catalog(vec![(relation("a"), vec![u("id"), u("left_value")])])
            .shape(open_star("b", Some(0)))
            .unresolved_stars(true),
        );
    }
    cases.extend(partial_catalog_modifier_cases());
    cases.extend(unknown_using_key_cases());
    cases.extend(set_operation_tail_cases());
    cases
}

fn behind(table: &str, column: &str) -> Unresolved {
    Unresolved::NamedBehindStar {
        relation: relation(table),
        column: u(column),
    }
}

/// `a` is known, `b` has no catalog entry. Modifiers apply to the known
/// columns of `a`; a column that only `b` may publish becomes an explicit
/// column whose position among the unknown columns is not known.
fn partial_catalog_modifier_cases() -> Vec<Case> {
    let a_columns = vec![(relation("a"), vec![u("id"), u("left_value")])];
    let from = "FROM a CROSS JOIN b";
    let mut explicit_flag = col(0, u("flag"), vec![]);
    explicit_flag.ordinal = None;
    let mut renamed_flag = partial_col(0, u("kept"), vec![], vec![behind("b", "flag")]);
    renamed_flag.ordinal = None;
    vec![
        // REPLACE keeps the position and name of the known column and only
        // changes where its value comes from.
        Case::new(
            "partial_catalog_replace_known_column",
            &format!("SELECT * REPLACE (a.left_value || 'x' AS left_value) {from}"),
            Dialect::Generic,
            vec![
                physical(0, "a", "id"),
                col(
                    1,
                    u("left_value"),
                    vec![source(relation("a"), u("left_value"))],
                ),
            ],
        )
        .catalog(a_columns.clone())
        .shape(open_star("b", Some(1)))
        .unresolved_stars(true),
        // `a` has no `flag`, so only `b` can have the column being replaced.
        Case::new(
            "partial_catalog_replace_column_only_unknown_side_publishes",
            &format!("SELECT * REPLACE (1 AS flag) {from}"),
            Dialect::Generic,
            vec![
                physical(0, "a", "id"),
                physical(1, "a", "left_value"),
                explicit_flag,
            ],
        )
        .catalog(a_columns.clone())
        .shape(open_star("b", Some(1)))
        .unresolved_stars(true),
        Case::new(
            "partial_catalog_rename_known_column",
            &format!("SELECT * RENAME (left_value AS renamed) {from}"),
            Dialect::Generic,
            vec![
                physical(0, "a", "id"),
                col(
                    1,
                    u("renamed"),
                    vec![source(relation("a"), u("left_value"))],
                ),
            ],
        )
        .catalog(a_columns.clone())
        .shape(open_star("b", Some(1)))
        .unresolved_stars(true),
        Case::new(
            "partial_catalog_rename_column_only_unknown_side_publishes",
            &format!("SELECT * RENAME (flag AS kept) {from}"),
            Dialect::Generic,
            vec![
                physical(0, "a", "id"),
                physical(1, "a", "left_value"),
                renamed_flag,
            ],
        )
        .catalog(a_columns.clone())
        .shape(open_star("b", Some(1)))
        .unresolved_stars(true),
        // The same results are read back through a derived table.
        Case::new(
            "partial_catalog_replaced_known_column_read_through_derived_table",
            &format!("SELECT left_value FROM (SELECT * REPLACE (a.id AS left_value) {from}) d"),
            Dialect::Generic,
            vec![col(
                0,
                u("left_value"),
                vec![source(relation("a"), u("id"))],
            )],
        )
        .catalog(a_columns.clone())
        .unresolved_stars(true),
        Case::new(
            "partial_catalog_renamed_known_column_read_through_derived_table",
            &format!("SELECT renamed FROM (SELECT * RENAME (left_value AS renamed) {from}) d"),
            Dialect::Generic,
            vec![col(
                0,
                u("renamed"),
                vec![source(relation("a"), u("left_value"))],
            )],
        )
        .catalog(a_columns.clone())
        .unresolved_stars(true),
        Case::new(
            "partial_catalog_renamed_unknown_column_read_through_derived_table",
            &format!("SELECT kept FROM (SELECT * RENAME (flag AS kept) {from}) d"),
            Dialect::Generic,
            vec![partial_col(0, u("kept"), vec![], vec![behind("b", "flag")])],
        )
        .catalog(a_columns.clone())
        .unresolved_stars(true),
        // `a` renamed its `left_value`, but `b` may still have a column of
        // that name.
        Case::new(
            "partial_catalog_renamed_known_name_may_come_from_unknown_side",
            &format!("SELECT left_value FROM (SELECT * RENAME (left_value AS renamed) {from}) d"),
            Dialect::Generic,
            vec![partial_col(
                0,
                u("left_value"),
                vec![],
                vec![behind("b", "left_value")],
            )],
        )
        .catalog(a_columns.clone())
        .unresolved_stars(true),
        // Only `b` could have had `flag`, and it is now called `kept`.
        Case::new(
            "partial_catalog_renamed_unknown_name_is_not_read_back",
            &format!("SELECT flag FROM (SELECT * RENAME (flag AS kept) {from}) d"),
            Dialect::Generic,
            vec![partial_col(
                0,
                u("flag"),
                vec![],
                vec![Unresolved::Unbound { column: u("flag") }],
            )],
        )
        .catalog(a_columns)
        .unresolved_stars(true),
    ]
}

/// The left operand of the set operation is `t1.*` with unknown columns and
/// the right one lists `a, b`. Names and positions of the unknown columns are
/// not known, so the right operand's columns stay reachable by name, each
/// with the right branch's source and the left operand's unresolved star.
/// A star over the set operation must not lose them.
fn set_operation_tail_cases() -> Vec<Case> {
    let set_operation = "SELECT * FROM t1 UNION ALL SELECT a, b FROM t2";
    let right_branch = |column: &str| {
        partial_col(
            0,
            u(column),
            vec![source(relation("t2"), u(column))],
            vec![Unresolved::BehindStar {
                relation: relation("t1"),
            }],
        )
    };
    let unpositioned = |column: &str| {
        let mut expected = right_branch(column);
        expected.ordinal = None;
        expected
    };
    vec![
        Case::new(
            "set_operation_right_column_read_directly",
            &format!("SELECT b FROM ({set_operation}) s"),
            Dialect::Generic,
            vec![right_branch("b")],
        )
        .unresolved_stars(true),
        Case::new(
            "set_operation_right_column_read_through_star",
            &format!("SELECT b FROM (SELECT * FROM ({set_operation}) s) d"),
            Dialect::Generic,
            vec![right_branch("b")],
        )
        .unresolved_stars(true),
        Case::new(
            "set_operation_right_columns_expanded_by_star",
            &format!("SELECT * FROM (SELECT * FROM ({set_operation}) s) d"),
            Dialect::Generic,
            vec![unpositioned("a"), unpositioned("b")],
        )
        .shape(open_star("t1", None))
        .unresolved_stars(true),
        Case::new(
            "set_operation_right_columns_respect_star_exclude",
            &format!("SELECT * EXCLUDE (b) FROM ({set_operation}) s"),
            Dialect::Generic,
            vec![unpositioned("a")],
        )
        .shape(open_star("t1", None))
        .unresolved_stars(true),
    ]
}

const JOIN_KINDS: [(&str, &str); 4] = [
    ("inner", "JOIN"),
    ("left", "LEFT JOIN"),
    ("right", "RIGHT JOIN"),
    ("full", "FULL JOIN"),
];

/// Where the merged `USING (id)` key of `a <join> b` gets its value: both
/// sides for INNER and FULL, the preserved side for LEFT and RIGHT.
fn using_key_sources(kind: &str) -> Vec<dlin_sqllineage::SourceColumn> {
    let a = source(relation("a"), u("id"));
    let b = source(relation("b"), u("id"));
    match kind {
        "left" => vec![a],
        "right" => vec![b],
        _ => vec![a, b],
    }
}

/// The catalogs `USING` is checked under: none, only `a`, only `b`.
fn using_catalogs() -> Vec<(
    &'static str,
    Vec<(dlin_sqllineage::Relation, Vec<dlin_sqllineage::Ident>)>,
)> {
    vec![
        ("no_catalog", vec![]),
        (
            "known_left",
            vec![(relation("a"), vec![u("id"), u("left_value")])],
        ),
        (
            "known_right",
            vec![(relation("b"), vec![u("id"), u("right_value")])],
        ),
    ]
}

/// `USING (id)` guarantees that every joined relation has `id`, so a relation
/// without catalog columns still supplies the key as a column of its own. The
/// join kind decides which sides supply the merged value. A key read through a
/// star must resolve exactly like a direct reference to it.
fn unknown_using_key_cases() -> Vec<Case> {
    let mut cases = Vec::new();
    for (catalog_name, catalog) in using_catalogs() {
        for (name, join) in JOIN_KINDS {
            let key = || col(0, u("id"), using_key_sources(name));
            cases.push(
                Case::new(
                    &format!("using_{name}_key_direct_reference_{catalog_name}"),
                    &format!("SELECT id FROM a {join} b USING (id)"),
                    Dialect::Generic,
                    vec![key()],
                )
                .catalog(catalog.clone()),
            );
            cases.push(
                Case::new(
                    &format!("using_{name}_key_read_through_star_{catalog_name}"),
                    &format!("SELECT id FROM (SELECT * FROM a {join} b USING (id)) d"),
                    Dialect::Generic,
                    vec![key()],
                )
                .catalog(catalog.clone())
                .unresolved_stars(true),
            );
        }
    }
    // `a` is known and `b` is not: the key stays where `a` lists it.
    for (name, join) in JOIN_KINDS {
        if name == "inner" {
            continue; // Covered by partial_catalog_using_star.
        }
        cases.push(
            Case::new(
                &format!("known_left_unknown_right_using_{name}_star"),
                &format!("SELECT * FROM a {join} b USING (id)"),
                Dialect::Generic,
                vec![
                    col(0, u("id"), using_key_sources(name)),
                    physical(1, "a", "left_value"),
                ],
            )
            .catalog(vec![(relation("a"), vec![u("id"), u("left_value")])])
            .shape(open_star("b", Some(1)))
            .unresolved_stars(true),
        );
    }
    // A CTE side that publishes `id` only behind its own star supplies the
    // column the way a reference to `id` in that CTE finds it.
    for (name, join) in JOIN_KINDS {
        let behind_t = Unresolved::NamedBehindStar {
            relation: relation("t"),
            column: u("id"),
        };
        let b = source(relation("b"), u("id"));
        let key = |index| match name {
            "left" => partial_col(index, u("id"), vec![], vec![behind_t.clone()]),
            "right" => col(index, u("id"), vec![b.clone()]),
            _ => partial_col(index, u("id"), vec![b.clone()], vec![behind_t.clone()]),
        };
        cases.push(
            Case::new(
                &format!("using_{name}_key_unknown_cte_read_through_star"),
                &format!(
                    "WITH c AS (SELECT * FROM t) SELECT id FROM (SELECT * FROM c {join} b USING (id)) d"
                ),
                Dialect::Generic,
                vec![key(0)],
            )
            .unresolved_stars(true),
        );
    }
    cases
}

#[test]
fn using_key_read_through_star_equals_direct_reference() {
    for (catalog_name, catalog) in using_catalogs() {
        for (name, join) in JOIN_KINDS {
            let case = Case::new("comparison", "SELECT 1", Dialect::Generic, vec![])
                .catalog(catalog.clone());
            let direct = case
                .analyze_sql(&format!("SELECT id FROM a {join} b USING (id)"), true)
                .unwrap();
            let through_star = case
                .analyze_sql(
                    &format!("SELECT id FROM (SELECT * FROM a {join} b USING (id)) d"),
                    true,
                )
                .unwrap();
            assert_eq!(
                direct.output.columns[0].lineage, through_star.output.columns[0].lineage,
                "{catalog_name} {name}"
            );
        }
    }
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
