pub(crate) mod analysis;
mod binding;
mod catalog;
mod context;
mod lineage;
mod mapping;
mod model;
mod set_operation;
mod star;
mod target;
mod topo;
mod width;

use context::EvalContext;
use mapping::{ScopeMappingCache, resolve_scope_mappings};
use model::OutputModels;
pub(crate) use model::OutputRef;
use width::{graph_has_unresolved_stars, validate_set_arities};

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::scope::ScopeTree;
use crate::types::{SemanticError, StatementType, Warning, WarningKind};

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

    let mut cx = EvalContext::new(&graph, &incoming, catalog, mapping_cache);

    let root = ScopeTree::root();
    let output_table = graph.tables.output.clone();
    // Scope mappings are cached in canonical form without an output table.
    // Internal CTE/derived references need that form, while the root output
    // table is only presentation metadata and is attached once here. Keeping
    // it out of the cache key avoids materializing the same scope once per
    // output column and cannot alter source resolution.
    let Ok(mappings) = resolve_scope_mappings(root, &mut cx) else {
        // Nothing is being evaluated yet, so the root cannot be re-entered.
        warnings.push(Warning {
            kind: WarningKind::UnexpectedCycle,
            location: None,
        });
        return Ok(analysis::assemble(
            &[],
            &graph.tables,
            if graph.scopes.has_unknown_output_width(root) {
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
    };
    let mut mappings = mappings.iter().cloned().collect::<Vec<_>>();
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
    use super::resolve;
    use crate::graph::RawGraph;
    use crate::graph::edge::EdgeKind;
    use crate::graph::scope::{Binding, ScopeColumn, ScopeKind, ScopeTree};
    use crate::types::StatementType;
    use crate::{Analyzer, Dialect, Ident, Lineage, Unresolved};

    /// A derived table whose only column reads that same column of itself
    /// cannot be produced by SQL once names are looked up from where they are
    /// written, but a column that depends on itself must still come out as an
    /// unresolved value, not as nothing and not as unbounded recursion.
    #[test]
    fn a_column_that_depends_on_itself_is_unresolved() {
        // A regression would overflow the stack and abort the process, so the
        // body runs in a child process that fails this test instead.
        const CHILD: &str = "DLIN_SQLLINEAGE_SELF_DEPENDENT_COLUMN";
        let name = "resolve::tests::a_column_that_depends_on_itself_is_unresolved";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", name, "--test-threads=1"])
                .env(CHILD, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "the child run failed ({}):\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let x = Ident::unquoted("x");
        let mut graph = RawGraph::new(Dialect::Generic);
        let derived = graph
            .scopes
            .push(ScopeTree::root(), ScopeKind::DerivedTable);
        let derived_env = graph.scopes.env(derived);
        let inner = graph.add_output(x.clone(), EdgeKind::Direct);
        let inner_read = graph.add_ref_with_binding(
            x.clone(),
            None,
            derived_env,
            Some(Binding::DerivedTable(derived)),
        );
        graph.add_edge(inner_read, inner, EdgeKind::Direct);
        graph.scopes.add_output_column(
            derived,
            ScopeColumn {
                name: x.clone(),
                node_id: inner,
            },
        );

        let root_env = graph.scopes.env(ScopeTree::root());
        let outer = graph.add_output(x.clone(), EdgeKind::Direct);
        let outer_read = graph.add_ref_with_binding(
            x.clone(),
            None,
            root_env,
            Some(Binding::DerivedTable(derived)),
        );
        graph.add_edge(outer_read, outer, EdgeKind::Direct);
        graph.scopes.add_output_column(
            ScopeTree::root(),
            ScopeColumn {
                name: x.clone(),
                node_id: outer,
            },
        );

        let analysis = resolve(graph, None, Vec::new(), &StatementType::Query)
            .expect("a cycle is an unresolved value, not an error");
        let lineage = &analysis.output.columns[0].lineage;
        assert!(
            matches!(lineage, Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && matches!(&unresolved[..], [Unresolved::Unbound { column }
                        | Unresolved::Ambiguous { column, .. }] if *column == x)),
            "{lineage:?}"
        );
    }

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
