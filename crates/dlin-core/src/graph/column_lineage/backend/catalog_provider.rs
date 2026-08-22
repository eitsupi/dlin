use sqllineage::{CatalogProvider, TableRef};

use super::{CatalogSnapshot, DlinDialect};

/// Adapts dlin's manifest catalog to sqllineage's catalog interface.
pub(crate) struct SqllineageCatalogProvider {
    snapshot: CatalogSnapshot,
    dialect: DlinDialect,
}

impl SqllineageCatalogProvider {
    pub(crate) fn new(snapshot: &CatalogSnapshot, dialect: DlinDialect) -> Self {
        Self {
            snapshot: snapshot.clone(),
            dialect,
        }
    }

    fn table_parts(table: &TableRef) -> Vec<&str> {
        match (table.catalog.as_deref(), table.schema.as_deref()) {
            (Some(catalog), Some(schema)) => vec![catalog, schema, table.table.as_str()],
            (None, Some(schema)) => vec![schema, table.table.as_str()],
            _ => vec![table.table.as_str()],
        }
    }

    fn relation_matches(&self, key: &str, table: &TableRef) -> bool {
        let table_parts = Self::table_parts(table);
        let key_parts: Vec<&str> = key.split('.').collect();
        key_parts.len() == table_parts.len()
            && key_parts
                .iter()
                .enumerate()
                .all(|(index, part)| identifiers_match(part, table_parts[index], self.dialect))
    }

    fn columns_for(&self, table: &TableRef) -> Option<&[String]> {
        let matching: Vec<&super::catalog::CatalogTable> = self
            .snapshot
            .tables()
            .filter(|candidate| self.relation_matches(&candidate.name, table))
            .collect();
        let [candidate] = matching.as_slice() else {
            return None;
        };
        self.snapshot
            .unambiguous_table_columns(&candidate.name)
            .filter(|columns| !columns.is_empty())
    }
}

/// Compare one relation name part. Every dialect folds case here, so the rule
/// lives in a single place if a dialect ever needs its own.
///
/// The folding matches what sqllineage already did to the identifier on its way
/// into a `TableRef`: `str::to_lowercase`, not an ASCII-only fold. An ASCII fold
/// would miss a cased non-ASCII identifier that sqllineage had already
/// lowercased, and the relation would silently look unknown.
///
/// `TableRef` carries no quote metadata, so Snowflake `foo` and `"foo"` are
/// indistinguishable here. Matching is the deliberate choice over guessing at
/// the original quoting.
pub(crate) fn identifiers_match(left: &str, right: &str, _dialect: DlinDialect) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

impl CatalogProvider for SqllineageCatalogProvider {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        self.columns_for(table).map(<[String]>::to_vec)
    }

    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        let qualifying: Vec<&TableRef> = candidates
            .iter()
            .filter(|candidate| {
                self.columns_for(candidate).is_some_and(|columns| {
                    columns
                        .iter()
                        .any(|known| identifiers_match(known, column, self.dialect))
                })
            })
            .collect();
        (qualifying.len() == 1).then(|| qualifying[0].clone())
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
        let snapshot = catalog();
        let provider = SqllineageCatalogProvider::new(&snapshot, DlinDialect::BigQuery);

        assert!(
            provider
                .list_columns(&table(Some("WAREHOUSE"), Some("ANALYTICS"), "ORDERS"))
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
