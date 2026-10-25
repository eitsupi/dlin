mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use dlin_sqllineage::{StatementKind, Transform};

#[test]
fn update_set_direct() {
    let sql = "UPDATE target SET col = source.val FROM source WHERE target.id = source.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);

    let m = find_mapping(&result.output.columns, "col");
    assert_eq!(concrete_sources(m), vec![("source".into(), "val".into())]);
    assert_eq!(m.transform, Transform::Direct);
}

#[test]
fn update_set_expression() {
    let sql =
        "UPDATE target SET total = source.a + source.b FROM source WHERE target.id = source.id";
    let result = analyze_one(sql);

    let m = find_mapping(&result.output.columns, "total");
    assert_eq!(
        concrete_sources(m),
        vec![("source".into(), "a".into()), ("source".into(), "b".into()),]
    );
    assert_eq!(m.transform, Transform::Expression);
}

#[test]
fn update_from_with_subquery_in_where() {
    let sql = "\
        UPDATE target \
        SET col = source.val \
        FROM source \
        WHERE target.id IN (SELECT id FROM filter_table)";
    let result = analyze_one(sql);
    let mut inputs = result.read;
    inputs.sort();
    assert_eq!(inputs, vec![table("filter_table"), table("source")]);
    assert_eq!(result.output.columns.len(), 1);
}

#[test]
fn cte_wrapped_update() {
    let sql = "\
        WITH src AS (SELECT id, val FROM raw) \
        UPDATE target SET col = src.val FROM src WHERE target.id = src.id";
    let result = analyze_one(sql);
    assert_eq!(result.kind, StatementKind::Update);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("raw")]);
    assert_eq!(result.output.columns.len(), 1);
    let m = find_mapping(&result.output.columns, "col");
    assert_eq!(concrete_sources(m), vec![("raw".into(), "val".into())]);
}
