use dlin_sqllineage::*;

fn one(sql: &str) -> StatementAnalysis {
    Analyzer::new(Dialect::Generic)
        .analyze(sql)
        .expect("should parse")
        .into_single()
        .expect("one statement")
}

fn source(relation: &str, column: &str) -> SourceColumn {
    SourceColumn {
        relation: Relation {
            catalog: None,
            schema: None,
            name: Ident::unquoted(relation),
        },
        column: Ident::unquoted(column),
    }
}

#[test]
fn insert_target_columns_rename_outputs_and_preserve_lineage() {
    let result =
        one("INSERT INTO target (target_x, target_total) SELECT x, y + 1 AS total FROM source");
    assert_eq!(result.kind, StatementKind::Insert);
    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("target_x"))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![source("source", "x")])
    );
    assert_eq!(result.output.columns[0].transform, Transform::Direct);
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("target_total"))
    );
    assert_eq!(
        result.output.columns[1].lineage,
        Lineage::Complete(vec![source("source", "y")])
    );
    assert_eq!(result.output.columns[1].transform, Transform::Expression);
    assert!(matches!(
        result.output.lookup("target_total", Dialect::Generic),
        Lookup::Found(column) if column.ordinal == Some(1)
    ));
    assert_eq!(
        result.output.lookup("total", Dialect::Generic),
        Lookup::Absent {
            may_hide_behind_star: false
        }
    );
}

#[test]
fn insert_target_columns_rename_values_and_keep_source_free_transform() {
    let result = one("INSERT INTO target (first_value, second_value) VALUES (1, 2)");
    assert_eq!(result.output.columns.len(), 2);
    for (column, name) in result
        .output
        .columns
        .iter()
        .zip(["first_value", "second_value"])
    {
        assert_eq!(column.name, OutputName::Explicit(Ident::unquoted(name)));
        assert_eq!(column.lineage, Lineage::Complete(vec![]));
        assert_eq!(column.transform, Transform::Direct);
    }
}

#[test]
fn insert_values_merge_sources_from_each_row_by_position() {
    let result = one("INSERT INTO target (picked) VALUES ((SELECT x FROM s)), ((SELECT y FROM s))");
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("picked"))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![source("s", "x"), source("s", "y")])
    );
}

#[test]
fn ctas_target_columns_rename_outputs() {
    let result = one("CREATE TABLE target (target_x INT, target_y INT) AS SELECT x, y FROM source");
    assert_eq!(result.kind, StatementKind::CreateTableAs);
    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("target_x"))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![source("source", "x")])
    );
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("target_y"))
    );
    assert_eq!(
        result.output.columns[1].lineage,
        Lineage::Complete(vec![source("source", "y")])
    );
}

#[test]
fn insert_target_columns_preserve_quoted_identifiers() {
    let result = one("INSERT INTO target (\"MiXeD\", \"second\") SELECT x, y FROM source");
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::quoted("MiXeD", Quote::Double))
    );
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::quoted("second", Quote::Double))
    );
}

#[test]
fn target_column_count_mismatch_keeps_source_names_and_reports_error() {
    for sql in [
        "INSERT INTO target (target_x, target_y, target_z) SELECT x, y FROM source",
        "CREATE TABLE target (target_x INT, target_y INT, target_z INT) AS SELECT x, y FROM source",
    ] {
        let result = one(sql);
        assert_eq!(result.output.columns.len(), 2, "{sql}");
        assert_eq!(
            result.output.columns[0].name,
            OutputName::Explicit(Ident::unquoted("x")),
            "{sql}"
        );
        assert_eq!(
            result.output.columns[1].name,
            OutputName::Explicit(Ident::unquoted("y")),
            "{sql}"
        );
        assert_eq!(
            result.diagnostics,
            [Diagnostic {
                severity: Severity::Error,
                kind: DiagnosticKind::TargetColumnCountMismatch {
                    target: 3,
                    source: 2,
                },
                location: None,
            }],
            "{sql}"
        );
    }
}

#[test]
fn unknown_star_keeps_old_source_name_when_target_width_is_unknown() {
    let result = one("INSERT INTO target (target_x, target_y) SELECT x, s.* FROM s");
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("x"))
    );
    assert!(result.has_unresolved_stars);
    assert!(matches!(result.output.shape, Shape::Open { .. }));
    assert!(result.diagnostics.is_empty());
}

