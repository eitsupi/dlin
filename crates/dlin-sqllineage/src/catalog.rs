//! Borrowing catalog interface for relation metadata.

use super::{Ident, Relation};
use serde::Serialize;
use std::borrow::Cow;

/// Catalog that can look up relation metadata without requiring a clone.
pub trait Catalog: Send + Sync {
    /// Find a relation and return its canonical spelling and columns.
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>>;
}

/// Whether the catalog lists every column of the relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SchemaCoverage {
    /// Unlisted names are absent.
    Exhaustive,
    /// Listed names exist; unlisted names may also exist.
    Partial,
}

/// Whether the complete column order is proven by the schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum SchemaOrder {
    /// Column ordinals describe the complete order.
    Proven,
    /// Display order does not establish physical positions.
    Unknown,
}

/// Opaque reference to schema evidence owned by the catalog provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EvidenceRef(pub String);

/// An unenumerated interval in a partial relation schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogUnknownSegment {
    /// Position immediately preceding the interval, when established.
    pub after_ordinal: Option<usize>,
}

/// What the catalog proves about the existence of a column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ColumnPresence {
    Present(Ident),
    Absent,
    Possible,
}

/// Canonical relation and its catalog columns.
///
/// Column storage order is display order, not evidence of positional order.
/// A provider with `Proven` order supplies an ordinal for each column. A
/// partial schema may prove individual positions without proving its full
/// order. Empty exhaustive schemas are distinct from an absent entry.
/// Partial schemas have an unknown remainder even when `unknown_segments`
/// is empty; explicit segments preserve the anchors of SQL-inferred output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogEntry<'a> {
    /// Canonical relation spelling to use in lineage results.
    pub relation: Relation,
    /// Relation columns, borrowed where possible.
    pub columns: Cow<'a, [CatalogColumn]>,
    /// Whether unlisted columns are proven absent.
    pub coverage: SchemaCoverage,
    /// Whether the full physical column order is established.
    pub order: SchemaOrder,
    /// Unenumerated intervals. Partial schemas without explicit intervals
    /// still have an unknown remainder.
    pub unknown_segments: Cow<'a, [CatalogUnknownSegment]>,
    /// Provider-owned evidence supporting this schema.
    pub evidence: Cow<'a, [EvidenceRef]>,
}

/// Column metadata available from a catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogColumn {
    /// Column name, including its quote metadata.
    pub name: Ident,
    /// Proven zero-based position, independent of display order.
    pub ordinal: Option<usize>,
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

    pub(crate) fn resolve_column(
        &self,
        column: &Ident,
        candidates: &[crate::types::TableRef],
    ) -> Option<crate::types::TableRef> {
        let mut matching = candidates
            .iter()
            .filter(|table| !matches!(self.column_presence(table, column), ColumnPresence::Absent));
        let only = matching.next()?;
        if matching.next().is_none()
            && matches!(
                self.column_presence(only, column),
                ColumnPresence::Present(_)
            )
        {
            Some(only.clone())
        } else {
            None
        }
    }

    pub(crate) fn column_presence(
        &self,
        table: &crate::types::TableRef,
        name: &Ident,
    ) -> ColumnPresence {
        let Some(entry) = self.entry(table) else {
            return ColumnPresence::Possible;
        };
        if let Some(column) = entry
            .columns
            .iter()
            .find(|column| column.name.column_key(self.dialect) == name.column_key(self.dialect))
        {
            return ColumnPresence::Present(column.name.clone());
        }
        match entry.coverage {
            SchemaCoverage::Exhaustive => ColumnPresence::Absent,
            SchemaCoverage::Partial => ColumnPresence::Possible,
        }
    }

    pub(crate) fn column(&self, table: &crate::types::TableRef, name: &Ident) -> Option<Ident> {
        match self.column_presence(table, name) {
            ColumnPresence::Present(canonical) => Some(canonical),
            ColumnPresence::Absent | ColumnPresence::Possible => None,
        }
    }
}
