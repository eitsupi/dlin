mod common;

use common::{analyze_one, concrete_sources, find_mapping, known_sources, table};
use common::{output_name, unresolved};
use dlin_sqllineage::{
    Analyzer, Diagnostic, DiagnosticKind, Dialect, Ident, Lineage, OutputColumn, Relation,
    Severity, Shape, StatementAnalysis, Transform, UnexpandedStar, Unresolved,
};

fn has_wildcard_from(column: &OutputColumn, table_name: &str) -> bool {
    unresolved(column).iter().any(|source| {
        matches!(source, Unresolved::BehindStar { relation } if relation.name.value == table_name)
    })
}

fn unexpanded(result: &StatementAnalysis) -> &[UnexpandedStar] {
    match &result.output.shape {
        Shape::Open { unexpanded } => unexpanded,
        Shape::Closed => panic!("expected an open output shape"),
    }
}

/// The relations behind the unresolved stars of an unexpanded projection.
fn star_relations(star: &UnexpandedStar) -> Vec<&str> {
    let Lineage::Partial { known, unresolved } = &star.lineage else {
        panic!("an unexpanded star is never complete: {:?}", star.lineage)
    };
    assert!(known.is_empty());
    let mut names: Vec<_> = unresolved
        .iter()
        .map(|item| match item {
            Unresolved::BehindStar { relation } => relation.name.value.as_str(),
            other => panic!("expected a wildcard, got {other:?}"),
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

#[test]
fn single_cte() {
    let sql = "WITH cte AS (SELECT a FROM t) SELECT a FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.read, vec![table("t")]);
    assert_eq!(result.written, None);
    assert_eq!(result.output.columns.len(), 1);
    let m = find_mapping(&result.output.columns, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    assert_eq!(m.transform, Transform::Direct);
}

#[test]
fn missing_column_from_cte_has_empty_ambiguous_candidates() {
    let sql = "WITH cte AS (SELECT present FROM source) SELECT missing FROM cte";
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "missing");
    match &m.lineage {
        Lineage::Partial { unresolved, .. } => assert!(matches!(&unresolved[..],
                [Unresolved::Unbound { column }] if column.value == "missing")),
        other => panic!("expected partial lineage, got {other:?}"),
    }
}

#[test]
fn cte_chain() {
    let sql = "WITH a AS (SELECT x FROM t), b AS (SELECT x FROM a) SELECT x FROM b";
    let result = analyze_one(sql);
    assert_eq!(result.read, vec![table("t")]);
    assert_eq!(result.output.columns.len(), 1);
    let m = find_mapping(&result.output.columns, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "x".into())]);
}

#[test]
fn cte_multiple_refs() {
    let sql = "WITH cte AS (SELECT a FROM t) SELECT c1.a AS a1, c2.a AS a2 FROM cte c1, cte c2";
    let result = analyze_one(sql);
    assert_eq!(result.read, vec![table("t")]);
    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "a1")),
        vec![("t".into(), "a".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "a2")),
        vec![("t".into(), "a".into())]
    );
}

#[test]
fn cte_shadowing() {
    let sql = "\
        WITH x AS (SELECT a FROM t1) \
        SELECT b FROM (WITH x AS (SELECT b FROM t2) SELECT b FROM x) sub";
    let result = analyze_one(sql);
    let mut inputs = result.read.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    let m = find_mapping(&result.output.columns, "b");
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
    assert_eq!(result.read, vec![table("t")]);
    assert_eq!(result.output.columns.len(), 1);

    let m = &result.output.columns[0];
    assert_eq!(output_name(m), Some("a"));
    // The base case contributes `t.a`; the recursive step must not feed the
    // CTE back into its own sources.
    let Lineage::Partial { known, unresolved } = &m.lineage else {
        panic!("expected partial lineage, got {:?}", m.lineage)
    };
    assert_eq!(
        known
            .iter()
            .map(|source| (
                source.relation.name.value.as_str(),
                source.column.value.as_str()
            ))
            .collect::<Vec<_>>(),
        [("t", "a")]
    );
    assert_eq!(unresolved.as_slice(), [Unresolved::Recursive]);
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
            .diagnostics
            .iter()
            .any(|d| matches!(d.kind, dlin_sqllineage::DiagnosticKind::UnexpectedCycle)),
        "topological sort should succeed after back-edge removal"
    );
}

