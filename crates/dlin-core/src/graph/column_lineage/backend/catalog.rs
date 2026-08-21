#![allow(dead_code)]

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogSnapshot {
    tables: BTreeMap<String, CatalogTable>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTable {
    pub name: String,
    pub columns: Vec<String>,
}

impl CatalogSnapshot {
    pub fn new() -> Self {
        Self {
            tables: BTreeMap::new(),
        }
    }

    pub fn add_table(
        &mut self,
        name: impl Into<String>,
        columns: impl IntoIterator<Item = String>,
    ) {
        let table_name = name.into();
        let mut cols = columns.into_iter().collect::<Vec<_>>();
        cols.sort_unstable();
        self.tables.insert(
            table_name.clone(),
            CatalogTable {
                name: table_name,
                columns: cols,
            },
        );
    }

    pub fn table_columns(&self, name: &str) -> Option<&[String]> {
        self.tables.get(name).map(|table| table.columns.as_slice())
    }

    pub fn tables(&self) -> impl Iterator<Item = &CatalogTable> {
        self.tables.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_snapshot_add_table_sorts_columns() {
        let mut catalog = CatalogSnapshot::new();

        catalog.add_table("model_a", vec!["z".into(), "a".into(), "m".into()]);

        let cols = catalog
            .table_columns("model_a")
            .expect("table should exist")
            .to_vec();
        assert_eq!(
            cols,
            vec!["a".to_string(), "m".to_string(), "z".to_string()]
        );
    }

    #[test]
    fn catalog_snapshot_add_table_overwrites_with_latest_definition() {
        let mut catalog = CatalogSnapshot::new();

        catalog.add_table("model_a", vec!["a".into()]);
        catalog.add_table("model_a", vec!["b".into(), "a".into()]);

        let cols = catalog
            .table_columns("model_a")
            .expect("table should exist")
            .to_vec();
        assert_eq!(cols, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn catalog_snapshot_tables_iterates_in_order() {
        let mut catalog = CatalogSnapshot::new();
        catalog.add_table("c.model", vec!["id".into()]);
        catalog.add_table("a.model", vec!["id".into()]);
        catalog.add_table("b.model", vec!["id".into()]);

        let names: Vec<String> = catalog.tables().map(|table| table.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "a.model".to_string(),
                "b.model".to_string(),
                "c.model".to_string()
            ]
        );
    }
}
