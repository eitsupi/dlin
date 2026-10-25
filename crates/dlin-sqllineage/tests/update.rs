mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use sqllineage::{StatementType, TransformKind};

#[test]
fn update_set_direct() {
    let sql = "UPDATE target SET col = source.val FROM source WHERE target.id = source.id";
    let result = analyze_one(sql);
    assert_eq!(result.tables.output, Some(table("target")));
    assert_eq!(result.tables.inputs, vec![table("source")]);

    let m = find_mapping(&result.columns.mappings, "col");
    assert_eq!(concrete_sources(m), vec![("source".into(), "val".into())]);
    assert_eq!(m.transform, TransformKind::Direct);
}

#[test]
fn update_set_expression() {
    let sql =
        "UPDATE target SET total = source.a + source.b FROM source WHERE target.id = source.id";
    let result = analyze_one(sql);

    let m = find_mapping(&result.columns.mappings, "total");
    assert_eq!(
        concrete_sources(m),
        vec![("source".into(), "a".into()), ("source".into(), "b".into()),]
    );
    assert_eq!(m.transform, TransformKind::Expression);
}

#[test]
fn update_from_with_subquery_in_where() {
    let sql = "\
        UPDATE target \
        SET col = source.val \
        FROM source \
        WHERE target.id IN (SELECT id FROM filter_table)";
    let result = analyze_one(sql);
    let mut inputs = result.tables.inputs;
    inputs.sort();
    assert_eq!(inputs, vec![table("filter_table"), table("source")]);
    assert_eq!(result.columns.mappings.len(), 1);
}

#[test]
fn cte_wrapped_update() {
    let sql = "\
        WITH src AS (SELECT id, val FROM raw) \
        UPDATE target SET col = src.val FROM src WHERE target.id = src.id";
    let result = analyze_one(sql);
    assert_eq!(result.statement_type, StatementType::Update);
    assert_eq!(result.tables.output, Some(table("target")));
    assert_eq!(result.tables.inputs, vec![table("raw")]);
    assert_eq!(result.columns.mappings.len(), 1);
    let m = find_mapping(&result.columns.mappings, "col");
    assert_eq!(concrete_sources(m), vec![("raw".into(), "val".into())]);
}
