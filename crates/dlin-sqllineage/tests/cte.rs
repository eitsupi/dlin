mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use sqllineage::{ColumnMapping, ColumnOrigin, TableRef, TransformKind};

fn has_wildcard_from(mapping: &ColumnMapping, table_name: &str) -> bool {
    mapping.sources.iter().any(|source| {
        match source {
        ColumnOrigin::Wildcard { table } => table.table == table_name,
        ColumnOrigin::Recursive { base_sources } => base_sources.iter().any(|source| {
            matches!(source, ColumnOrigin::Wildcard { table } if table.table == table_name)
        }),
        _ => false,
    }
    })
}

#[test]
fn single_cte() {
    let sql = "WITH cte AS (SELECT a FROM t) SELECT a FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.tables.inputs, vec![table("t")]);
    assert_eq!(result.tables.output, None);
    assert_eq!(result.columns.mappings.len(), 1);
    let m = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    assert_eq!(m.transform, TransformKind::Direct);
}

#[test]
fn missing_column_from_cte_has_empty_ambiguous_candidates() {
    let sql = "WITH cte AS (SELECT present FROM source) SELECT missing FROM cte";
    let result = analyze_one(sql);
    let m = find_mapping(&result.columns.mappings, "missing");
    match &m.sources[0] {
        ColumnOrigin::Ambiguous { column, candidates } => {
            assert_eq!(column, "missing");
            assert!(candidates.is_empty());
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn cte_chain() {
    let sql = "WITH a AS (SELECT x FROM t), b AS (SELECT x FROM a) SELECT x FROM b";
    let result = analyze_one(sql);
    assert_eq!(result.tables.inputs, vec![table("t")]);
    assert_eq!(result.columns.mappings.len(), 1);
    let m = find_mapping(&result.columns.mappings, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "x".into())]);
}

#[test]
fn cte_multiple_refs() {
    let sql = "WITH cte AS (SELECT a FROM t) SELECT c1.a AS a1, c2.a AS a2 FROM cte c1, cte c2";
    let result = analyze_one(sql);
    assert_eq!(result.tables.inputs, vec![table("t")]);
    assert_eq!(result.columns.mappings.len(), 2);
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "a1")),
        vec![("t".into(), "a".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "a2")),
        vec![("t".into(), "a".into())]
    );
}

#[test]
fn cte_shadowing() {
    let sql = "\
        WITH x AS (SELECT a FROM t1) \
        SELECT b FROM (WITH x AS (SELECT b FROM t2) SELECT b FROM x) sub";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    let m = find_mapping(&result.columns.mappings, "b");
    assert_eq!(concrete_sources(m), vec![("t2".into(), "b".into())]);
}

#[test]
fn recursive_cte_base_case() {
    let sql = "\
        WITH RECURSIVE cte AS (\
            SELECT a FROM t \
            UNION ALL \
            SELECT a FROM cte\
        ) SELECT a FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.tables.inputs, vec![table("t")]);
    assert_eq!(result.columns.mappings.len(), 1);

    let m = &result.columns.mappings[0];
    assert_eq!(m.target.column, "a");
    match &m.sources[0] {
        ColumnOrigin::Recursive { base_sources } => {
            assert!(!base_sources.is_empty());
            assert!(base_sources.iter().all(|source| {
                !matches!(
                    source,
                    ColumnOrigin::Concrete { table, .. } if table.table == "cte"
                )
            }));
            match &base_sources[0] {
                ColumnOrigin::Concrete { table, column } => {
                    assert_eq!(table.table, "t");
                    assert_eq!(column, "a");
                }
                other => panic!("expected Concrete in base_sources, got: {other:?}"),
            }
        }
        other => panic!("expected Recursive origin, got: {other:?}"),
    }
}

#[test]
fn recursive_cte_no_cycle_warning() {
    let sql = "\
        WITH RECURSIVE cte AS (\
            SELECT 1 AS n \
            UNION ALL \
            SELECT n + 1 FROM cte WHERE n < 10\
        ) SELECT n FROM cte";
    let result = analyze_one(sql);
    assert!(
        !result
            .warnings
            .iter()
            .any(|w| matches!(w.kind, sqllineage::WarningKind::UnexpectedCycle)),
        "topological sort should succeed after back-edge removal"
    );
}

#[test]
fn derived_table() {
    let sql = "SELECT x FROM (SELECT a AS x FROM t) sub";
    let result = analyze_one(sql);
    assert_eq!(result.tables.inputs, vec![table("t")]);
    let m = find_mapping(&result.columns.mappings, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
}

#[test]
fn scalar_subquery_table_input() {
    let sql = "SELECT a, (SELECT MAX(b) FROM t2) AS m FROM t1";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    let m_a = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m_a), vec![("t1".into(), "a".into())]);
}

#[test]
fn correlated_subquery_table_input() {
    let sql = "SELECT a, (SELECT MAX(b) FROM t2 WHERE t2.id = t1.id) AS mb FROM t1";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
}

