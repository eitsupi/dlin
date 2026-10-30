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

    // Rebuild the list so that every expansion takes the position of the
    // wildcard it replaces; appending would move all expanded columns behind
    // the columns that were written after the star.
    let mut result = Vec::with_capacity(mappings.len());
    for mapping in mappings.drain(..) {
        if let [ColumnOrigin::Wildcard { table }] = mapping.sources.as_slice()
            && let Some(columns) = catalog.list_columns(table)
        {
            for col_name in columns {
                result.push(ColumnMapping {
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
        } else {
            result.push(mapping);
        }
    }
    *mappings = result;
}

#[cfg(test)]
mod tests {
    use super::apply_catalog;
    use crate::types::{
        CatalogProvider, ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind,
    };

    struct Columns;

    impl CatalogProvider for Columns {
        fn list_columns(&self, table: &TableRef) -> Option<Vec<crate::Ident>> {
            match table.table.value.as_str() {
                "t" => Some(vec![
                    crate::Ident::unquoted("x"),
                    crate::Ident::unquoted("y"),
                ]),
                "u" => Some(vec![crate::Ident::unquoted("p")]),
                _ => None,
            }
        }

        fn resolve_column(
            &self,
            _column: &crate::Ident,
            _candidates: &[TableRef],
        ) -> Option<TableRef> {
            None
        }
    }

    fn named(name: &str) -> ColumnMapping {
        ColumnMapping {
            target: ColumnRef {
                table: None,
                column: crate::Ident::unquoted(name),
            },
            sources: vec![ColumnOrigin::Concrete {
                table: TableRef::new("src"),
                column: crate::Ident::unquoted(name),
            }],
            transform: TransformKind::Direct,
        }
    }

    fn star(table: &str) -> ColumnMapping {
        ColumnMapping {
            target: ColumnRef {
                table: None,
                column: crate::Ident::unquoted("*"),
            },
            sources: vec![ColumnOrigin::Wildcard {
                table: TableRef::new(table),
            }],
            transform: TransformKind::Direct,
        }
    }

    fn targets(mappings: &[ColumnMapping]) -> Vec<&str> {
        mappings
            .iter()
            .map(|mapping| mapping.target.column.value.as_str())
            .collect()
    }

    #[test]
    fn expanded_star_replaces_the_wildcard_in_place() {
        let mut mappings = vec![named("a"), star("t"), named("b")];
        apply_catalog(&mut mappings, &Columns);
        assert_eq!(targets(&mappings), ["a", "x", "y", "b"]);
    }

    #[test]
    fn several_stars_are_each_expanded_in_place() {
        let mut mappings = vec![star("t"), named("a"), star("u"), named("b"), star("t")];
        apply_catalog(&mut mappings, &Columns);
        assert_eq!(targets(&mappings), ["x", "y", "a", "p", "b", "x", "y"]);
    }

    #[test]
    fn star_without_catalog_columns_is_kept_where_it_was() {
        let mut mappings = vec![named("a"), star("unknown"), star("t"), named("b")];
        apply_catalog(&mut mappings, &Columns);
        assert_eq!(targets(&mappings), ["a", "*", "x", "y", "b"]);
        assert!(matches!(
            mappings[1].sources.as_slice(),
            [ColumnOrigin::Wildcard { table }] if table.table == crate::Ident::unquoted("unknown")
        ));
    }
}