#[test]
fn derived_table() {
    let sql = "SELECT x FROM (SELECT a AS x FROM t) sub";
    let result = analyze_one(sql);
    assert_eq!(result.read, vec![table("t")]);
    let m = find_mapping(&result.output.columns, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
}

#[test]
fn scalar_subquery_table_input() {
    let sql = "SELECT a, (SELECT MAX(b) FROM t2) AS m FROM t1";
    let result = analyze_one(sql);
    let mut inputs = result.read.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    let m_a = find_mapping(&result.output.columns, "a");
    assert_eq!(concrete_sources(m_a), vec![("t1".into(), "a".into())]);
}

#[test]
fn correlated_subquery_table_input() {
    let sql = "SELECT a, (SELECT MAX(b) FROM t2 WHERE t2.id = t1.id) AS mb FROM t1";
    let result = analyze_one(sql);
    let mut inputs = result.read.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
}

#[test]
fn union_all_columns() {
    let sql = "SELECT a, b FROM t1 UNION ALL SELECT c, d FROM t2";
    let result = analyze_one(sql);
    let mut inputs = result.read.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    assert_eq!(result.output.columns.len(), 2);

    let m_a = find_mapping(&result.output.columns, "a");
    assert_eq!(
        concrete_sources(m_a),
        vec![("t1".into(), "a".into()), ("t2".into(), "c".into())]
    );
    let m_b = find_mapping(&result.output.columns, "b");
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
    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(output_name(&result.output.columns[0]), Some("left_name"));
    assert_eq!(output_name(&result.output.columns[1]), Some("second_name"));
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("t1".into(), "a".into()), ("t2".into(), "c".into())]
    );
    assert_eq!(
        concrete_sources(&result.output.columns[1]),
        vec![("t1".into(), "b".into()), ("t2".into(), "d".into())]
    );
}

#[test]
fn nested_union_preserves_outer_left_names_and_all_branch_sources() {
    let sql = "SELECT a AS first_name FROM t1 \
               UNION ALL SELECT b AS second_name FROM t2 \
               UNION ALL SELECT c AS third_name FROM t3";
    let result = analyze_one(sql);
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(output_name(&result.output.columns[0]), Some("first_name"));
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
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
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(output_name(&result.output.columns[0]), Some("value"));
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("t1".into(), "a".into()), ("t2".into(), "b".into())]
    );
    assert_eq!(result.output.columns[0].transform, Transform::Aggregation);
}

#[test]
fn unknown_leading_star_is_preserved_without_catalog() {
    let result = analyze_one("SELECT * FROM unknown_left UNION ALL SELECT a, b FROM known");
    assert!(result.has_unresolved_stars);
    let stars = unexpanded(&result);
    assert_eq!(stars.len(), 1);
    assert_eq!(
        stars[0].relation.as_ref().map(|r| r.name.value.as_str()),
        Some("unknown_left")
    );
    assert_eq!(stars[0].after_ordinal, None);
    assert_eq!(star_relations(&stars[0]), ["unknown_left"]);

    // The known branch's columns follow the star, so their positions are unknown.
    assert_eq!(result.output.columns.len(), 2);
    for (column, name) in result.output.columns.iter().zip(["a", "b"]) {
        assert_eq!(output_name(column), Some(name));
        assert_eq!(column.ordinal, None);
        assert!(has_wildcard_from(column, "unknown_left"));
        assert_eq!(
            known_sources(column)
                .iter()
                .map(|source| (
                    source.relation.name.value.as_str(),
                    source.column.value.as_str()
                ))
                .collect::<Vec<_>>(),
            [("known", name)]
        );
    }
}

