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

/// [`evaluate`] with a budget of `limit` steps.
pub(super) fn evaluate_limited<R>(
    graph: &RawGraph,
    limit: u64,
    body: impl FnOnce(&mut Evaluator<'_, '_, '_>) -> R,
) -> R {
    let mut incoming: Vec<Vec<usize>> = vec![vec![]; graph.nodes.len()];
    for (index, edge) in graph.edges.iter().enumerate() {
        incoming[edge.to].push(index);
    }
    let mut cx = EvalContext::new(graph, &incoming, None, ScopeMappingCache::default());
    cx.set_step_limit(limit);
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

/// A short rendering of origins: `t.a` for a concrete column, `?a` for the
/// unresolved marker of the column `a`, and the debug form of anything else.
pub(super) fn show(origins: &[ColumnOrigin]) -> Vec<String> {
    origins
        .iter()
        .map(|origin| match origin {
            ColumnOrigin::Concrete { table, column } => {
                format!("{}.{}", table.table.value, column.value)
            }
            ColumnOrigin::Ambiguous { column, candidates } if candidates.is_empty() => {
                format!("?{}", column.value)
            }
            ColumnOrigin::NamedWildcard { table, column } => {
                format!("{}.*:{}", table.table.value, column.value)
            }
            ColumnOrigin::Wildcard { table } => format!("{}.*", table.table.value),
            ColumnOrigin::UnknownStar => "*?".to_owned(),
            other => format!("{other:?}"),
        })
        .collect()
}

/// A derived table scope with one output column `name` that reads the column
/// `read` of the derived table `source` through a whole-row candidate, the
/// way a reference that the scope's model does not settle is read.
pub(super) fn derived_reading(
    graph: &mut RawGraph,
    name: &str,
    read: &str,
    source: Option<ScopeId>,
) -> (
    ScopeId,
    crate::graph::node::NodeId,
    crate::graph::node::NodeId,
) {
    use crate::graph::edge::EdgeKind;
    use crate::graph::scope::{Binding, ScopeColumn, ScopeKind};
    let scope = graph
        .scopes
        .push(ScopeTree::root(), ScopeKind::DerivedTable);
    let env = graph.scopes.env(scope);
    let output = graph.add_output(Ident::unquoted(name), EdgeKind::Direct);
    let candidate = graph.add_row_value_candidate(
        Ident::unquoted(read),
        env,
        source.map(Binding::DerivedTable),
    );
    graph.add_edge(candidate, output, EdgeKind::Direct);
    graph.scopes.add_output_column(
        scope,
        ScopeColumn {
            name: Ident::unquoted(name),
            node_id: output,
        },
    );
    (scope, output, candidate)
}

/// Point the whole-row candidate `candidate` at `source`, once that scope exists.
pub(super) fn bind_candidate(
    graph: &mut RawGraph,
    candidate: crate::graph::node::NodeId,
    source: ScopeId,
) {
    match &mut graph.nodes[candidate] {
        crate::graph::node::RawNode::RowValueCandidate { binding, .. } => {
            *binding = Some(crate::graph::scope::Binding::DerivedTable(source));
        }
        other => panic!("expected a whole-row candidate, got {other:?}"),
    }
}

/// Add an output `name` to the root that reads the column `read` of the
/// derived table `source`.
pub(super) fn root_reading(graph: &mut RawGraph, name: &str, read: &str, source: ScopeId) {
    use crate::graph::edge::EdgeKind;
    use crate::graph::scope::{Binding, ScopeColumn};
    let env = graph.scopes.env(ScopeTree::root());
    let output = graph.add_output(Ident::unquoted(name), EdgeKind::Direct);
    let reference = graph.add_ref_with_binding(
        Ident::unquoted(read),
        None,
        env,
        Some(Binding::DerivedTable(source)),
    );
    graph.add_edge(reference, output, EdgeKind::Direct);
    graph.scopes.add_output_column(
        ScopeTree::root(),
        ScopeColumn {
            name: Ident::unquoted(name),
            node_id: output,
        },
    );
}

/// Two derived tables that read each other: `a.x` is `b.y` and `b.y` is
/// `a.x`. The root outputs `p` (from `a.x`) and `q` (from `b.y`).
pub(super) struct Pair {
    pub graph: RawGraph,
    pub a: ScopeId,
    pub b: ScopeId,
    /// The candidates inside `a` and `b`.
    pub candidates: [crate::graph::node::NodeId; 2],
}

pub(super) fn pair() -> Pair {
    let mut graph = RawGraph::new(Dialect::Generic);
    let (a, _, a_reads) = derived_reading(&mut graph, "x", "y", None);
    let (b, _, b_reads) = derived_reading(&mut graph, "y", "x", Some(a));
    bind_candidate(&mut graph, a_reads, b);
    root_reading(&mut graph, "p", "x", a);
    root_reading(&mut graph, "q", "y", b);
    Pair {
        graph,
        a,
        b,
        candidates: [a_reads, b_reads],
    }
}
