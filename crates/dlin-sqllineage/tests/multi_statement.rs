use sqllineage::{AnalyzeOptions, StatementType, TableRef, analyze};

fn table(name: &str) -> TableRef {
    TableRef::new(name)
}

fn ok(sql: &str) -> Vec<sqllineage::AnalyzeResult> {
    analyze(sql, AnalyzeOptions::default()).expect("should parse")
}

#[test]
fn multi_statement_independent_results() {
    let results = ok("INSERT INTO t1 SELECT a FROM src1; INSERT INTO t2 SELECT b FROM src2");
    assert_eq!(results.len(), 2);

    assert_eq!(results[0].statement_type, StatementType::Insert);
    assert_eq!(results[0].tables.output, Some(table("t1")));
    assert_eq!(results[0].tables.inputs, vec![table("src1")]);

    assert_eq!(results[1].statement_type, StatementType::Insert);
    assert_eq!(results[1].tables.output, Some(table("t2")));
    assert_eq!(results[1].tables.inputs, vec![table("src2")]);
}

#[test]
fn multi_statement_mixed_types() {
    let results = ok("CREATE TABLE t AS SELECT a FROM src; DROP TABLE old; SELECT x FROM y");
    assert_eq!(results.len(), 3);

    assert_eq!(results[0].statement_type, StatementType::CreateTable);
    assert_eq!(results[0].tables.output, Some(table("t")));

    assert_eq!(results[1].statement_type, StatementType::Other);
    assert!(results[1].tables.inputs.is_empty());

    assert_eq!(results[2].statement_type, StatementType::Query);
    assert_eq!(results[2].tables.inputs, vec![table("y")]);
}

#[test]
fn single_statement_returns_vec_of_one() {
    let results = ok("SELECT a FROM t");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].statement_type, StatementType::Query);
}

#[test]
fn empty_input_returns_empty_vec() {
    let results = ok("");
    assert!(results.is_empty());
}

#[test]
fn case_normalization_default() {
    let r = &ok("INSERT INTO MySchema.MyTable SELECT Col FROM SRC")[0];
    assert_eq!(
        r.tables.output,
        Some(TableRef::with_schema("myschema", "mytable"))
    );
    assert_eq!(r.tables.inputs, vec![table("src")]);
}

#[test]
fn case_normalization_disabled() {
    let results = analyze(
        "INSERT INTO MySchema.MyTable SELECT Col FROM SRC",
        AnalyzeOptions {
            normalize_case: false,
            ..AnalyzeOptions::default()
        },
    )
    .expect("should parse");
    let r = &results[0];
    assert_eq!(
        r.tables.output,
        Some(TableRef::with_schema("MySchema", "MyTable"))
    );
    assert_eq!(r.tables.inputs, vec![table("SRC")]);
}

#[test]
fn statement_type_update() {
    assert_eq!(
        ok("UPDATE t SET a = 1")[0].statement_type,
        StatementType::Update
    );
}

#[test]
fn statement_type_delete() {
    assert_eq!(
        ok("DELETE FROM t WHERE id = 1")[0].statement_type,
        StatementType::Delete
    );
}

#[test]
fn statement_type_merge() {
    let r = ok("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET t.a = s.a");
    assert_eq!(r[0].statement_type, StatementType::Merge);
}

#[test]
fn parse_error_returns_err() {
    let result = analyze("THIS IS NOT VALID SQL !!!", AnalyzeOptions::default());
    assert!(result.is_err());
}

#[test]
fn parse_error_has_message() {
    let err = analyze("SELECT FROM", AnalyzeOptions::default()).unwrap_err();
    assert!(!err.message.is_empty());
}

#[test]
fn parse_error_displays_nicely() {
    let err = analyze("NOT VALID", AnalyzeOptions::default()).unwrap_err();
    let display = format!("{err}");
    assert!(display.contains("SQL parse error"), "got: {display}");
}
