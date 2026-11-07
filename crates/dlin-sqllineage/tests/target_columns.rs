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
