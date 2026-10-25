use crate::types::{CatalogProvider, ColumnMapping, ColumnOrigin, ColumnRef, TransformKind};

/// Apply `CatalogProvider` to refine unresolved column origins.
pub(crate) fn apply_catalog(mappings: &mut Vec<ColumnMapping>, catalog: &dyn CatalogProvider) {
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
        }
    }

    let mut expanded = Vec::new();
    let mut to_remove = Vec::new();

    for (idx, mapping) in mappings.iter().enumerate() {
        if let [ColumnOrigin::Wildcard { table }] = mapping.sources.as_slice()
            && let Some(columns) = catalog.list_columns(table)
        {
            to_remove.push(idx);
            for col_name in columns {
                expanded.push(ColumnMapping {
                    target: ColumnRef {
                        table: mapping.target.table.clone(),
                        column: col_name.clone(),
                    },
                    sources: vec![ColumnOrigin::Concrete {
                        table: table.clone(),
                        column: col_name,
                    }],
                    transform: TransformKind::Direct,
                });
            }
        }
    }

    for &idx in to_remove.iter().rev() {
        mappings.remove(idx);
    }
    mappings.extend(expanded);
}
