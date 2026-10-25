use dlin_sqllineage::{Analyzer, Dialect, Ident, Relation, StatementKind};

fn table(name: &str) -> Relation {
    Relation {
        catalog: None,
        schema: None,
        name: Ident::unquoted(name),
    }
}

fn ok(sql: &str) -> Vec<dlin_sqllineage::StatementAnalysis> {
    Analyzer::new(Dialect::Generic)
        .analyze(sql)
        .expect("should parse")
        .statements
}

#[test]
fn multi_statement_independent_results() {
    let results = ok("INSERT INTO t1 SELECT a FROM src1; INSERT INTO t2 SELECT b FROM src2");
    assert_eq!(results.len(), 2);

    assert_eq!(results[0].kind, StatementKind::Insert);
    assert_eq!(results[0].written, Some(table("t1")));
    assert_eq!(results[0].read, vec![table("src1")]);

    assert_eq!(results[1].kind, StatementKind::Insert);
    assert_eq!(results[1].written, Some(table("t2")));
    assert_eq!(results[1].read, vec![table("src2")]);
}

#[test]
fn multi_statement_mixed_types() {
    let results = ok("CREATE TABLE t AS SELECT a FROM src; DROP TABLE old; SELECT x FROM y");
    assert_eq!(results.len(), 3);

    assert_eq!(results[0].kind, StatementKind::CreateTableAs);
    assert_eq!(results[0].written, Some(table("t")));

    assert_eq!(results[1].kind, StatementKind::Other);
    assert!(results[1].read.is_empty());

    assert_eq!(results[2].kind, StatementKind::Query);
    assert_eq!(results[2].read, vec![table("y")]);
}

#[test]
fn single_statement_returns_vec_of_one() {
    let results = ok("SELECT a FROM t");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].kind, StatementKind::Query);
}

#[test]
fn empty_input_returns_empty_vec() {
    let results = ok("");
    assert!(results.is_empty());
}

#[test]
fn relation_names_keep_source_spelling_and_expose_dialect_key() {
    let results = Analyzer::new(Dialect::PostgreSql)
        .analyze("INSERT INTO MySchema.MyTable SELECT Col FROM SRC")
        .unwrap();
    let r = &results.statements[0];
    assert_eq!(
        r.written,
        Some(Relation {
            catalog: None,
            schema: Some(Ident::unquoted("MySchema")),
            name: Ident::unquoted("MyTable")
        })
    );
    assert_eq!(r.read[0].name.value, "SRC");
    assert_eq!(r.read[0].name.relation_key(Dialect::PostgreSql), "src");
}

#[test]
fn quoted_relation_names_preserve_spelling() {
    let results = Analyzer::new(Dialect::PostgreSql)
        .analyze("INSERT INTO \"MySchema\".\"MyTable\" SELECT Col FROM SRC")
        .expect("should parse");
    let r = &results.statements[0];
    assert_eq!(
        r.written,
        Some(Relation {
            catalog: None,
            schema: Some(Ident::quoted("MySchema", dlin_sqllineage::Quote::Double)),
            name: Ident::quoted("MyTable", dlin_sqllineage::Quote::Double),
        })
    );
    assert_eq!(r.read[0].name.value, "SRC");
    assert_eq!(r.read[0].name.quote, None);
}

#[test]
fn statement_type_update() {
    assert_eq!(ok("UPDATE t SET a = 1")[0].kind, StatementKind::Update);
}

#[test]
fn statement_type_delete() {
    assert_eq!(
        ok("DELETE FROM t WHERE id = 1")[0].kind,
        StatementKind::Delete
    );
}

#[test]
fn statement_type_merge() {
    let r = ok("MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET t.a = s.a");
    assert_eq!(r[0].kind, StatementKind::Merge);
}

#[test]
fn parse_error_returns_err() {
    let result = Analyzer::new(Dialect::Generic).analyze("THIS IS NOT VALID SQL !!!");
    assert!(result.is_err());
}

#[test]
fn parse_error_has_message() {
    let err = Analyzer::new(Dialect::Generic)
        .analyze("SELECT FROM")
        .unwrap_err();
    assert!(!err.message.is_empty());
}

#[test]
fn parse_error_displays_nicely() {
    let err = Analyzer::new(Dialect::Generic)
        .analyze("NOT VALID")
        .unwrap_err();
    let display = format!("{err}");
    assert!(display.contains("SQL parse error"), "got: {display}");
}
