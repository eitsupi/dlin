mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use dlin_sqllineage::StatementKind;

#[test]
fn cte_wrapped_delete() {
    let sql = "\
        WITH src AS (SELECT id FROM raw) \
        DELETE FROM target USING src WHERE target.id = src.id";
    let result = analyze_one(sql);
    assert_eq!(result.kind, StatementKind::Delete);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("raw")]);
}

#[test]
fn cte_wrapped_merge() {
    let sql = "\
        WITH src AS (SELECT id, val FROM raw) \
        MERGE INTO target USING src ON target.id = src.id \
        WHEN MATCHED THEN UPDATE SET target.val = src.val";
    let result = analyze_one(sql);
    assert_eq!(result.kind, StatementKind::Merge);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("raw")]);
    let m = find_mapping(&result.output.columns, "val");
    assert_eq!(concrete_sources(m), vec![("raw".into(), "val".into())]);
}
