use crate::catalog::CatalogContext;
use crate::types::{ColumnMapping, ColumnOrigin};

/// Apply `Catalog` to refine unresolved column origins. The mappings keep their
/// number and order.
///
/// A star over a relation the catalog lists is expanded when the model of the
/// scope is built, so no wildcard of such a relation reaches this function. That
/// relies on the catalog answering consistently within one analysis: a catalog
/// whose answer for a relation changed between lookups would leave a wildcard
/// mapping that is not expanded here.
pub(crate) fn apply_catalog(mappings: &mut [ColumnMapping], catalog: &CatalogContext<'_>) {
    for mapping in mappings.iter_mut() {
        for source in &mut mapping.sources {
            if let ColumnOrigin::Ambiguous { column, candidates } = source
                && !candidates.is_empty()
                && let Some(owner) = catalog.resolve_column(column, candidates)
            {
                *source = ColumnOrigin::Concrete {
                    table: owner,
                    column: column.clone(),
                };
            }
            if let ColumnOrigin::Concrete { table, column } = source
                && let Some(canonical) = catalog.column(table, column)
            {
                *column = canonical;
            }
        }
    }
}
