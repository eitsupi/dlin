mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use sqllineage::{ColumnOrigin, TransformKind};

#[test]
fn select_columns() {
    let result = analyze_one("SELECT a, b FROM t");
    assert_eq!(result.columns.mappings.len(), 2);

    let m_a = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m_a), vec![("t".into(), "a".into())]);
    assert_eq!(m_a.transform, TransformKind::Direct);
    assert_eq!(m_a.target.table, None);

    let m_b = find_mapping(&result.columns.mappings, "b");
    assert_eq!(concrete_sources(m_b), vec![("t".into(), "b".into())]);
    assert_eq!(m_b.transform, TransformKind::Direct);
}

#[test]
fn unresolved_column_has_empty_ambiguous_candidates() {
    let result = analyze_one("SELECT missing");
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
fn select_expression() {
    let result = analyze_one("SELECT a + b AS c FROM t");
    assert_eq!(result.columns.mappings.len(), 1);

    let m = find_mapping(&result.columns.mappings, "c");
    assert_eq!(
        concrete_sources(m),
        vec![("t".into(), "a".into()), ("t".into(), "b".into())]
    );
    assert_eq!(m.transform, TransformKind::Expression);
}

#[test]
fn select_qualified_column() {
    let result = analyze_one("SELECT t.a FROM t");
    assert_eq!(result.columns.mappings.len(), 1);

    let m = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    assert_eq!(m.transform, TransformKind::Direct);
}

#[test]
fn insert_select_columns() {
    let result = analyze_one("INSERT INTO out SELECT a, b FROM t");
    assert_eq!(result.tables.output, Some(table("out")));
    assert_eq!(result.columns.mappings.len(), 2);

    let m_a = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m_a), vec![("t".into(), "a".into())]);
    assert_eq!(m_a.target.table, Some(table("out")));
}

#[test]
fn select_alias() {
    let result = analyze_one("SELECT a AS x FROM t");
    let m = find_mapping(&result.columns.mappings, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    assert_eq!(m.transform, TransformKind::Direct);
}

#[test]
fn select_aggregate() {
    let result = analyze_one("SELECT SUM(a) FROM t");
    let m = &result.columns.mappings[0];
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    assert_eq!(m.transform, TransformKind::Aggregation);
}

#[test]
fn select_count_star_is_aggregation_without_sources() {
    let result = analyze_one("SELECT COUNT(*) AS c FROM t");
    let m = find_mapping(&result.columns.mappings, "c");

    assert!(m.sources.is_empty());
    assert_eq!(m.transform, TransformKind::Aggregation);
}

#[test]
fn select_multiple_tables_qualified() {
    let result = analyze_one("SELECT t1.a, t2.b FROM t1 JOIN t2 ON t1.id = t2.id");
    assert_eq!(result.columns.mappings.len(), 2);

    let m_a = find_mapping(&result.columns.mappings, "a");
    assert_eq!(concrete_sources(m_a), vec![("t1".into(), "a".into())]);

    let m_b = find_mapping(&result.columns.mappings, "b");
    assert_eq!(concrete_sources(m_b), vec![("t2".into(), "b".into())]);
}

#[test]
fn duplicate_output_names_preserve_projection_order() {
    let result = analyze_one("SELECT a.id, b.id FROM a JOIN b ON a.id = b.bid");
    let sources: Vec<_> = result
        .columns
        .mappings
        .iter()
        .map(concrete_sources)
        .collect();

    assert_eq!(
        sources,
        vec![
            vec![("a".into(), "id".into())],
            vec![("b".into(), "id".into())]
        ]
    );
}

#[test]
fn three_duplicate_output_names_preserve_projection_order() {
    let result =
        analyze_one("SELECT a.id, b.id, c.id FROM a JOIN b ON a.id = b.bid JOIN c ON a.id = c.cid");
    let sources: Vec<_> = result
        .columns
        .mappings
        .iter()
        .map(concrete_sources)
        .collect();

    assert_eq!(
        sources,
        vec![
            vec![("a".into(), "id".into())],
            vec![("b".into(), "id".into())],
            vec![("c".into(), "id".into())],
        ]
    );
}

#[test]
fn select_case_expression() {
    let result = analyze_one("SELECT CASE WHEN a > 0 THEN b ELSE c END AS d FROM t");
    let m = find_mapping(&result.columns.mappings, "d");
    assert_eq!(m.transform, TransformKind::Conditional);
    assert_eq!(
        concrete_sources(m),
        vec![
            ("t".into(), "a".into()),
            ("t".into(), "b".into()),
            ("t".into(), "c".into()),
        ]
    );
}

#[test]
fn select_cast_passthrough() {
    let result = analyze_one("SELECT CAST(a AS INT) AS a_int FROM t");
    let m = find_mapping(&result.columns.mappings, "a_int");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    assert_eq!(m.transform, TransformKind::Direct);
}

#[test]
fn unnest_source_free_alias_has_no_physical_sources() {
    let result = analyze_one(
        "SELECT item FROM UNNEST(GENERATE_DATE_ARRAY(DATE('2020-01-01'), DATE('2020-01-03'))) AS item",
    );
    let mapping = find_mapping(&result.columns.mappings, "item");
    assert!(mapping.sources.is_empty());
    assert_eq!(result.tables.inputs, Vec::<sqllineage::TableRef>::new());
}

#[test]
fn unnest_alias_depends_on_array_column() {
    let result = analyze_one("SELECT item FROM base, UNNEST(base.items_array) AS item");
    let mapping = find_mapping(&result.columns.mappings, "item");
    assert_eq!(
        concrete_sources(mapping),
        vec![("base".into(), "items_array".into())]
    );
}

#[test]
fn unnest_unresolved_array_is_ambiguous_not_alias_column() {
    let result = analyze_one("SELECT item FROM UNNEST(missing_array) AS item");
    let mapping = find_mapping(&result.columns.mappings, "item");
    assert!(matches!(
        mapping.sources.as_slice(),
        [ColumnOrigin::Ambiguous { column, candidates }] if column == "item" && candidates.is_empty()
    ));
}

#[test]
fn unnest_unqualified_array_column_keeps_prior_table_binding() {
    let result = analyze_one("SELECT item FROM base, UNNEST(items_array) AS item");
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "item")),
        vec![("base".into(), "items_array".into())]
    );
}

