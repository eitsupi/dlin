//! Borrowing catalog interface for relation metadata.

use super::{Ident, Relation};
use serde::Serialize;
use std::borrow::Cow;

/// Catalog that can look up relation metadata without requiring a clone.
pub trait Catalog: Send + Sync {
    /// Find a relation and return its canonical spelling and columns.
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>>;
}

/// Canonical relation and its catalog columns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogEntry<'a> {
    /// Canonical relation spelling to use in lineage results.
    pub relation: Relation,
    /// Relation columns, borrowed where possible.
    pub columns: Cow<'a, [CatalogColumn]>,
}

/// Column metadata available from a catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogColumn {
    /// Column name, including its quote metadata.
    pub name: Ident,
    /// Optional SQL type name.
    pub data_type: Option<String>,
    /// Nested fields for structured column types, if known.
    pub fields: Option<Vec<CatalogColumn>>,
}

/// Catalog access shared by the resolver and the transitional result conversion.
#[derive(Clone, Copy)]
pub(crate) struct CatalogContext<'a> {
    catalog: &'a dyn Catalog,
    pub(crate) dialect: crate::Dialect,
}

impl<'a> CatalogContext<'a> {
    pub(crate) fn new(catalog: &'a dyn Catalog, dialect: crate::Dialect) -> Self {
        Self { catalog, dialect }
    }

    pub(crate) fn entry(&self, table: &crate::types::TableRef) -> Option<CatalogEntry<'_>> {
        self.catalog.lookup(&Relation {
            catalog: table.catalog.clone(),
            schema: table.schema.clone(),
            name: table.table.clone(),
        })
    }

    pub(crate) fn list_columns(&self, table: &crate::types::TableRef) -> Option<Vec<Ident>> {
        Some(
            self.entry(table)?
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        )
    }

    pub(crate) fn resolve_column(
        &self,
        column: &Ident,
        candidates: &[crate::types::TableRef],
    ) -> Option<crate::types::TableRef> {
        let mut matching = candidates
            .iter()
            .filter(|table| self.column(table, column).is_some());
        let only = matching.next()?;
        matching.next().is_none().then(|| only.clone())
    }

    pub(crate) fn column(&self, table: &crate::types::TableRef, name: &Ident) -> Option<Ident> {
        self.entry(table)?
            .columns
            .iter()
            .find(|column| column.name.column_key(self.dialect) == name.column_key(self.dialect))
            .map(|column| column.name.clone())
    }
}
