//! Shared setup of the tests that evaluate published columns.

use super::super::Evaluator;
use crate::graph::RawGraph;
use crate::graph::scope::{ScopeId, ScopeTree};
use crate::resolve::context::EvalContext;
use crate::resolve::mapping::ScopeMappingCache;
use crate::resolve::model::{OutputElement, OutputModel, SlotId};
use crate::types::ColumnOrigin;
use crate::{Dialect, Ident};

pub(super) fn parse(sql: &str, dialect: Dialect) -> RawGraph {
    let statements = sqlparser::parser::Parser::new(&*dialect.to_sqlparser_dialect())
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements())
        .expect("test SQL parses");
    let (graph, _, _) = crate::build::LineageBuilder::new(dialect)
        .build(&statements[0])
        .unwrap_or_else(|_| panic!("test SQL builds"));
    graph
}

/// Run `body` with an evaluator over a fresh context for `graph`.
pub(super) fn evaluate<R>(
    graph: &RawGraph,
    body: impl FnOnce(&mut Evaluator<'_, '_, '_>) -> R,
) -> R {
    let mut incoming: Vec<Vec<usize>> = vec![vec![]; graph.nodes.len()];
    for (index, edge) in graph.edges.iter().enumerate() {
        incoming[edge.to].push(index);
    }
    let mut cx = EvalContext::new(graph, &incoming, None, ScopeMappingCache::default());
    body(&mut Evaluator { cx: &mut cx })
}

/// The scope of the CTE `name`, read from the root query.
pub(super) fn cte(graph: &RawGraph, name: &str) -> ScopeId {
    let mut scope = ScopeTree::root();
    while let crate::graph::scope::OutputPlan::Delegate(child) = graph.scopes.output_plan(scope) {
        scope = *child;
    }
    graph
        .scopes
        .lookup_cte(graph.scopes.env(scope), &Ident::unquoted(name))
        .visible
        .unwrap_or_else(|| panic!("expected the CTE {name}"))
}

/// The ids of the columns of a published model, in output order.
pub(super) fn slot_ids(model: &OutputModel) -> Vec<SlotId> {
    model
        .elements
        .iter()
        .filter_map(|element| match element {
            OutputElement::Column(slot) => Some(slot.published()),
            _ => None,
        })
        .collect()
}

/// A short rendering of origins: `t.a` for a concrete column, the debug form
/// of anything else.
pub(super) fn show(origins: &[ColumnOrigin]) -> Vec<String> {
    origins
        .iter()
        .map(|origin| match origin {
            ColumnOrigin::Concrete { table, column } => {
                format!("{}.{}", table.table.value, column.value)
            }
            other => format!("{other:?}"),
        })
        .collect()
}
