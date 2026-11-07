use dlin_sqllineage::{
    Diagnostic, DiagnosticKind, Dialect, Lineage, Severity, Shape, UnexpandedStar, Unresolved,
};

use super::support::{Case, col, duplicate_name, q, relation, source, u};

pub fn cases() -> Vec<Case> {
    vec![
        Case::new(
            "insert_target_columns_rename_catalog_expanded_star",
            "INSERT INTO target (a, b) SELECT * FROM s",
            Dialect::Generic,
            vec![
                col(0, u("a"), vec![source(relation("s"), u("x"))]),
                col(1, u("b"), vec![source(relation("s"), u("y"))]),
            ],
        )
        .catalog(vec![(relation("s"), vec![u("x"), u("y")])]),
        Case::new(
            "insert_target_columns_preserve_union_branch_sources",
            "INSERT INTO target (a) SELECT x FROM s UNION ALL SELECT y FROM r",
            Dialect::Generic,
            vec![col(
                0,
                u("a"),
                vec![source(relation("s"), u("x")), source(relation("r"), u("y"))],
            )],
        ),
        Case::new(
            "insert_target_columns_rename_select_outputs",
            "INSERT INTO target (target_x, target_total) SELECT x, y + 1 AS total FROM source",
            Dialect::Generic,
            vec![
                col(0, u("target_x"), vec![source(relation("source"), u("x"))]),
                col(
                    1,
                    u("target_total"),
                    vec![source(relation("source"), u("y"))],
                ),
            ],
        ),
        Case::new(
            "insert_target_columns_rename_values",
            "INSERT INTO target (first_value, second_value) VALUES (1, 2)",
            Dialect::Generic,
            vec![
                col(0, u("first_value"), vec![]),
                col(1, u("second_value"), vec![]),
            ],
        ),
        Case::new(
            "insert_target_values_merge_sources_across_rows",
            "INSERT INTO target (picked) VALUES ((SELECT x FROM s)), ((SELECT y FROM s))",
            Dialect::Generic,
            vec![col(
                0,
                u("picked"),
                vec![source(relation("s"), u("x")), source(relation("s"), u("y"))],
            )],
        ),
        Case::new(
            "insert_values_without_target_columns_keeps_empty_output",
            "INSERT INTO target VALUES (1, 2)",
            Dialect::Generic,
            vec![],
        ),
        Case::new(
            "insert_target_columns_preserve_quoted_identifiers",
            "INSERT INTO target (\"MiXeD\", \"second\") SELECT x, y FROM source",
            Dialect::Generic,
            vec![
                col(0, q("MiXeD"), vec![source(relation("source"), u("x"))]),
                col(1, q("second"), vec![source(relation("source"), u("y"))]),
            ],
        ),
        Case::new(
            "ctas_target_columns_rename_select_outputs",
            "CREATE TABLE target (target_x INT, target_y INT) AS SELECT x, y FROM source",
            Dialect::Generic,
            vec![
                col(0, u("target_x"), vec![source(relation("source"), u("x"))]),
                col(1, u("target_y"), vec![source(relation("source"), u("y"))]),
            ],
        ),
        Case::new(
            "insert_target_column_count_mismatch_keeps_source_names",
            "INSERT INTO target (target_x, target_y, target_z) SELECT x, y FROM source",
            Dialect::Generic,
            vec![
                col(0, u("x"), vec![source(relation("source"), u("x"))]),
                col(1, u("y"), vec![source(relation("source"), u("y"))]),
            ],
        )
        .diagnostics(vec![Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::TargetColumnCountMismatch {
                target: 3,
                source: 2,
            },
            location: None,
        }]),
        Case::new(
            "insert_unknown_star_keeps_known_source_name",
            "INSERT INTO target (target_x, target_y) SELECT x, s.* FROM s",
            Dialect::Generic,
            vec![col(0, u("x"), vec![source(relation("s"), u("x"))])],
        )
        .shape(Shape::Open {
            unexpanded: vec![UnexpandedStar {
                relation: Some(relation("s")),
                after_ordinal: Some(0),
                lineage: Lineage::Partial {
                    known: vec![],
                    unresolved: vec![Unresolved::BehindStar {
                        relation: relation("s"),
                    }],
                },
            }],
        })
        .unresolved_stars(true),
        Case::new(
            "insert_without_target_columns_keeps_select_names",
            "INSERT INTO target SELECT x AS source_x, y FROM source",
            Dialect::Generic,
            vec![
                col(0, u("source_x"), vec![source(relation("source"), u("x"))]),
                col(1, u("y"), vec![source(relation("source"), u("y"))]),
            ],
        ),
        Case::new(
            "ctas_without_target_columns_keeps_select_names",
            "CREATE TABLE target AS SELECT x AS source_x, y FROM source",
            Dialect::Generic,
            vec![
                col(0, u("source_x"), vec![source(relation("source"), u("x"))]),
                col(1, u("y"), vec![source(relation("source"), u("y"))]),
            ],
        ),
        Case::new(
            "insert_target_rename_can_create_duplicate_names",
            "INSERT INTO target (same, same) SELECT x, y FROM source",
            Dialect::Generic,
            vec![
                col(0, u("same"), vec![source(relation("source"), u("x"))]),
                col(1, u("same"), vec![source(relation("source"), u("y"))]),
            ],
        )
        .diagnostics(vec![duplicate_name(u("same"), vec![0, 1])]),
        Case::new(
            "insert_target_rename_resolves_existing_duplicate_names",
            "INSERT INTO target (left_value, right_value) SELECT x AS same, y AS same FROM source",
            Dialect::Generic,
            vec![
                col(0, u("left_value"), vec![source(relation("source"), u("x"))]),
                col(
                    1,
                    u("right_value"),
                    vec![source(relation("source"), u("y"))],
                ),
            ],
        ),
    ]
}
