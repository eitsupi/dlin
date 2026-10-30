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
            ])
            .merged_star_failure(
                vec![
                    physical(0, "a", "id"),
                    physical(1, "a", "left_value"),
                    physical(2, "b", "id"),
                    physical(3, "b", "right_value"),
                ],
                vec![duplicate_name(u("id"), vec![0, 2])],
                "Unqualified stars duplicate USING/NATURAL merged columns instead of expanding one merged output.",
            ),
        );
    }
    cases
}