#[test]
fn union_all_columns() {
    let sql = "SELECT a, b FROM t1 UNION ALL SELECT c, d FROM t2";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    assert_eq!(result.columns.mappings.len(), 2);

    let m_a = find_mapping(&result.columns.mappings, "a");
    assert_eq!(
        concrete_sources(m_a),
        vec![("t1".into(), "a".into()), ("t2".into(), "c".into())]
    );
    let m_b = find_mapping(&result.columns.mappings, "b");
    assert_eq!(
        concrete_sources(m_b),
        vec![("t1".into(), "b".into()), ("t2".into(), "d".into())]
    );
}

#[test]
fn union_keeps_left_names_and_merges_explicit_columns_positionally() {
    let sql = "SELECT a AS left_name, b AS second_name FROM t1 \
               UNION ALL SELECT c AS right_name, d AS other_name FROM t2";
    let result = analyze_one(sql);
    assert_eq!(result.columns.mappings.len(), 2);
    assert_eq!(result.columns.mappings[0].target.column, "left_name");
    assert_eq!(result.columns.mappings[1].target.column, "second_name");
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("t1".into(), "a".into()), ("t2".into(), "c".into())]
    );
    assert_eq!(
        concrete_sources(&result.columns.mappings[1]),
        vec![("t1".into(), "b".into()), ("t2".into(), "d".into())]
    );
}

#[test]
fn nested_union_preserves_outer_left_names_and_all_branch_sources() {
    let sql = "SELECT a AS first_name FROM t1 \
               UNION ALL SELECT b AS second_name FROM t2 \
               UNION ALL SELECT c AS third_name FROM t3";
    let result = analyze_one(sql);
    assert_eq!(result.columns.mappings.len(), 1);
    assert_eq!(result.columns.mappings[0].target.column, "first_name");
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![
            ("t1".into(), "a".into()),
            ("t2".into(), "b".into()),
            ("t3".into(), "c".into())
        ]
    );
}

#[test]
fn union_transform_prefers_aggregation_across_branches() {
    let sql = "SELECT SUM(a) AS value FROM t1 UNION ALL SELECT b AS other_value FROM t2";
    let result = analyze_one(sql);
    assert_eq!(result.columns.mappings.len(), 1);
    assert_eq!(result.columns.mappings[0].target.column, "value");
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("t1".into(), "a".into()), ("t2".into(), "b".into())]
    );
    assert_eq!(
        result.columns.mappings[0].transform,
        TransformKind::Aggregation
    );
}

#[test]
fn unknown_leading_star_is_preserved_without_catalog() {
    let result = analyze_one("SELECT * FROM unknown_left UNION ALL SELECT a, b FROM known");
    assert_eq!(result.columns.mappings.len(), 3);
    match &result.columns.mappings[0].sources[0] {
        ColumnOrigin::Wildcard { table } => assert_eq!(table.table, "unknown_left"),
        other => panic!("expected wildcard, got {other:?}"),
    }
    assert!(has_wildcard_from(
        &result.columns.mappings[1],
        "unknown_left"
    ));
    assert!(has_wildcard_from(
        &result.columns.mappings[2],
        "unknown_left"
    ));
}

#[test]
fn unknown_non_leading_star_does_not_drop_known_branch_columns() {
    let result = analyze_one("SELECT id, * FROM unknown_left UNION ALL SELECT a, b, c FROM known");
    assert_eq!(result.columns.mappings.len(), 4);
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Concrete { .. }, ColumnOrigin::Concrete { .. }]
    ));
    match &result.columns.mappings[1].sources[0] {
        ColumnOrigin::Wildcard { table } => assert_eq!(table.table, "unknown_left"),
        other => panic!("expected wildcard, got {other:?}"),
    }
    assert!(has_wildcard_from(
        &result.columns.mappings[2],
        "unknown_left"
    ));
    assert!(has_wildcard_from(
        &result.columns.mappings[3],
        "unknown_left"
    ));
    assert_eq!(result.columns.mappings[2].target.column, "b");
    assert_eq!(result.columns.mappings[3].target.column, "c");
}

#[test]
fn unknown_right_star_marks_known_left_tail_as_unresolved() {
    let result = analyze_one("SELECT a, b, c FROM known UNION ALL SELECT * FROM unknown_right");
    assert_eq!(result.columns.mappings.len(), 4);
    for mapping in &result.columns.mappings[..3] {
        assert!(has_wildcard_from(mapping, "unknown_right"));
    }
}

#[test]
fn unknown_stars_on_both_set_branches_are_both_retained() {
    let result = analyze_one("SELECT * FROM unknown_left UNION ALL SELECT * FROM unknown_right");
    assert_eq!(result.columns.mappings.len(), 2);
    for (mapping, table_name) in result
        .columns
        .mappings
        .iter()
        .zip(["unknown_left", "unknown_right"])
    {
        assert!(has_wildcard_from(mapping, table_name));
        assert!(has_wildcard_from(
            mapping,
            if table_name == "unknown_left" {
                "unknown_right"
            } else {
                "unknown_left"
            }
        ));
    }
}

