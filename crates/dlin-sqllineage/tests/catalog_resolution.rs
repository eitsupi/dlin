use std::borrow::Cow;

use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, Dialect, Ident, Lineage, OutputName, Quote,
    Relation, SourceColumn,
};

struct CanonicalCatalog;

impl Catalog for CanonicalCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name == Ident::unquoted("source")).then(|| CatalogEntry {
            relation: Relation {
                catalog: Some(Ident::quoted("Warehouse", Quote::Backtick)),
                schema: Some(Ident::quoted("Analytics", Quote::Backtick)),
                name: Ident::quoted("EventLog", Quote::Backtick),
            },
            columns: Cow::Owned(vec![CatalogColumn {
                name: Ident::quoted("MiXeD", Quote::Backtick),
                data_type: None,
                fields: None,
            }]),
        })
    }
}

fn one(dialect: Dialect, sql: &str, catalog: &dyn Catalog) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(dialect)
        .with_catalog(catalog)
        .analyze(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error:?}"))
        .into_single()
        .unwrap()
}

fn canonical_source(column: Ident) -> SourceColumn {
    SourceColumn {
        relation: Relation {
            catalog: Some(Ident::quoted("Warehouse", Quote::Backtick)),
            schema: Some(Ident::quoted("Analytics", Quote::Backtick)),
            name: Ident::quoted("EventLog", Quote::Backtick),
        },
        column,
    }
}

#[test]
fn qualified_reference_uses_catalog_column_and_relation_spelling() {
    let result = one(
        Dialect::BigQuery,
        "SELECT s.`mIxEd` FROM source AS s",
        &CanonicalCatalog,
    );

    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::quoted("mIxEd", Quote::Backtick))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![canonical_source(Ident::quoted(
            "MiXeD",
            Quote::Backtick
        ))])
    );
}

#[test]
fn catalog_column_spelling_and_relation_survive_a_cte() {
    let result = one(
        Dialect::BigQuery,
        "WITH events AS (SELECT s.`mIxEd` AS x FROM source AS s) \
         SELECT events.x FROM events",
        &CanonicalCatalog,
    );

    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("x"))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![canonical_source(Ident::quoted(
            "MiXeD",
            Quote::Backtick
        ))])
    );
}

#[test]
fn catalog_star_uses_canonical_column_and_relation_spelling_through_alias() {
    let result = one(
        Dialect::Generic,
        "SELECT s.* FROM source AS s",
        &CanonicalCatalog,
    );

    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::quoted("MiXeD", Quote::Backtick))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![canonical_source(Ident::quoted(
            "MiXeD",
            Quote::Backtick
        ))])
    );
}

struct DistinctGenericColumns;

impl Catalog for DistinctGenericColumns {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name == Ident::unquoted("items")).then(|| CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(vec![
                CatalogColumn {
                    name: Ident::quoted("id", Quote::Double),
                    data_type: None,
                    fields: None,
                },
                CatalogColumn {
                    name: Ident::unquoted("ID"),
                    data_type: None,
                    fields: None,
                },
            ]),
        })
    }
}

#[test]
fn generic_catalog_membership_keeps_quoted_lowercase_id_distinct_from_unquoted_id() {
    let result = one(
        Dialect::Generic,
        "SELECT * FROM items",
        &DistinctGenericColumns,
    );

    assert_eq!(result.output.columns.len(), 2);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::quoted("id", Quote::Double))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![canonical_source_for_items(Ident::quoted(
            "id",
            Quote::Double
        ))])
    );
    assert_eq!(
        result.output.columns[1].name,
        OutputName::Explicit(Ident::unquoted("ID"))
    );
    assert_eq!(
        result.output.columns[1].lineage,
        Lineage::Complete(vec![canonical_source_for_items(Ident::unquoted("ID"))])
    );
}

fn canonical_source_for_items(column: Ident) -> SourceColumn {
    SourceColumn {
        relation: Relation {
            catalog: None,
            schema: None,
            name: Ident::unquoted("items"),
        },
        column,
    }
}

struct NaturalJoinColumns;

impl Catalog for NaturalJoinColumns {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        let column = if relation.name == Ident::unquoted("a") {
            Ident::quoted("id", Quote::Double)
        } else if relation.name == Ident::unquoted("b") {
            Ident::unquoted("ID")
        } else {
            return None;
        };

        Some(CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Owned(vec![CatalogColumn {
                name: column,
                data_type: None,
                fields: None,
            }]),
        })
    }
}

#[test]
fn generic_natural_join_membership_uses_identifier_keys() {
    let result = one(
        Dialect::Generic,
        "SELECT id FROM a NATURAL JOIN b",
        &NaturalJoinColumns,
    );

    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        result.output.columns[0].name,
        OutputName::Explicit(Ident::unquoted("id"))
    );
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![SourceColumn {
            relation: Relation {
                catalog: None,
                schema: None,
                name: Ident::unquoted("b"),
            },
            column: Ident::unquoted("ID"),
        }])
    );
}
