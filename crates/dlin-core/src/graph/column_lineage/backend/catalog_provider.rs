use std::borrow::Cow;

use super::catalog::CatalogSnapshot;
use super::dialect::DlinDialect;
use crate::graph::column_lineage::relation::{Identifier, RelationRef};

pub(crate) struct CatalogAdapter<'a> {
    snapshot: &'a CatalogSnapshot,
    dialect: DlinDialect,
}

impl<'a> CatalogAdapter<'a> {
    pub(crate) const fn new(snapshot: &'a CatalogSnapshot, dialect: DlinDialect) -> Self {
        Self { snapshot, dialect }
    }
}

impl sqllineage::Catalog for CatalogAdapter<'_> {
    fn lookup(&self, relation: &sqllineage::Relation) -> Option<sqllineage::CatalogEntry<'_>> {
        let query = relation_ref_from_engine(relation);
        let table = self
            .snapshot
            .resolve_table_exact(&query, self.dialect)
            .or_else(|| {
                allows_case_folding(&query, self.dialect)
                    .then(|| self.snapshot.resolve_table_exact_case_insensitive(&query))
                    .flatten()
            })?;
        let columns = self
            .snapshot
            .unambiguous_columns_for_relation(&table.relation)
            .filter(|columns| !columns.is_empty())?;
        Some(sqllineage::CatalogEntry {
            relation: relation_from_ref(&table.relation),
            columns: Cow::Owned(
                columns
                    .iter()
                    .map(|name| sqllineage::CatalogColumn {
                        name: sqllineage::Ident::unquoted(name.clone()),
                        data_type: None,
                        fields: None,
                    })
                    .collect(),
            ),
        })
    }
}

pub(crate) fn relation_ref_from_engine(relation: &sqllineage::Relation) -> RelationRef {
    use crate::graph::column_lineage::relation::QuoteStyle;
    use sqllineage::{Ident, Quote};

    let component = |ident: &Ident| match ident.quote {
        Some(Quote::Double | Quote::Bracket | Quote::Single) => {
            Identifier::quoted(ident.value.clone(), QuoteStyle::Double)
        }
        Some(Quote::Backtick) => Identifier::quoted(ident.value.clone(), QuoteStyle::Backtick),
        None => Identifier::unquoted(ident.value.clone()),
    };
    RelationRef::from_identifiers(
        relation.catalog.as_ref().map(component),
        relation.schema.as_ref().map(component),
        component(&relation.name),
    )
}

/// Keep dlin's source spelling stable while the engine retains SQL quote metadata.
pub(crate) fn source_relation(
    relation: &sqllineage::Relation,
    catalog: Option<&CatalogSnapshot>,
    dialect: DlinDialect,
) -> RelationRef {
    let query = relation_ref_from_engine(relation);
    if let Some(table) = catalog.and_then(|catalog| {
        catalog.resolve_table_exact(&query, dialect).or_else(|| {
            allows_case_folding(&query, dialect)
                .then(|| catalog.resolve_table_exact_case_insensitive(&query))
                .flatten()
        })
    }) {
        return table.relation.clone();
    }
    let part = |ident: &sqllineage::Ident| {
        if ident.quote.is_some() {
            ident.value.clone()
        } else {
            ident.value.to_lowercase()
        }
    };
    let catalog = relation.catalog.as_ref().map(part);
    let schema = relation.schema.as_ref().map(part);
    let name = part(&relation.name);
    RelationRef::from_backend(catalog.as_deref(), schema.as_deref(), &name)
}

fn relation_from_ref(relation: &RelationRef) -> sqllineage::Relation {
    use sqllineage::{Ident, Quote};
    let ident = |part: &Identifier| match part.quote_style() {
        crate::graph::column_lineage::relation::QuoteStyle::Double => {
            Ident::quoted(part.value().to_owned(), Quote::Double)
        }
        crate::graph::column_lineage::relation::QuoteStyle::Backtick => {
            Ident::quoted(part.value().to_owned(), Quote::Backtick)
        }
        crate::graph::column_lineage::relation::QuoteStyle::Unquoted
        | crate::graph::column_lineage::relation::QuoteStyle::Unknown => {
            Ident::unquoted(part.value().to_owned())
        }
    };
    sqllineage::Relation {
        catalog: relation.catalog().map(ident),
        schema: relation.schema().map(ident),
        name: ident(relation.name()),
    }
}

fn allows_case_folding(query: &RelationRef, dialect: DlinDialect) -> bool {
    use crate::graph::column_lineage::relation::QuoteStyle;
    matches!(dialect, DlinDialect::Generic | DlinDialect::BigQuery)
        && query
            .catalog()
            .into_iter()
            .chain(query.schema())
            .chain(std::iter::once(query.name()))
            .all(|part| part.quote_style() == QuoteStyle::Unquoted)
}

#[cfg(test)]
mod source_relation_tests {
    use super::*;