#[test]
fn unknown_non_leading_star_does_not_drop_known_branch_columns() {
    let result = analyze_one("SELECT id, * FROM unknown_left UNION ALL SELECT a, b, c FROM known");
    assert_eq!(result.output.columns.len(), 3);

    let id = find_mapping(&result.output.columns, "id");
    assert_eq!(id.ordinal, Some(0));
    assert_eq!(
        concrete_sources(id),
        vec![
            ("known".into(), "a".into()),
            ("unknown_left".into(), "id".into())
        ]
    );
    for name in ["b", "c"] {
        let column = find_mapping(&result.output.columns, name);
        assert_eq!(column.ordinal, None);
        assert!(has_wildcard_from(column, "unknown_left"));
        assert_eq!(
            known_sources(column)
                .iter()
                .map(|source| (
                    source.relation.name.value.as_str(),
                    source.column.value.as_str()
                ))
                .collect::<Vec<_>>(),
            [("known", name)]
        );
    }

    let stars = unexpanded(&result);
    assert_eq!(stars.len(), 1);
    assert_eq!(
        stars[0].relation.as_ref().map(|r| r.name.value.as_str()),
        Some("unknown_left")
    );
    assert_eq!(stars[0].after_ordinal, Some(0));
}

#[test]
fn unknown_right_star_marks_known_left_tail_as_unresolved() {
    let result = analyze_one("SELECT a, b, c FROM known UNION ALL SELECT * FROM unknown_right");
    assert!(matches!(&result.output.shape, Shape::Open { .. }));
    assert_eq!(result.output.columns.len(), 3);
    for mapping in &result.output.columns {
        assert!(has_wildcard_from(mapping, "unknown_right"));
    }
}

#[test]
fn unknown_stars_on_both_set_branches_are_both_retained() {
    let result = analyze_one("SELECT * FROM unknown_left UNION ALL SELECT * FROM unknown_right");
    assert!(result.output.columns.is_empty());
    let stars = unexpanded(&result);
    // Each branch's star is retained, and either branch may supply the columns.
    for table_name in ["unknown_left", "unknown_right"] {
        assert!(stars.iter().any(|star| {
            star.relation.as_ref().map(|r| r.name.value.as_str()) == Some(table_name)
        }));
    }
    for star in stars {
        assert_eq!(star.after_ordinal, None);
        assert_eq!(star_relations(star), ["unknown_left", "unknown_right"]);
    }
}

#[test]
fn nested_unknown_set_keeps_every_branch_mapping() {
    let result = analyze_one(
        "SELECT * FROM unknown_left UNION ALL SELECT a FROM known UNION ALL SELECT * FROM unknown_right",
    );
    assert_eq!(result.output.columns.len(), 1);
    let a = find_mapping(&result.output.columns, "a");
    assert_eq!(a.ordinal, None);
    assert!(has_wildcard_from(a, "unknown_left"));
    assert!(has_wildcard_from(a, "unknown_right"));

    // Both unknown branches keep their star, each aware of the other branch.
    let stars = unexpanded(&result);
    for table_name in ["unknown_left", "unknown_right"] {
        assert!(stars.iter().any(|star| {
            star.relation.as_ref().map(|r| r.name.value.as_str()) == Some(table_name)
        }));
    }
    for star in stars {
        assert_eq!(star_relations(star), ["unknown_left", "unknown_right"]);
    }
}

#[test]
fn leading_unknown_star_hides_nonleading_only_set_names() {
    let result = analyze_one(
        "SELECT * FROM unknown_source \
         UNION ALL SELECT id, amt AS total FROM known_table \
         UNION ALL SELECT id, fee FROM third_table",
    );
    assert!(matches!(&result.output.shape, Shape::Open { .. }));
    let names = result
        .output
        .columns
        .iter()
        .map(|mapping| output_name(mapping).unwrap_or(""))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["id", "total"]);
}

