pub(crate) mod analysis;
mod arity;
mod binding;
mod catalog;
mod lineage;
mod mapping;
mod model;
mod star;
mod target;
mod topo;

use arity::{graph_has_unresolved_stars, validate_set_arities};
use mapping::{ScopeMappingCache, resolve_scope_mappings};
use model::OutputModels;

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::scope::ScopeTree;
use crate::types::{ColumnOrigin, SemanticError, StatementType, Warning, WarningKind};

/// Resolve the graph into the public statement analysis.
#[allow(clippy::if_not_else, clippy::useless_let_if_seq)]
pub(crate) fn resolve(
    mut graph: RawGraph,
    catalog: Option<&CatalogContext<'_>>,
    mut warnings: Vec<Warning>,
    statement_type: &StatementType,
) -> Result<crate::StatementAnalysis, SemanticError> {
    crate::keys::sort_dedup_by_key(&mut graph.tables.inputs, |table| {
        crate::keys::table_key(table, graph.dialect)
    });

    if graph.nodes.is_empty() {
        return Ok(analysis::assemble(
            &[],
            &graph.tables,
            if graph.scopes.has_unknown_output_width(ScopeTree::root()) {
                &[]
            } else {
                &graph.target_columns
            },
            &warnings,
            statement_type,
            false,
            graph.dialect,
            catalog,
        ));
    }

    if topo::topological_sort(&graph.nodes, &graph.edges).is_err() {
        warnings.push(Warning {
            kind: WarningKind::UnexpectedCycle,
            location: None,
        });
        return Ok(analysis::assemble(
            &[],
            &graph.tables,
            if graph.scopes.has_unknown_output_width(ScopeTree::root()) {
                &[]
            } else {
                &graph.target_columns
            },
            &warnings,
            statement_type,
            false,
            graph.dialect,
            catalog,
        ));
    }

    let mut mapping_cache = ScopeMappingCache::default();
    let (output_width, has_unresolved_stars) = {
        let mut models = OutputModels::new(&graph, catalog, &mut mapping_cache.models);
        (
            validate_set_arities(&mut models)?,
            graph_has_unresolved_stars(&graph, &mut models),
        )
    };

    let mut incoming: Vec<Vec<usize>> = vec![vec![]; graph.nodes.len()];
    for (idx, edge) in graph.edges.iter().enumerate() {
        incoming[edge.to].push(idx);
    }

    let mut resolved: Vec<Option<ColumnOrigin>> = vec![None; graph.nodes.len()];

    let root = ScopeTree::root();
    let output_table = graph.tables.output.clone();
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

    Ok(analysis::assemble(
        &mappings,
        &graph.tables,
        if output_width.is_none() {
            &[]
        } else {
            &graph.target_columns
        },
        &warnings,
        statement_type,
        has_unresolved_stars,
        graph.dialect,
        catalog,
    ))
}

#[cfg(test)]
mod tests {
    use super::mapping::{reset_scope_mapping_stats, scope_mapping_computations};
    use crate::{Analyzer, Dialect, Lineage};

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
        let results = Analyzer::new(Dialect::Generic)
            .analyze(&sql)
            .expect("explicit projection should resolve");
        assert_eq!(results.statements.len(), 1);
        let columns = &results.statements[0].output.columns;
        assert_eq!(columns.len(), 10);
        assert!(columns.iter().all(|column| matches!(&column.lineage,
            Lineage::Complete(sources) if sources.iter().any(|source|
                source.relation.name == crate::Ident::unquoted("external_table")
                    && source.column == crate::Ident::unquoted("id")))));
        // One materialization per scope, independent of the ten requested
        // columns. The exact scope count is an implementation detail, but it
        // must remain bounded by the five wrappers plus the base and root.
        assert!(scope_mapping_computations() <= 8);
    }
}