#[test]
fn target_column_renaming_updates_duplicate_detection_and_lookup() {
    let duplicate = one("INSERT INTO target (same, same) SELECT x, y FROM source");
    assert!(matches!(
        duplicate.output.lookup("same", Dialect::Generic),
        Lookup::Duplicate(columns) if columns.len() == 2
    ));
    assert_eq!(
        duplicate.diagnostics,
        [Diagnostic {
            severity: Severity::Warning,
            kind: DiagnosticKind::DuplicateOutputName {
                name: Ident::unquoted("same"),
                ordinals: vec![0, 1],
            },
            location: None,
        }]
    );

    let resolved =
        one("INSERT INTO target (left_value, right_value) SELECT x AS same, y AS same FROM source");
    assert!(resolved.diagnostics.is_empty());
    assert!(matches!(
        resolved.output.lookup("left_value", Dialect::Generic),
        Lookup::Found(column) if column.ordinal == Some(0)
    ));
    assert!(matches!(
        resolved.output.lookup("right_value", Dialect::Generic),
        Lookup::Found(column) if column.ordinal == Some(1)
    ));
    assert_eq!(
        resolved.output.lookup("same", Dialect::Generic),
        Lookup::Absent {
            may_hide_behind_star: false
        }
    );
}

#[test]
fn target_column_lists_absent_preserves_query_names_and_values_behavior() {
    let insert = one("INSERT INTO target SELECT x AS source_x, y FROM source");
    assert_eq!(
        insert.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("source_x"))
    );
    assert_eq!(
        insert.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("y"))
    );

    let ctas = one("CREATE TABLE target AS SELECT x AS source_x, y FROM source");
    assert_eq!(
        ctas.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("source_x"))
    );
    assert_eq!(
        ctas.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("y"))
    );

    let values = one("INSERT INTO target VALUES (1, 2)");
    assert!(values.output.columns.is_empty());
    assert!(values.diagnostics.is_empty());
}

#[test]
fn uneven_values_rows_keep_legacy_output_without_target_diagnostics() {
    for sql in [
        "INSERT INTO target (a) VALUES (1), (2, 3)",
        "INSERT INTO target (a, b) VALUES (1, 2), (3)",
    ] {
        let result = one(sql);
        assert_eq!(
            result.output,
            Output {
                columns: vec![],
                shape: Shape::Closed
            },
            "{sql}"
        );
        assert!(
            result.diagnostics.is_empty(),
            "{sql}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn unmodeled_table_source_is_not_a_known_zero_width() {
    let result = one("INSERT INTO target (a) TABLE source");
    assert!(result.output.columns.is_empty());
    assert!(result.diagnostics.is_empty());
}

#[test]
fn values_keep_source_free_and_mixed_row_expression_transforms() {
    for sql in [
        "INSERT INTO target (a) VALUES (1 + 2)",
        "INSERT INTO target (a) VALUES ((SELECT x FROM s)), (1 + 2)",
    ] {
        let result = one(sql);
        assert_eq!(
            result.output.columns[0].transform,
            Transform::Expression,
            "{sql}"
        );
    }
}

#[test]
fn explicit_projection_width_is_known_over_an_unmodeled_cte_source() {
    let result = one("INSERT INTO target (a) WITH c AS (TABLE public.source) SELECT 1 AS x FROM c");
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("a"))
    );
    assert!(result.diagnostics.is_empty());
}

#[test]
fn values_keep_source_free_scalar_aggregate_transform() {
    let result = one("INSERT INTO target (a) VALUES ((SELECT COUNT(*) FROM s))");
    assert_eq!(result.output.columns[0].transform, Transform::Aggregation);
    assert_eq!(result.output.columns[0].lineage, Lineage::Complete(vec![]));
}

#[test]
fn nested_values_lineage_does_not_depend_on_insert_column_list() {
    for select in [
        "SELECT v.c FROM (VALUES (1)) v(c)",
        "SELECT x FROM s WHERE x IN (VALUES (1))",
        "SELECT (VALUES ((SELECT x FROM s))) AS value",
    ] {
        let named = one(&format!("INSERT INTO target (a) {select}"));
        let unnamed = one(&format!("INSERT INTO target {select}"));
        let mut normalized = named.clone();
        for (column, original) in normalized
            .output
            .columns
            .iter_mut()
            .zip(&unnamed.output.columns)
        {
            column.name.clone_from(&original.name);
        }
        assert_eq!(normalized, unnamed, "{select}");
    }
}