#[test]
fn unnest_known_empty_virtual_dependency_stays_source_free() {
    let result = analyze_one("SELECT item FROM UNNEST([1, 2]) AS source, UNNEST(source) AS item");
    assert!(
        find_mapping(&result.columns.mappings, "item")
            .sources
            .is_empty()
    );
}

#[test]
fn unqualified_identifier_does_not_capture_relation_alias() {
    let result = analyze_one("SELECT a FROM table1 AS a, table2 AS b");
    let mapping = find_mapping(&result.columns.mappings, "a");
    assert!(matches!(
        mapping.sources.as_slice(),
        [ColumnOrigin::Ambiguous { column, candidates }]
            if column == "a" && candidates.len() == 2
    ));
}

#[test]
fn duplicate_virtual_slots_are_ambiguous_but_qualified_slots_resolve() {
    let result =
        analyze_one("SELECT x FROM base, UNNEST(base.first) AS u(x), UNNEST(base.second) AS v(x)");
    assert!(matches!(
        find_mapping(&result.columns.mappings, "x").sources.as_slice(),
        [ColumnOrigin::Ambiguous { column, candidates }] if column == "x" && candidates.is_empty()
    ));

    let result = analyze_one(
        "SELECT u.x, v.x FROM base, UNNEST(base.first) AS u(x), UNNEST(base.second) AS v(x)",
    );
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("base".into(), "first".into())]
    );
    assert_eq!(
        concrete_sources(&result.columns.mappings[1]),
        vec![("base".into(), "second".into())]
    );
}

#[test]
fn unnest_alias_columns_keep_array_expression_ordinals() {
    let result = analyze_one("SELECT x, y FROM base, UNNEST(base.first, base.second) AS u(x, y)");
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "x")),
        vec![("base".into(), "first".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "y")),
        vec![("base".into(), "second".into())]
    );
}

#[test]
fn unnest_offset_is_a_source_free_generated_slot() {
    let result = analyze_one("SELECT item, off FROM UNNEST([1, 2]) AS item WITH OFFSET AS off");
    assert!(
        find_mapping(&result.columns.mappings, "item")
            .sources
            .is_empty()
    );
    assert!(
        find_mapping(&result.columns.mappings, "off")
            .sources
            .is_empty()
    );
}
