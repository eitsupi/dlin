use crate::catalog::CatalogContext;
use crate::types::{ColumnMapping, ColumnOrigin, Evidence};

/// Apply `Catalog` to refine unresolved column origins. The mappings keep their
/// number and order.
///
/// A star over a relation the catalog lists is expanded when the model of the
/// scope is built, so no wildcard of such a relation reaches this function. That
/// relies on the catalog answering consistently within one analysis: a catalog
/// whose answer for a relation changed between lookups would leave a wildcard
/// mapping that is not expanded here.
///
/// An origin the catalog was asked about relies on the schemas of every
/// candidate, since they decide whether one of them owns the column, and a canonical spelling on the schema it came from.
pub(crate) fn apply_catalog(mappings: &mut [ColumnMapping], catalog: &CatalogContext<'_>) {
    for mapping in mappings.iter_mut() {
        for source in &mut mapping.sources.origins {
            if let ColumnOrigin::Ambiguous { column, candidates } = &source.kind
                && !candidates.is_empty()
            {
                for candidate in candidates {
                    source.evidence.add(candidate);
                }
                if let Some(owner) = catalog.resolve_column(column, candidates) {
                    source.kind = ColumnOrigin::Concrete {
                        table: owner,
                        column: column.clone(),
                    };
                }
            }
            if let ColumnOrigin::Concrete { table, column } = &mut source.kind
                && let Some(canonical) = catalog.column(table, column)
            {
                *column = canonical;
                source.evidence.extend(&Evidence::relation(table));
            }
        }
    }
}
