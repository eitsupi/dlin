use sqllineage::{AnalyzeOptions, CatalogProvider, ColumnOrigin, TableRef, analyze};

fn first_result(sql: &str, catalog: Option<Box<dyn CatalogProvider>>) -> sqllineage::AnalyzeResult {
    analyze(
        sql,
        AnalyzeOptions {
            catalog,
            ..AnalyzeOptions::default()
        },
    )
    .expect("analysis")
    .into_iter()
    .next()
    .expect("one statement")
}

struct ExtCatalog;

impl CatalogProvider for ExtCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        (table.table == "ext_a").then(|| vec!["col_x".into(), "col_y".into()])
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        None
    }
}

#[test]
fn named_projection_through_unknown_star_keeps_requested_column_and_uncertainty() {
    for sql in [
        "WITH src AS (SELECT * FROM some_unknown_source) SELECT id FROM src",
        "SELECT id FROM (SELECT * FROM some_unknown_source) src",
    ] {
        let result = first_result(sql, None);
        assert!(result.columns.has_unresolved_stars, "SQL: {sql}");
        assert!(matches!(
            result.columns.mappings[0].sources.as_slice(),
            [ColumnOrigin::NamedWildcard { table, column }]
                if table.table == "some_unknown_source" && column == "id"
        ));
    }
}

#[test]
fn nested_join_star_is_publicly_marked_even_when_output_is_from_base() {
    let result = first_result(
        "SELECT id FROM some_table JOIN (SELECT * FROM some_unknown_source) src ON 1=1",
        None,
    );
    assert!(result.columns.has_unresolved_stars);
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Concrete { table, column }]
            if table.table == "some_table" && column == "id"
    ));
}

#[test]
fn leading_unknown_star_does_not_publish_nonleading_only_set_names() {
    let result = first_result(
        "SELECT * FROM unknown_source UNION ALL SELECT id, amt AS total FROM known_table UNION ALL SELECT id, fee FROM third_table",
        None,
    );
    let names = result
        .columns
        .mappings
        .iter()
        .map(|mapping| mapping.target.column.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["*", "id", "total"]);
}

#[test]
fn source_free_set_branch_is_retained_as_incomplete_lineage() {
    let result = first_result(
        "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c1, 2 AS c2 UNION ALL SELECT a.col_x, a.col_y FROM a) SELECT c1 FROM u",
        Some(Box::new(ExtCatalog)),
    );
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [
            ColumnOrigin::Concrete { table, column },
            ColumnOrigin::SourceFree { column: marker }
        ] if marker == "c1" && table.table == "ext_a" && column == "col_x"
    ));
}
