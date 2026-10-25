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