#[test]
fn nested_unknown_set_keeps_every_branch_mapping() {
    let result = analyze_one(
        "SELECT * FROM unknown_left UNION ALL SELECT a FROM known UNION ALL SELECT * FROM unknown_right",
    );
    assert_eq!(result.columns.mappings.len(), 3);
    assert!(matches!(
        result.columns.mappings[0].sources[0],
        ColumnOrigin::Wildcard { .. }
    ));
    assert!(matches!(
        result.columns.mappings[2].sources[0],
        ColumnOrigin::Wildcard { .. }
    ));
    assert!(has_wildcard_from(
        &result.columns.mappings[1],
        "unknown_left"
    ));
    assert!(has_wildcard_from(
        &result.columns.mappings[1],
        "unknown_right"
    ));
}

#[test]
fn leading_unknown_star_hides_nonleading_only_set_names() {
    let result = analyze_one(
        "SELECT * FROM unknown_source \
         UNION ALL SELECT id, amt AS total FROM known_table \
         UNION ALL SELECT id, fee FROM third_table",
    );
    let names = result
        .columns
        .mappings
        .iter()
        .map(|mapping| mapping.target.column.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["*", "id", "total"]);
}

#[test]
fn exact_set_arity_mismatch_is_an_analysis_error() {
    let result = sqllineage::analyze(
        "SELECT a FROM t1 UNION ALL SELECT b, c FROM t2",
        sqllineage::AnalyzeOptions::default(),
    );
    let error = match result {
        Ok(_) => panic!("exact arity mismatch should not be truncated"),
        Err(error) => error,
    };
    assert_eq!(
        error.message,
        "set operation arity mismatch: left has 1 columns, right has 2 columns"
    );
}

#[test]
fn union_inside_cte() {
    let sql = "\
        WITH cte AS (SELECT a FROM t1 UNION ALL SELECT b FROM t2) \
        SELECT a FROM cte";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    let m = find_mapping(&result.columns.mappings, "a");
    assert_eq!(
        concrete_sources(m),
        vec![("t1".into(), "a".into()), ("t2".into(), "b".into())]
    );
}

#[test]
fn select_star_from_derived_table() {
    let sql = "\
        SELECT * FROM (\
            SELECT base_date, money_code, action_type_code \
            FROM core.cos_dw.some_table\
        )";
    let result = analyze_one(sql);
    assert_eq!(
        result.tables.inputs,
        vec![TableRef {
            catalog: Some("core".into()),
            schema: Some("cos_dw".into()),
            table: "some_table".into(),
        }]
    );
    assert_eq!(result.columns.mappings.len(), 3);
    let m = find_mapping(&result.columns.mappings, "base_date");
    assert_eq!(
        concrete_sources(m),
        vec![("some_table".into(), "base_date".into())]
    );
    assert_eq!(m.transform, TransformKind::Direct);

    let m = find_mapping(&result.columns.mappings, "money_code");
    assert_eq!(
        concrete_sources(m),
        vec![("some_table".into(), "money_code".into())]
    );

    let m = find_mapping(&result.columns.mappings, "action_type_code");
    assert_eq!(
        concrete_sources(m),
        vec![("some_table".into(), "action_type_code".into())]
    );
}

#[test]
fn select_star_from_cte() {
    let sql = "WITH cte AS (SELECT a, b FROM t) SELECT * FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.tables.inputs, vec![table("t")]);
    assert_eq!(result.columns.mappings.len(), 2);
    let m = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    let m = find_mapping(&result.columns.mappings, "b");
    assert_eq!(concrete_sources(m), vec![("t".into(), "b".into())]);
}

#[test]
fn cte_join_unqualified_column() {
    let sql = "\
        WITH cte AS (SELECT a FROM t1) \
        SELECT a FROM cte JOIN t2 ON cte.id = t2.id";
    let result = analyze_one(sql);
    let m = find_mapping(&result.columns.mappings, "a");
    // "a" should come from CTE (which traces to t1.a), not from t2
    assert_eq!(concrete_sources(m), vec![("t1".into(), "a".into())]);
}

#[test]
fn qualified_wildcard_on_cte() {
    let sql = "WITH cte AS (SELECT a, b FROM t) SELECT cte.* FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.columns.mappings.len(), 2);
    let m = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    let m = find_mapping(&result.columns.mappings, "b");
    assert_eq!(concrete_sources(m), vec![("t".into(), "b".into())]);
}

#[test]
fn nested_select_star_derived() {
    let sql = "SELECT * FROM (SELECT * FROM (SELECT a FROM t) inner_q) outer_q";
    let result = analyze_one(sql);
    assert_eq!(result.columns.mappings.len(), 1);
    let m = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
}

#[test]
fn cte_chain_select_star() {
    let sql = "WITH a AS (SELECT x FROM t), b AS (SELECT * FROM a) SELECT * FROM b";
    let result = analyze_one(sql);
    assert_eq!(result.columns.mappings.len(), 1);
    let m = find_mapping(&result.columns.mappings, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "x".into())]);
}