#[test]
fn exact_set_arity_mismatch_is_an_analysis_error() {
    let result = Analyzer::new(Dialect::Generic)
        .analyze("SELECT a FROM t1 UNION ALL SELECT b, c FROM t2")
        .unwrap();
    assert_eq!(result.statements.len(), 1);
    assert!(result.statements[0].output.columns.is_empty());
    assert_eq!(
        result.statements[0].diagnostics,
        [Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 },
            location: None,
        }]
    );
}

#[test]
fn union_inside_cte() {
    let sql = "\
        WITH cte AS (SELECT a FROM t1 UNION ALL SELECT b FROM t2) \
        SELECT a FROM cte";
    let result = analyze_one(sql);
    let mut inputs = result.read.clone();
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
    let m = find_mapping(&result.output.columns, "a");
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
        result.read,
        vec![Relation {
            catalog: Some(Ident::unquoted("core")),
            schema: Some(Ident::unquoted("cos_dw")),
            name: Ident::unquoted("some_table"),
        }]
    );
    assert_eq!(result.output.columns.len(), 3);
    let m = find_mapping(&result.output.columns, "base_date");
    assert_eq!(
        concrete_sources(m),
        vec![("some_table".into(), "base_date".into())]
    );
    assert_eq!(m.transform, Transform::Direct);

    let m = find_mapping(&result.output.columns, "money_code");
    assert_eq!(
        concrete_sources(m),
        vec![("some_table".into(), "money_code".into())]
    );

    let m = find_mapping(&result.output.columns, "action_type_code");
    assert_eq!(
        concrete_sources(m),
        vec![("some_table".into(), "action_type_code".into())]
    );
}

#[test]
fn select_star_from_cte() {
    let sql = "WITH cte AS (SELECT a, b FROM t) SELECT * FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.read, vec![table("t")]);
    assert_eq!(result.output.columns.len(), 2);
    let m = find_mapping(&result.output.columns, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    let m = find_mapping(&result.output.columns, "b");
    assert_eq!(concrete_sources(m), vec![("t".into(), "b".into())]);
}

#[test]
fn cte_join_unqualified_column() {
    let sql = "\
        WITH cte AS (SELECT a FROM t1) \
        SELECT a FROM cte JOIN t2 ON cte.id = t2.id";
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "a");
    // "a" should come from CTE (which traces to t1.a), not from t2
    assert_eq!(concrete_sources(m), vec![("t1".into(), "a".into())]);
}

#[test]
fn qualified_wildcard_on_cte() {
    let sql = "WITH cte AS (SELECT a, b FROM t) SELECT cte.* FROM cte";
    let result = analyze_one(sql);
    assert_eq!(result.output.columns.len(), 2);
    let m = find_mapping(&result.output.columns, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
    let m = find_mapping(&result.output.columns, "b");
    assert_eq!(concrete_sources(m), vec![("t".into(), "b".into())]);
}

#[test]
fn nested_select_star_derived() {
    let sql = "SELECT * FROM (SELECT * FROM (SELECT a FROM t) inner_q) outer_q";
    let result = analyze_one(sql);
    assert_eq!(result.output.columns.len(), 1);
    let m = find_mapping(&result.output.columns, "a");
    assert_eq!(concrete_sources(m), vec![("t".into(), "a".into())]);
}

#[test]
fn cte_chain_select_star() {
    let sql = "WITH a AS (SELECT x FROM t), b AS (SELECT * FROM a) SELECT * FROM b";
    let result = analyze_one(sql);
    assert_eq!(result.output.columns.len(), 1);
    let m = find_mapping(&result.output.columns, "x");
    assert_eq!(concrete_sources(m), vec![("t".into(), "x".into())]);
}
