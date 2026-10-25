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
