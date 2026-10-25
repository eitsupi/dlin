use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, Ident, Lineage, Output, OutputColumn,
    OutputName, Relation, SourceColumn, Unresolved,
};
use std::borrow::Cow;

#[allow(dead_code)]
pub fn analyze_one(sql: &str) -> dlin_sqllineage::StatementAnalysis {
    analyze_all(sql, dlin_sqllineage::Dialect::Generic)
        .into_single()
        .expect("SQL should contain one statement")
}

#[allow(dead_code)]
pub fn analyze_all(sql: &str, dialect: dlin_sqllineage::Dialect) -> dlin_sqllineage::Analysis {
    Analyzer::new(dialect)
        .analyze(sql)
        .expect("SQL should parse")
}

#[allow(dead_code)]
pub fn analyze_with_catalog<C: Catalog>(
    sql: &str,
    dialect: dlin_sqllineage::Dialect,
    catalog: &C,
) -> dlin_sqllineage::StatementAnalysis {
    Analyzer::new(dialect)
        .with_catalog(catalog)
        .analyze(sql)
        .expect("SQL should parse")
        .into_single()
        .expect("SQL should contain one statement")
}

#[allow(dead_code)]
pub fn analyze_one_with(
    sql: &str,
    dialect: dlin_sqllineage::Dialect,
    catalog: Option<&dyn Catalog>,
) -> dlin_sqllineage::StatementAnalysis {
    let analyzer = Analyzer::new(dialect);
    let analyzer = if let Some(catalog) = catalog {
        analyzer.with_catalog(catalog)
    } else {
        analyzer
    };
    analyzer
        .analyze(sql)
        .expect("SQL should parse")
        .into_single()
        .expect("SQL should contain one statement")
}

#[allow(dead_code)]
pub fn table(name: &str) -> Relation {
    Relation {
        catalog: None,
        schema: None,
        name: Ident::unquoted(name),
    }
}

#[allow(dead_code)]
pub fn find_mapping<'a>(columns: &'a [OutputColumn], name: &str) -> &'a OutputColumn {
    columns
        .iter()
        .find(|column| match &column.name {
            OutputName::Explicit(ident) | OutputName::Inferred(ident) => ident.value == name,
            OutputName::Anonymous => false,
        })
        .unwrap_or_else(|| panic!("no output column named '{name}'"))
}

#[allow(dead_code)]
pub fn output_name(column: &OutputColumn) -> Option<&str> {
    match &column.name {
        OutputName::Explicit(ident) | OutputName::Inferred(ident) => Some(&ident.value),
        OutputName::Anonymous => None,
    }
}

#[allow(dead_code)]
pub fn concrete_sources(column: &OutputColumn) -> Vec<(String, String)> {
    let sources = match &column.lineage {
        Lineage::Complete(sources) => sources,
        other => panic!("expected complete lineage, got {other:?}"),
    };
    let mut values: Vec<_> = sources
        .iter()
        .map(|source| {
            (
                source.relation.name.value.clone(),
                source.column.value.clone(),
            )
        })
        .collect();
    values.sort();
    values
}

#[allow(dead_code)]
pub fn known_sources(column: &OutputColumn) -> &[SourceColumn] {
    match &column.lineage {
        Lineage::Complete(sources) | Lineage::Partial { known: sources, .. } => sources,
    }
}

#[allow(dead_code)]
pub fn unresolved(column: &OutputColumn) -> &[Unresolved] {
    match &column.lineage {
        Lineage::Complete(_) => &[],
        Lineage::Partial { unresolved, .. } => unresolved,
    }
}

#[allow(dead_code)]
pub fn output_names(output: &Output) -> Vec<Option<&str>> {
    output
        .columns
        .iter()
        .map(|column| match &column.name {
            OutputName::Explicit(ident) | OutputName::Inferred(ident) => Some(ident.value.as_str()),
            OutputName::Anonymous => None,
        })
        .collect()
}

#[allow(dead_code)]
pub fn catalog_entry<'a>(relation: &Relation, columns: &'a [&str]) -> CatalogEntry<'a> {
    CatalogEntry {
        relation: relation.clone(),
        columns: Cow::Owned(
            columns
                .iter()
                .map(|column| CatalogColumn {
                    name: Ident::unquoted(*column),
                    data_type: None,
                    fields: None,
                })
                .collect(),
        ),
    }
}
