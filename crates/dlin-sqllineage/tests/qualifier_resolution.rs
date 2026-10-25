//! Multi-part column qualifiers are resolved against the relations in scope.
//! A qualifier that names no relation must never be turned into a table by
//! joining its text.

use dlin_sqllineage::{AnalyzeOptions, AnalyzeResult, ColumnOrigin, Dialect, TableRef, analyze};

fn run(sql: &str, dialect: Dialect) -> AnalyzeResult {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            ..AnalyzeOptions::default()
        },
    )
    .unwrap_or_else(|err| panic!("{dialect:?}: {sql}: {err:?}"))
    .remove(0)
}

fn schema_table(schema: &str, table: &str) -> TableRef {
    TableRef::with_schema(schema, table)
}

fn sources<'a>(result: &'a AnalyzeResult, target: &str) -> &'a [ColumnOrigin] {
    &result
        .columns
        .mappings
        .iter()
        .find(|mapping| mapping.target.column == target)
        .unwrap_or_else(|| panic!("no mapping for {target}"))
        .sources
}

fn assert_concrete(result: &AnalyzeResult, target: &str, expected: &TableRef, column: &str) {
    match sources(result, target) {
        [
            ColumnOrigin::Concrete {
                table,
                column: actual,
            },
        ] => {
            assert_eq!(table, expected, "table of {target}");
            assert_eq!(actual, column, "column of {target}");
        }
        other => panic!("expected Concrete for {target}, got {other:?}"),
    }
}

fn assert_unresolved(result: &AnalyzeResult, target: &str, column: &str) {
    match sources(result, target) {
        [
            ColumnOrigin::Ambiguous {
                column: actual,
                candidates,
            },
        ] if candidates.is_empty() => assert_eq!(actual, column),
        other => panic!("expected unresolved Ambiguous for {target}, got {other:?}"),
    }
}

const DIALECTS: [Dialect; 5] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::Snowflake,
    Dialect::BigQuery,
    Dialect::DuckDb,
];

#[test]
fn schema_qualified_column_resolves_to_the_relation() {
    for dialect in DIALECTS {
        let result = run("SELECT sch.tbl.col FROM sch.tbl", dialect);
        assert_concrete(&result, "col", &schema_table("sch", "tbl"), "col");
        assert_eq!(result.tables.inputs, [schema_table("sch", "tbl")]);
    }
}

#[test]
fn table_name_and_alias_qualifiers_resolve_to_the_schema_relation() {
    for dialect in DIALECTS {
        let unaliased = run("SELECT tbl.col FROM sch.tbl", dialect);
        assert_concrete(&unaliased, "col", &schema_table("sch", "tbl"), "col");
        let aliased = run("SELECT x.col FROM sch.tbl AS x", dialect);
        assert_concrete(&aliased, "col", &schema_table("sch", "tbl"), "col");
        let full = run("SELECT sch.tbl.col FROM sch.tbl AS x", dialect);
        assert_concrete(&full, "col", &schema_table("sch", "tbl"), "col");
    }
}

#[test]
fn fully_qualified_columns_in_a_join_keep_their_schemas() {
    for dialect in DIALECTS {
        let result = run(
            "SELECT sch.tbl.a, sch.other.b FROM sch.tbl JOIN sch.other ON sch.tbl.id = sch.other.id",
            dialect,
        );
        assert_concrete(&result, "a", &schema_table("sch", "tbl"), "a");
        assert_concrete(&result, "b", &schema_table("sch", "other"), "b");
    }
}

#[test]
fn catalog_qualified_column_resolves_to_the_relation() {
    for dialect in DIALECTS {
        let result = run("SELECT cat.sch.tbl.col FROM cat.sch.tbl", dialect);
        let expected = TableRef {
            catalog: Some("cat".into()),
            schema: Some("sch".into()),
            table: "tbl".into(),
        };
        assert_concrete(&result, "col", &expected, "col");
    }
}

/// Two relations that share a last name but live in different schemas are
/// both reachable by their full names.
#[test]
fn same_table_name_in_two_schemas_is_told_apart_by_the_schema_qualifier() {
    for dialect in DIALECTS {
        let result = run(
            "SELECT s.t.a AS a, o.t.b AS b FROM s.t JOIN o.t ON s.t.id = o.t.id",
            dialect,
        );
        assert_concrete(&result, "a", &schema_table("s", "t"), "a");
        assert_concrete(&result, "b", &schema_table("o", "t"), "b");
    }
}

/// A qualifier with more parts than any relation name has cannot be bound.
/// The old code joined the parts into one table name and reported it as a
/// confident source.
#[test]
fn overlong_qualifier_without_a_relation_is_unresolved() {
    for dialect in DIALECTS {
        for sql in ["SELECT a.b.c.d.e", "SELECT a.b.c.d.e AS e"] {
            let result = run(sql, dialect);
            assert_unresolved(&result, "e", "e");
        }
    }
}

/// The relation qualifier forms that are valid without a FROM clause keep
/// their structured relation.
#[test]
fn short_qualifier_without_from_keeps_structured_relation() {
    let result = run("SELECT orders.id AS id", Dialect::Generic);
    assert_concrete(&result, "id", &TableRef::new("orders"), "id");
    let result = run("SELECT raw.orders.id AS id", Dialect::Generic);
    assert_concrete(&result, "id", &schema_table("raw", "orders"), "id");
}
