mod common;

use common::{analyze_one, concrete_sources, find_mapping};
use dlin_sqllineage::Transform;

#[test]
fn sum_over_partition_by() {
    let sql = "SELECT SUM(amount) OVER (PARTITION BY dept ORDER BY hire_date) AS running FROM t";
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "running");
    assert_eq!(
        concrete_sources(m),
        vec![
            ("t".into(), "amount".into()),
            ("t".into(), "dept".into()),
            ("t".into(), "hire_date".into()),
        ]
    );
    assert_eq!(m.transform, Transform::Expression);
}

#[test]
fn row_number_over() {
    let sql = "SELECT ROW_NUMBER() OVER (PARTITION BY dept ORDER BY id) AS rn FROM t";
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "rn");
    assert_eq!(
        concrete_sources(m),
        vec![("t".into(), "dept".into()), ("t".into(), "id".into())]
    );
    assert_eq!(m.transform, Transform::Expression);
}

#[test]
fn aggregate_without_window() {
    let sql = "SELECT SUM(amount) AS total FROM t";
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "total");
    assert_eq!(m.transform, Transform::Aggregation);
}

#[test]
fn window_with_regular_columns() {
    let sql = "SELECT a, SUM(b) OVER (PARTITION BY c) AS win FROM t";
    let result = analyze_one(sql);
    assert_eq!(result.output.columns.len(), 2);

    let m_a = find_mapping(&result.output.columns, "a");
    assert_eq!(m_a.transform, Transform::Direct);

    let m_win = find_mapping(&result.output.columns, "win");
    assert_eq!(
        concrete_sources(m_win),
        vec![("t".into(), "b".into()), ("t".into(), "c".into())]
    );
}

/// A window written inline, named in a WINDOW clause, or refining a named
/// window has the same PARTITION BY and ORDER BY inputs.
#[test]
fn named_and_inherited_windows_match_inline_window() {
    let cases = [
        r#"SELECT ROW_NUMBER() OVER (PARTITION BY dept ORDER BY id) AS rn FROM t"#,
        r#"SELECT ROW_NUMBER() OVER w AS rn FROM t WINDOW w AS (PARTITION BY dept ORDER BY id)"#,
        r#"SELECT ROW_NUMBER() OVER (w ORDER BY id) AS rn FROM t WINDOW w AS (PARTITION BY dept)"#,
        r#"SELECT ROW_NUMBER() OVER w2 AS rn FROM t
           WINDOW w1 AS (PARTITION BY dept), w2 AS (w1 ORDER BY id)"#,
        r#"SELECT ROW_NUMBER() OVER W AS rn FROM t WINDOW w AS (PARTITION BY dept ORDER BY id)"#,
    ];
    for sql in cases {
        let result = analyze_one(sql);
        let m = find_mapping(&result.output.columns, "rn");
        assert_eq!(
            concrete_sources(m),
            vec![("t".into(), "dept".into()), ("t".into(), "id".into())],
            "{sql}"
        );
    }
}

/// BigQuery lets a named window be another name for one.
#[test]
fn renamed_named_window_matches_inline_window() {
    let sql = r#"SELECT ROW_NUMBER() OVER w2 AS rn FROM t
                 WINDOW w1 AS (PARTITION BY dept ORDER BY id), w2 AS w1"#;
    let result = common::analyze_all(sql, dlin_sqllineage::Dialect::BigQuery)
        .into_single()
        .unwrap();
    let m = find_mapping(&result.output.columns, "rn");
    assert_eq!(
        concrete_sources(m),
        vec![("t".into(), "dept".into()), ("t".into(), "id".into())]
    );
}

/// A window that cannot be resolved leaves its inputs unknown rather than
/// reporting a complete lineage without them.
#[test]
fn unresolved_named_window_is_partial() {
    let cases = [
        r#"SELECT SUM(amount) OVER w AS s FROM t"#,
        r#"SELECT SUM(amount) OVER (w ORDER BY id) AS s FROM t WINDOW v AS (PARTITION BY dept)"#,
        // A subquery cannot use the WINDOW clause of the outer query.
        r#"SELECT (SELECT MAX(amount) OVER w FROM u) AS s FROM t WINDOW w AS (PARTITION BY dept)"#,
        // Two definitions of one name do not say which is meant.
        r#"SELECT SUM(amount) OVER w AS s FROM t WINDOW w AS (PARTITION BY dept), w AS (ORDER BY id)"#,
    ];
    for sql in cases {
        let result = analyze_one(sql);
        let m = find_mapping(&result.output.columns, "s");
        assert!(
            common::unresolved(m)
                .iter()
                .any(|reason| matches!(reason, dlin_sqllineage::Unresolved::Unsupported { what } if *what == "named window")),
            "{sql}: {:?}",
            m.lineage
        );
    }
}

#[test]
fn circular_named_windows_are_partial() {
    let sql = r#"SELECT SUM(amount) OVER w1 AS s FROM t
                 WINDOW w1 AS (w2 ORDER BY id), w2 AS (w1 PARTITION BY dept)"#;
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "s");
    assert!(
        common::unresolved(m)
            .iter()
            .any(|reason| matches!(reason, dlin_sqllineage::Unresolved::Unsupported { what } if *what == "named window")),
        "{:?}",
        m.lineage
    );
}

/// A subquery of a WINDOW definition reads its relations once, whether the
/// window is used or not.
#[test]
fn named_window_subquery_reads_its_relation() {
    for sql in [
        r#"SELECT a.id FROM a WINDOW w AS (ORDER BY (SELECT MAX(x) FROM c))"#,
        r#"SELECT SUM(id) OVER w AS s, MIN(id) OVER w AS m FROM a
           WINDOW w AS (ORDER BY (SELECT MAX(x) FROM c))"#,
    ] {
        let result = analyze_one(sql);
        assert_eq!(
            result
                .read
                .iter()
                .map(|relation| relation.name.value.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"],
            "{sql}"
        );
    }
}

/// Each SELECT of a set operation, and a subquery, uses only its own WINDOW
/// clause even when the names are the same.
#[test]
fn named_windows_are_scoped_to_their_select() {
    let sql = r#"SELECT SUM(amount) OVER w AS s FROM t WINDOW w AS (PARTITION BY dept)
                 UNION ALL
                 SELECT SUM(amount) OVER w FROM u WINDOW w AS (ORDER BY id)"#;
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "s");
    assert_eq!(
        concrete_sources(m),
        vec![
            ("t".into(), "amount".into()),
            ("t".into(), "dept".into()),
            ("u".into(), "amount".into()),
            ("u".into(), "id".into()),
        ]
    );

    let sql = r#"SELECT (SELECT MAX(u.x) OVER w FROM u WINDOW w AS (ORDER BY u.id)) AS s
                 FROM t WINDOW w AS (PARTITION BY dept)"#;
    let result = analyze_one(sql);
    let m = find_mapping(&result.output.columns, "s");
    assert_eq!(
        concrete_sources(m),
        vec![("u".into(), "id".into()), ("u".into(), "x".into())]
    );
}