    #[test]
    fn quoted_sources_do_not_use_case_insensitive_catalog_fallback() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("Users", ["id".to_string()]);
        for dialect in [DlinDialect::Generic, DlinDialect::BigQuery] {
            let mut relation = sqllineage::Relation {
                catalog: None,
                schema: None,
                name: sqllineage::Ident::quoted("users", sqllineage::Quote::Double),
            };
            assert_eq!(
                source_relation(&relation, Some(&catalog), dialect)
                    .name()
                    .value(),
                "users"
            );
            relation.name = sqllineage::Ident::unquoted("users");
            assert_eq!(
                source_relation(&relation, Some(&catalog), dialect)
                    .name()
                    .value(),
                "Users"
            );
        }
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct SqllineageCatalogProvider {
    snapshot: CatalogSnapshot,
    dialect: DlinDialect,
}

#[cfg(test)]
impl SqllineageCatalogProvider {
    pub(crate) fn new(snapshot: &CatalogSnapshot, dialect: DlinDialect) -> Self {
        Self {
            snapshot: snapshot.clone(),
            dialect,
        }
    }
}

#[cfg(test)]
impl sqllineage::Catalog for SqllineageCatalogProvider {
    fn lookup(&self, relation: &sqllineage::Relation) -> Option<sqllineage::CatalogEntry<'_>> {
        let adapter = CatalogAdapter::new(&self.snapshot, self.dialect);
        let entry = <CatalogAdapter<'_> as sqllineage::Catalog>::lookup(&adapter, relation)?;
        Some(sqllineage::CatalogEntry {
            relation: entry.relation,
            columns: Cow::Owned(entry.columns.into_owned()),
        })
    }
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TableRef {
    pub catalog: Option<String>,
    pub schema: Option<String>,
    pub table: String,
}

#[cfg(test)]
impl SqllineageCatalogProvider {
    pub(crate) fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        let query = RelationRef::from_backend_lookup(
            table.catalog.as_deref(),
            table.schema.as_deref(),
            &table.table,
        );
        let found = match self.dialect {
            DlinDialect::Generic | DlinDialect::BigQuery => {
                self.snapshot.resolve_table_exact_case_insensitive(&query)
            }
            _ => self.snapshot.resolve_table_exact(&query, self.dialect),
        }?;
        self.snapshot
            .unambiguous_columns_for_relation(&found.relation)
            .filter(|columns| !columns.is_empty())
            .map(<[String]>::to_vec)
    }

    pub(crate) fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        let matching = candidates
            .iter()
            .filter(|candidate| {
                self.list_columns(candidate).is_some_and(|columns| {
                    columns
                        .iter()
                        .any(|known| known.to_lowercase() == column.to_lowercase())
                })
            })
            .collect::<Vec<_>>();
        (matching.len() == 1).then(|| (*matching[0]).clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(catalog: Option<&str>, schema: Option<&str>, table: &str) -> TableRef {
        TableRef {
            catalog: catalog.map(str::to_string),
            schema: schema.map(str::to_string),
            table: table.to_string(),
        }
    }

    fn catalog() -> CatalogSnapshot {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("orders", ["id".to_string()]);
        snapshot.add_table("analytics.orders", ["id".to_string(), "total".to_string()]);
        snapshot.add_table(
            "warehouse.analytics.orders",
            ["id".to_string(), "total".to_string(), "region".to_string()],
        );
        snapshot
    }

    #[test]
    fn list_columns_requires_exact_relation_arity() {
        let snapshot = catalog();
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert_eq!(
            provider
                .list_columns(&table(None, None, "orders"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            provider
                .list_columns(&table(None, Some("analytics"), "orders"))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            provider
                .list_columns(&table(Some("warehouse"), Some("analytics"), "orders"))
                .unwrap()
                .len(),
            3
        );
        assert!(
            provider
                .list_columns(&table(None, Some("warehouse"), "analytics.orders"))
                .is_none()
        );
        assert!(
            provider
                .list_columns(&table(None, Some("analytics"), "missing"))
                .is_none()
        );
    }

    #[test]
    fn list_columns_folds_case_beyond_ascii() {
        let mut snapshot = CatalogSnapshot::new();
        // sqllineage lowercases an unquoted identifier with `to_lowercase`, so a
        // cased non-ASCII relation reaches the provider already folded.
        snapshot.add_table("Заказы", ["id".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Snowflake);

        assert!(
            provider
                .list_columns(&table(None, None, "заказы"))
                .is_some()
        );
    }

    #[test]
    fn list_columns_matches_bigquery_identifiers_case_insensitively() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("Warehouse.Analytics.Orders", ["id".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::BigQuery);

        assert!(
            provider
                .list_columns(&table(Some("WAREHOUSE"), Some("ANALYTICS"), "ORDERS"))
                .is_some()
        );
    }

    #[test]
    fn list_columns_matches_mixed_case_generic_relation_after_backend_lowercasing() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("Warehouse.Analytics.Orders", ["id".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert!(
            provider
                .list_columns(&table(Some("warehouse"), Some("analytics"), "orders"))
                .is_some()
        );
    }

    #[test]
    fn list_columns_rejects_empty_and_conflicted_tables() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("empty", Vec::<String>::new());
        snapshot.add_table("conflicted", ["id".to_string()]);
        snapshot.add_table("conflicted", ["other".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert!(provider.list_columns(&table(None, None, "empty")).is_none());
        assert!(
            provider
                .list_columns(&table(None, None, "conflicted"))
                .is_none()
        );
    }

    #[test]
    fn resolve_column_requires_exactly_one_qualifying_candidate() {
        let snapshot = catalog();
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);
        let one = table(None, None, "orders");
        let two = table(None, Some("analytics"), "orders");
        let unknown = table(None, None, "missing");

        assert_eq!(
            provider.resolve_column("id", std::slice::from_ref(&one)),
            Some(one.clone())
        );
        assert!(
            provider
                .resolve_column("id", &[one.clone(), two.clone()])
                .is_none()
        );
        assert!(provider.resolve_column("id", &[unknown]).is_none());
        assert!(provider.resolve_column("missing", &[one]).is_none());
    }

    #[test]
    fn resolve_column_folds_catalog_column_case() {
        let mut snapshot = CatalogSnapshot::new();
        snapshot.add_table("orders", ["ID".to_string()]);
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::Generic);

        assert_eq!(
            provider.resolve_column("id", &[table(None, None, "orders")]),
            Some(table(None, None, "orders"))
        );
    }
}
