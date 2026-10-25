mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use sqllineage::StatementType;

#[test]
fn cte_wrapped_delete() {
    let sql = "\
        WITH src AS (SELECT id FROM raw) \
        DELETE FROM target USING src WHERE target.id = src.id";
    let result = analyze_one(sql);
    assert_eq!(result.statement_type, StatementType::Delete);
    assert_eq!(result.tables.output, Some(table("target")));
    assert_eq!(result.tables.inputs, vec![table("raw")]);
}

#[test]
fn cte_wrapped_merge() {
    let sql = "\
        WITH src AS (SELECT id, val FROM raw) \
        MERGE INTO target USING src ON target.id = src.id \
        WHEN MATCHED THEN UPDATE SET target.val = src.val";
    let result = analyze_one(sql);
    assert_eq!(result.statement_type, StatementType::Merge);
    assert_eq!(result.tables.output, Some(table("target")));
    assert_eq!(result.tables.inputs, vec![table("raw")]);
    let m = find_mapping(&result.columns.mappings, "val");
    assert_eq!(concrete_sources(m), vec![("raw".into(), "val".into())]);
}