#[test]
fn target_values_still_follow_top_level_set_operation_branches() {
    for sql in [
        "INSERT INTO target (a) VALUES (1) UNION ALL VALUES (2)",
        "INSERT INTO target (a) (VALUES (1))",
    ] {
        let result = one(sql);
        assert_eq!(result.output.columns.len(), 1, "{sql}");
        assert_eq!(
            result.output.columns[0].name,
            OutputName::Explicit(Ident::unquoted("a")),
            "{sql}"
        );
        assert_eq!(
            result.output.columns[0].lineage,
            Lineage::Complete(vec![]),
            "{sql}"
        );
        assert!(
            result.diagnostics.is_empty(),
            "{sql}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn ordinary_set_operations_do_not_treat_unmodeled_branches_as_zero_width() {
    let failures = [
        "SELECT 1 UNION ALL TABLE t",
        "SELECT 1 UNION ALL VALUES (2)",
    ]
    .into_iter()
    .filter_map(|sql| {
        let result = one(sql);
        (!result.diagnostics.is_empty()).then(|| format!("{sql}: {:?}", result.diagnostics))
    })
    .collect::<Vec<_>>();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn unknown_set_operation_branches_prevent_target_renaming_and_diagnostics() {
    for prefix in [
        "",
        "INSERT INTO target ",
        "INSERT INTO target (a) ",
        "INSERT INTO target (a, b) ",
    ] {
        for branch in ["TABLE other_source", "VALUES (1)", "VALUES (1), (2, 3)"] {
            if prefix.contains("(a") && branch == "VALUES (1)" {
                continue; // A uniform target-body VALUES branch is modeled.
            }
            let sql = format!("{prefix}SELECT x FROM s UNION ALL {branch}");
            let result = one(&sql);
            assert!(
                result.diagnostics.is_empty(),
                "{sql}: {:?}",
                result.diagnostics
            );
            assert_eq!(result.output.shape, Shape::Closed, "{sql}");
            let mut expected_read = vec![source("s", "x").relation];
            if branch.starts_with("TABLE") {
                expected_read.insert(0, source("other_source", "x").relation);
            }
            assert_eq!(result.read, expected_read, "{sql}");
            assert_eq!(result.output.columns.len(), 1, "{sql}");
            assert_eq!(
                result.output.columns[0].name,
                OutputName::Explicit(Ident::unquoted("x")),
                "{sql}"
            );
            assert_eq!(
                result.output.columns[0].lineage,
                Lineage::Partial {
                    known: vec![source("s", "x")],
                    unresolved: vec![Unresolved::Unsupported {
                        what: if branch.starts_with("TABLE") {
                            "TABLE source"
                        } else {
                            "VALUES source"
                        },
                    }],
                },
                "{sql}"
            );
        }
    }
}

#[test]
fn unknown_width_follows_output_plans_and_derived_stars() {
    for sql in [
        "INSERT INTO target (a, b) WITH c AS (SELECT 1) SELECT x FROM s UNION ALL TABLE public.other_source",
        "INSERT INTO target (a, b) (SELECT x FROM s UNION ALL TABLE public.other_source)",
        "INSERT INTO target (a) SELECT * FROM (TABLE public.other_source) v",
    ] {
        let result = one(sql);
        assert!(
            result.diagnostics.is_empty(),
            "{sql}: {:?}",
            result.diagnostics
        );
        assert!(
            result
                .output
                .columns
                .iter()
                .all(|column| column.name != OutputName::Explicit(Ident::unquoted("a"))),
            "{sql}"
        );
    }
}

#[test]
fn nested_unknown_cte_does_not_hide_a_known_projection_width_mismatch() {
    let result =
        one("INSERT INTO target (a, b) WITH c AS (TABLE public.source) SELECT 1 AS x FROM c");
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("x"))
    );
    assert_eq!(
        result.diagnostics,
        [Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::TargetColumnCountMismatch {
                target: 2,
                source: 1
            },
            location: None
        }]
    );
}

