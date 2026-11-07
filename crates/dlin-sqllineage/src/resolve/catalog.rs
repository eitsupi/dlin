use crate::catalog::CatalogContext;
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TransformKind};

/// Apply `Catalog` to refine unresolved column origins.
pub(crate) fn apply_catalog(mappings: &mut Vec<ColumnMapping>, catalog: &CatalogContext<'_>) {
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

    // Rebuild the list so that every expansion takes the position of the
    // wildcard it replaces; appending would move all expanded columns behind
    // the columns that were written after the star.
    let mut result = Vec::with_capacity(mappings.len());
    for mapping in mappings.drain(..) {
        if let [ColumnOrigin::Wildcard { table }] = mapping.sources.as_slice()
            && let Some(entry) = catalog.entry(table)
            && entry.coverage == crate::SchemaCoverage::Exhaustive
            && entry.order == crate::SchemaOrder::Proven
        {
            for col_name in entry.columns.iter().map(|column| column.name.clone()) {
                result.push(ColumnMapping {
                    star_position: crate::types::StarPosition::Inferred,
                    ordinal: None,
                    output_name: Some(crate::OutputName::Explicit(col_name.clone())),
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
    use crate::catalog::CatalogContext;
    use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind};

    struct Columns;

    impl crate::Catalog for Columns {
        fn lookup(&self, relation: &crate::Relation) -> Option<crate::CatalogEntry<'_>> {
            let names = match relation.name.value.as_str() {
                "t" => vec!["x", "y"],
                "u" => vec!["p"],
                _ => return None,
            };
            Some(crate::CatalogEntry {
                coverage: crate::SchemaCoverage::Exhaustive,
                order: crate::SchemaOrder::Proven,
                unknown_segments: std::borrow::Cow::Borrowed(&[]),
                evidence: std::borrow::Cow::Borrowed(&[]),
                relation: relation.clone(),
                columns: names
                    .into_iter()
                    .enumerate()
                    .map(|(ordinal, name)| crate::CatalogColumn {
                        ordinal: Some(ordinal),
                        name: crate::Ident::unquoted(name),
                        data_type: None,
                        fields: None,
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
        }
    }

    fn named(name: &str) -> ColumnMapping {
        ColumnMapping {
            star_position: crate::types::StarPosition::Inferred,
            ordinal: None,
            output_name: Some(crate::OutputName::Explicit(crate::Ident::unquoted(name))),
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
            star_position: crate::types::StarPosition::Inferred,
            ordinal: None,
            output_name: None,
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
        apply_catalog(
            &mut mappings,
            &CatalogContext::new(&Columns, crate::Dialect::Generic),
        );
        assert_eq!(targets(&mappings), ["a", "x", "y", "b"]);
    }

    #[test]
    fn several_stars_are_each_expanded_in_place() {
        let mut mappings = vec![star("t"), named("a"), star("u"), named("b"), star("t")];
        apply_catalog(
            &mut mappings,
            &CatalogContext::new(&Columns, crate::Dialect::Generic),
        );
        assert_eq!(targets(&mappings), ["x", "y", "a", "p", "b", "x", "y"]);
    }

    #[test]
    fn star_without_catalog_columns_is_kept_where_it_was() {
        let mut mappings = vec![named("a"), star("unknown"), star("t"), named("b")];
        apply_catalog(
            &mut mappings,
            &CatalogContext::new(&Columns, crate::Dialect::Generic),
        );
        assert_eq!(targets(&mappings), ["a", "*", "x", "y", "b"]);
        assert!(matches!(
            mappings[1].sources.as_slice(),
            [ColumnOrigin::Wildcard { table }] if table.table == crate::Ident::unquoted("unknown")
        ));
    }
}
