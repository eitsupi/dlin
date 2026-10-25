use common::output_name;
use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, Dialect, Ident, Lineage, Relation,
    StatementAnalysis, Unresolved,
};
use std::borrow::Cow;
mod common;

fn first_result(sql: &str, catalog: Option<&dyn Catalog>) -> StatementAnalysis {
    let analyzer = Analyzer::new(Dialect::Generic);
    let analyzer = if let Some(catalog) = catalog {
        analyzer.with_catalog(catalog)
    } else {
        analyzer
    };
    analyzer
        .analyze(sql)
        .expect("analysis")
        .into_single()
        .expect("one statement")
}

struct ExtCatalog;

impl Catalog for ExtCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name.value == "ext_a").then(|| CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(
                ["col_x", "col_y"]
                    .into_iter()
                    .map(|name| CatalogColumn {
                        name: Ident::unquoted(name),
                        data_type: None,
                        fields: None,
                    })
                    .collect(),
            ),
        })
    }
}

#[test]
fn named_projection_through_unknown_star_keeps_requested_column_and_uncertainty() {
    for sql in [
        "WITH src AS (SELECT * FROM some_unknown_source) SELECT id FROM src",
        "SELECT id FROM (SELECT * FROM some_unknown_source) src",
    ] {
        let result = first_result(sql, None);
        assert!(
            matches!(&result.output.shape, dlin_sqllineage::Shape::Closed),
            "SQL: {sql}"
        );
        assert!(matches!(
            &result.output.columns[0].lineage,
            Lineage::Partial { unresolved, .. }
                if matches!(&unresolved[..], [Unresolved::NamedBehindStar { relation, column }]
                    if relation.name.value == "some_unknown_source" && column.value == "id")
        ));
    }
}

#[test]
fn nested_join_star_does_not_open_a_closed_root_projection() {
    let result = first_result(
        "SELECT id FROM some_table JOIN (SELECT * FROM some_unknown_source) src ON 1=1",
        None,
    );
    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Closed
    ));
    assert!(
        matches!(&result.output.columns[0].lineage, Lineage::Complete(sources)
        if matches!(&sources[..], [source]
            if source.relation.name.value == "some_table" && source.column.value == "id"))
    );
}

#[test]
fn leading_unknown_star_does_not_publish_nonleading_only_set_names() {
    let result = first_result(
        "SELECT * FROM unknown_source UNION ALL SELECT id, amt AS total FROM known_table UNION ALL SELECT id, fee FROM third_table",
        None,
    );
    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    let names = result
        .output
        .columns
        .iter()
        .map(|mapping| output_name(mapping).unwrap_or(""))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["id", "total"]);
}

#[test]
fn source_free_set_branch_is_retained_as_incomplete_lineage() {
    let result = first_result(
        "WITH a AS (SELECT * FROM ext_a), u AS (SELECT 1 AS c1, 2 AS c2 UNION ALL SELECT a.col_x, a.col_y FROM a) SELECT c1 FROM u",
        Some(&ExtCatalog),
    );
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Complete(sources) if matches!(&sources[..], [source]
            if source.relation.name.value == "ext_a" && source.column.value == "col_x")
    ));
}