#[test]
fn unknown_branch_lineage_propagates_through_delegates_and_nested_sets() {
    for sql in [
        "WITH c AS (SELECT 1) SELECT x FROM s UNION ALL TABLE public.other_source",
        "SELECT x FROM s UNION ALL (SELECT x FROM s UNION ALL TABLE public.other_source)",
        "SELECT x FROM s UNION ALL (WITH c AS (SELECT 1) SELECT x FROM s UNION ALL VALUES (1))",
        "SELECT q.x FROM (SELECT x FROM s UNION ALL TABLE public.other_source) q",
        "WITH q AS (SELECT x FROM s UNION ALL TABLE public.other_source) SELECT x FROM q",
        "WITH q AS (TABLE public.other_source UNION ALL SELECT x FROM s) SELECT x FROM q",
        "SELECT x, y FROM s UNION ALL TABLE public.other_source",
    ] {
        let result = one(sql);
        assert!(
            result.diagnostics.is_empty(),
            "{sql}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.output.shape, Shape::Closed, "{sql}");
        let what = if sql.contains("TABLE") {
            "TABLE source"
        } else {
            "VALUES source"
        };
        for (index, column) in result.output.columns.iter().enumerate() {
            assert_eq!(
                column.lineage,
                Lineage::Partial {
                    known: vec![source("s", if index == 0 { "x" } else { "y" })],
                    unresolved: vec![Unresolved::Unsupported { what }],
                },
                "{sql}"
            );
        }
        assert!(!result.output.columns.is_empty(), "{sql}");
    }
}

#[test]
fn table_source_registers_read_without_fabricating_columns() {
    let result = one("INSERT INTO target TABLE public.other_source");
    assert_eq!(
        result.read,
        vec![Relation {
            catalog: None,
            schema: Some(Ident::unquoted("public")),
            name: Ident::unquoted("other_source"),
        }]
    );
    assert!(result.output.columns.is_empty());
    assert_eq!(result.output.shape, Shape::Closed);
    assert!(result.diagnostics.is_empty());
}

#[test]
fn nested_unknown_branch_kinds_are_retained_without_tainting_unrelated_values() {
    let result = one("SELECT x FROM s UNION ALL (TABLE public.t UNION ALL VALUES (1))");
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![source("s", "x")],
            unresolved: vec![
                Unresolved::Unsupported {
                    what: "TABLE source"
                },
                Unresolved::Unsupported {
                    what: "VALUES source"
                },
            ],
        }
    );
    let result = one("WITH q AS (SELECT x FROM s UNION ALL TABLE public.t) SELECT 1 FROM q");
    assert_eq!(result.output.columns[0].lineage, Lineage::Complete(vec![]));
}

#[test]
fn table_cte_source_does_not_fabricate_a_physical_read() {
    let result = one("WITH c AS (SELECT x FROM s) SELECT x FROM s UNION ALL TABLE c");
    assert_eq!(result.read, vec![source("s", "x").relation]);
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Partial {
            known: vec![source("s", "x")],
            unresolved: vec![Unresolved::Unsupported {
                what: "TABLE source"
            }],
        }
    );
}

#[test]
fn unknown_left_branch_retains_known_right_sources() {
    for branch in ["TABLE public.t", "VALUES (1), (2, 3)"] {
        for prefix in ["INSERT INTO target ", "INSERT INTO target (a) "] {
            let result = one(&format!("{prefix}{branch} UNION ALL SELECT x FROM s"));
            assert!(result.diagnostics.is_empty());
            assert_eq!(result.output.shape, Shape::Closed);
            assert_eq!(result.output.columns.len(), 1);
            assert_eq!(
                result.output.columns[0].lineage,
                Lineage::Partial {
                    known: vec![source("s", "x")],
                    unresolved: vec![Unresolved::Unsupported {
                        what: if branch.starts_with("TABLE") {
                            "TABLE source"
                        } else {
                            "VALUES source"
                        },
                    }],
                }
            );
        }
    }
}

#[test]
fn unknown_branch_marker_survives_unknown_star_merges() {
    for sql in [
        "SELECT * FROM u UNION ALL (SELECT x FROM s UNION ALL TABLE public.t)",
        "(SELECT x FROM s UNION ALL TABLE public.t) UNION ALL SELECT * FROM u",
    ] {
        let result = one(sql);
        let Shape::Open { unexpanded } = &result.output.shape else {
            panic!("expected the existing unexpanded star: {sql}");
        };
        for lineage in result
            .output
            .columns
            .iter()
            .map(|column| &column.lineage)
            .chain(unexpanded.iter().map(|star| &star.lineage))
        {
            assert!(
                matches!(lineage, Lineage::Partial { unresolved, .. }
                if unresolved.contains(&Unresolved::Unsupported { what: "TABLE source" })),
                "{sql}: {lineage:?}"
            );
        }
    }
}

#[test]
fn qualified_table_source_is_not_hidden_by_a_same_named_cte() {
    let result = one("WITH c AS (SELECT x FROM s) SELECT x FROM s UNION ALL TABLE public.c");
    assert_eq!(
        result.read,
        vec![
            source("s", "x").relation,
            Relation {
                catalog: None,
                schema: Some(Ident::unquoted("public")),
                name: Ident::unquoted("c")
            },
        ]
    );
}
