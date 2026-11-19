mod common;

use common::{analyze_one, table};
use dlin_sqllineage::{Ident, Relation};

#[test]
fn bare_select_single_table() {
    let result = analyze_one("SELECT a, b FROM t1");
    assert_eq!(result.written, None);
    assert_eq!(result.read, vec![table("t1")]);
}

#[test]
fn bare_select_join() {
    let sql = "SELECT t1.a, t2.b FROM t1 JOIN t2 ON t1.id = t2.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, None);
    let mut inputs = result.read;
    inputs.sort();
    assert_eq!(inputs, vec![table("t1"), table("t2")]);
}

#[test]
fn bare_select_subquery_in_from() {
    let result = analyze_one("SELECT x FROM (SELECT a FROM src) sub");
    assert_eq!(result.read, vec![table("src")]);
}

#[test]
fn insert_into_select() {
    let result = analyze_one("INSERT INTO target SELECT a, b FROM source");
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);
}

#[test]
fn insert_into_select_join() {
    let sql = "INSERT INTO target SELECT a.x, b.y FROM a JOIN b ON a.id = b.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    let mut inputs = result.read;
    inputs.sort();
    assert_eq!(inputs, vec![table("a"), table("b")]);
}

#[test]
fn create_table_as_select() {
    let result = analyze_one("CREATE TABLE target AS SELECT a, b FROM source");
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);
}

#[test]
fn create_table_as_select_join() {
    let sql = "CREATE TABLE target AS SELECT s1.a, s2.b FROM s1 JOIN s2 ON s1.id = s2.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    let mut inputs = result.read;
    inputs.sort();
    assert_eq!(inputs, vec![table("s1"), table("s2")]);
}

#[test]
fn update_with_from() {
    let sql = "UPDATE target SET col1 = source.col1 FROM source WHERE target.id = source.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);
}

#[test]
fn update_with_from_join() {
    let sql =
        "UPDATE target SET col1 = s1.col1 FROM s1 JOIN s2 ON s1.id = s2.id WHERE target.id = s1.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    let mut inputs = result.read;
    inputs.sort();
    assert_eq!(inputs, vec![table("s1"), table("s2")]);
}

#[test]
fn delete_using() {
    let sql = "DELETE FROM target USING source WHERE target.id = source.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);
}

#[test]
fn delete_using_join() {
    let sql = "DELETE FROM target USING s1 JOIN s2 ON s1.id = s2.id WHERE target.id = s1.id";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    let mut inputs = result.read;
    inputs.sort();
    assert_eq!(inputs, vec![table("s1"), table("s2")]);
}

#[test]
fn merge_into_using() {
    let sql = "\
        MERGE INTO target \
        USING source ON target.id = source.id \
        WHEN MATCHED THEN UPDATE SET target.col = source.col \
        WHEN NOT MATCHED THEN INSERT (col) VALUES (source.col)";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);
}

#[test]
fn merge_into_using_subquery() {
    let sql = "\
        MERGE INTO target \
        USING (SELECT id, col FROM source) AS src ON target.id = src.id \
        WHEN MATCHED THEN UPDATE SET target.col = src.col";
    let result = analyze_one(sql);
    assert_eq!(result.written, Some(table("target")));
    assert_eq!(result.read, vec![table("source")]);
}

#[test]
fn select_with_schema_qualified_table() {
    let result = analyze_one("SELECT a FROM myschema.mytable");
    assert_eq!(
        result.read,
        vec![Relation {
            catalog: None,
            schema: Some(Ident::unquoted("myschema")),
            namespace: None,
            name: Ident::unquoted("mytable"),
        }]
    );
}

#[test]
fn non_lineage_ddl_returns_empty() {
    let result = analyze_one("DROP TABLE foo");
    assert_eq!(result.written, None);
    assert!(result.read.is_empty());
}

/// `relation` as `catalog|schema|namespace|name`, with `-` for a missing part.
fn relation_parts(relation: &Relation) -> String {
    format!(
        "{}|{}|{}|{}",
        relation
            .catalog
            .as_ref()
            .map_or("-", |part| part.value.as_str()),
        relation
            .schema
            .as_ref()
            .map_or("-", |part| part.value.as_str()),
        relation
            .namespace
            .map_or("-", |namespace| namespace.sql_name()),
        relation.name.value
    )
}

#[test]
fn bigquery_information_schema_views_keep_their_scope() {
    // Quoting the region and the namespace together is split by the parser
    // like quoting them apart; a project is optional, and a dataset scopes
    // dataset views the way a region scopes region views.
    for (sql, expected) in [
        (
            "SELECT job_id FROM `p`.`region-us.INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`",
            "p|region-us|INFORMATION_SCHEMA|JOBS_BY_PROJECT",
        ),
        (
            "SELECT job_id FROM `p`.`region-us`.`INFORMATION_SCHEMA`.`JOBS_BY_PROJECT`",
            "p|region-us|INFORMATION_SCHEMA|JOBS_BY_PROJECT",
        ),
        (
            "SELECT job_id FROM `region-us`.INFORMATION_SCHEMA.JOBS_BY_PROJECT",
            "-|region-us|INFORMATION_SCHEMA|JOBS_BY_PROJECT",
        ),
        (
            "SELECT table_name FROM p.analytics.information_schema.TABLES",
            "p|analytics|INFORMATION_SCHEMA|TABLES",
        ),
    ] {
        let result = common::analyze_all(sql, dlin_sqllineage::Dialect::BigQuery)
            .into_single()
            .unwrap();
        assert_eq!(
            result.read.iter().map(relation_parts).collect::<Vec<_>>(),
            [expected],
            "{sql}"
        );
    }
}

#[test]
fn information_schema_is_a_schema_outside_bigquery() {
    let result = common::analyze_all(
        "SELECT table_name FROM analytics.information_schema.tables",
        dlin_sqllineage::Dialect::Snowflake,
    )
    .into_single()
    .unwrap();
    assert_eq!(
        result.read.iter().map(relation_parts).collect::<Vec<_>>(),
        ["analytics|information_schema|-|tables"]
    );
}
