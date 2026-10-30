mod arity;
mod binding;
mod catalog;
mod lineage;
mod mapping;
mod star;
mod topo;

use arity::{graph_has_unresolved_stars, validate_set_arities};
use mapping::{ScopeMappingCache, resolve_scope_mappings};

use crate::graph::RawGraph;
use crate::graph::scope::ScopeTree;
use crate::types::{
    AnalyzeResult, CatalogProvider, ColumnLineage, ColumnOrigin, SemanticError, StatementType,
    Warning, WarningKind,
};

/// Resolve `RawGraph` into `AnalyzeResult`.
#[allow(clippy::if_not_else, clippy::useless_let_if_seq)]
pub(crate) fn resolve(
    mut graph: RawGraph,
    catalog: Option<&dyn CatalogProvider>,
    mut warnings: Vec<Warning>,
    statement_type: StatementType,
) -> Result<AnalyzeResult, SemanticError> {
    crate::keys::sort_dedup_by_key(&mut graph.tables.inputs, |table| {
        crate::keys::table_key(table, graph.dialect)
    });

    if graph.nodes.is_empty() {
        return Ok(AnalyzeResult {
            statement_type,
            tables: graph.tables,
            source_order: Vec::new(),
            columns: ColumnLineage {
                mappings: Vec::new(),
                has_unresolved_stars: false,
            },
            warnings,
        });
    }

    if topo::topological_sort(&graph.nodes, &graph.edges).is_err() {
        warnings.push(Warning {
            kind: WarningKind::UnexpectedCycle,
            location: None,
        });
        return Ok(AnalyzeResult {
            statement_type,
            tables: graph.tables,
            source_order: Vec::new(),
            columns: ColumnLineage {
                mappings: Vec::new(),
                has_unresolved_stars: false,
            },
            warnings,
        });
    }

    validate_set_arities(&graph, catalog)?;
    let has_unresolved_stars = graph_has_unresolved_stars(&graph, catalog);

    let mut incoming: Vec<Vec<usize>> = vec![vec![]; graph.nodes.len()];
    for (idx, edge) in graph.edges.iter().enumerate() {
        incoming[edge.to].push(idx);
    }

    let mut resolved: Vec<Option<ColumnOrigin>> = vec![None; graph.nodes.len()];

    let root = ScopeTree::root();
    let output_table = graph.tables.output.clone();
    let mut mapping_cache = ScopeMappingCache::default();
    // Scope mappings are cached in canonical form without an output table.
    // Internal CTE/derived references need that form, while the root output
    // table is only presentation metadata and is attached once here. Keeping
    // it out of the cache key avoids materializing the same scope once per
    // output column and cannot alter source resolution.
    let mut mappings = resolve_scope_mappings(
        root,
        &graph,
        &mut resolved,
        &incoming,
        catalog,
        &mut mapping_cache,
    )
    .iter()
    .cloned()
    .collect::<Vec<_>>();
    for mapping in &mut mappings {
        mapping.target.table.clone_from(&output_table);
    }

    let mut source_order: Vec<_> = resolved
        .into_iter()
        .enumerate()
        .filter_map(|(node, origin)| {
            origin.map(|origin| {
                (
                    graph.reference_positions[node].unwrap_or((u64::MAX, u64::MAX)),
                    node,
                    origin,
                )
            })
        })
        .collect();
    source_order.sort_by_key(|(position, node, _)| (*position, *node));
    Ok(AnalyzeResult {
        statement_type,
        tables: graph.tables,
        source_order: source_order
            .into_iter()
            .map(|(_, _, origin)| origin)
            .collect(),
        columns: ColumnLineage {
            mappings,
            has_unresolved_stars,
        },
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::mapping::{reset_scope_mapping_stats, scope_mapping_computations};
    use crate::legacy_analyze as analyze;
    use crate::types::{AnalyzeOptions, ColumnOrigin, Dialect};

    #[test]
    fn explicit_projection_reuses_scope_mappings() {
        let columns = (0..10).map(|index| format!("c{index}")).collect::<Vec<_>>();
        let names = columns.join(", ");
        let base_projection = (0..10)
            .map(|index| format!("id + 1 AS c{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "WITH base AS (SELECT {base_projection} FROM external_table), \
             cte0 AS (SELECT {names} FROM base), \
             cte1 AS (SELECT {names} FROM cte0), \
             cte2 AS (SELECT {names} FROM cte1), \
             cte3 AS (SELECT {names} FROM cte2), \
             cte4 AS (SELECT {names} FROM cte3) \
             SELECT {names} FROM cte4"
        );

        reset_scope_mapping_stats();
        let results = analyze(
            &sql,
            AnalyzeOptions {
                dialect: Dialect::Generic,
                ..Default::default()
            },
        )
        .expect("explicit projection should resolve");

        assert_eq!(results.len(), 1);
        let mappings = &results[0].columns.mappings;
        assert_eq!(mappings.len(), 10);
        assert!(mappings.iter().all(|mapping| {
            mapping.sources.iter().any(|source| {
                matches!(
                    source,
                    ColumnOrigin::Concrete { table, column }
                        if table.table == crate::Ident::unquoted("external_table")
                            && column == &crate::Ident::unquoted("id")
                )
            })
        }));
        // One materialization per scope, independent of the ten requested
        // columns. The exact scope count is an implementation detail, but it
        // must remain bounded by the five wrappers plus the base and root.
        assert!(scope_mapping_computations() <= 8);
    }
}
