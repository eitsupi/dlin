mod common;

use common::{analyze_one, concrete_sources, find_mapping, table};
use dlin_sqllineage::Transform;

#[test]
fn merge_when_matched_update_set() {
    let sql = "\
        MERGE INTO target t \
        USING source s ON t.id = s.id \
        WHEN MATCHED THEN UPDATE SET t.col = s.val";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);

    let m = find_mapping(&result.output.columns, "col");
    assert_eq!(concrete_sources(m), vec![("source".into(), "val".into())]);
    assert_eq!(m.transform, Transform::Direct);
}

#[test]
fn merge_when_not_matched_insert() {
    let sql = "\
        MERGE INTO target t \
        USING source s ON t.id = s.id \
        WHEN NOT MATCHED THEN INSERT (id, name) VALUES (s.id, s.name)";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));

    let m_id = find_mapping(&result.output.columns, "id");
    assert_eq!(concrete_sources(m_id), vec![("source".into(), "id".into())]);

    let m_name = find_mapping(&result.output.columns, "name");
    assert_eq!(
        concrete_sources(m_name),
        vec![("source".into(), "name".into())]
    );
}

#[test]
fn merge_both_clauses() {
    let sql = "\
        MERGE INTO target t \
        USING source s ON t.id = s.id \
        WHEN MATCHED THEN UPDATE SET t.val = s.val \
        WHEN NOT MATCHED THEN INSERT (id, val) VALUES (s.id, s.val)";
    let result = analyze_one(sql);

    // UPDATE SET produces "val", INSERT VALUES produces "id" and "val"
    let val_mappings: Vec<_> = result
        .output
        .columns
        .iter()
        .filter(|m| common::output_name(m) == Some("val"))
        .collect();
    assert!(
        val_mappings.len() >= 2,
        "expected val from both UPDATE SET and INSERT"
    );

    let id_mappings: Vec<_> = result
        .output
        .columns
        .iter()
        .filter(|m| common::output_name(m) == Some("id"))
        .collect();
    assert!(!id_mappings.is_empty(), "expected id from INSERT clause");
}
