//! The state shared by every path that evaluates lineage.
//!
//! Evaluation walks from an output through references, scopes and stars, and
//! the same scope or node can be reached from many entry points. Two kinds of
//! state therefore have to be kept apart:
//!
//! - what is *currently being evaluated* (the stack), which is how a cycle is
//!   recognized: reaching a key that is already on the stack means the answer
//!   depends on itself;
//! - what has *already been computed* (the caches), which is how work is
//!   reused: a finished answer never depends on the path that produced it.
//!
//! One [`EvalContext`] is created per statement and passed to every evaluation
//! path, so re-entry is detected no matter which path started the walk. A
//! detected cycle is not an answer: the caller reports the value as
//! indeterminate.

use std::collections::HashSet;

use super::mapping::ScopeMappingCache;

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::node::NodeId;
use crate::graph::scope::ScopeId;
use crate::types::ColumnOrigin;

/// Something whose evaluation can be in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EvalKey {
    /// The sources collected for a node.
    Node(NodeId),
    /// The value of an output slot of a scope.
    Slot { scope: ScopeId, index: usize },
    /// The mappings of a scope.
    Mappings(ScopeId),
}

/// An upper bound on the work of one statement. It is unlimited unless a
/// limit is set, and running over it is treated like reaching a cycle: the
/// evaluation stops and reports an indeterminate value.
#[derive(Debug, Default)]
struct Budget {
    steps: u64,
    limit: Option<u64>,
}

impl Budget {
    fn charge(&mut self) -> bool {
        self.steps = self.steps.saturating_add(1);
        self.limit.is_none_or(|limit| self.steps <= limit)
    }
}

pub(super) struct EvalContext<'a, 'c> {
    pub graph: &'a RawGraph,
    /// Edges into each node.
    pub incoming: &'a [Vec<usize>],
    pub catalog: Option<&'a CatalogContext<'c>>,
    /// Already computed: the origin of each node.
    pub resolved: Vec<Option<ColumnOrigin>>,
    /// Already computed: the mappings and models of each scope.
    pub mapping_cache: ScopeMappingCache,
    /// Currently being evaluated.
    stack: HashSet<EvalKey>,
    budget: Budget,
}

impl<'a, 'c> EvalContext<'a, 'c> {
    pub(super) fn new(
        graph: &'a RawGraph,
        incoming: &'a [Vec<usize>],
        catalog: Option<&'a CatalogContext<'c>>,
        mapping_cache: ScopeMappingCache,
    ) -> Self {
        Self {
            graph,
            incoming,
            catalog,
            resolved: vec![None; graph.nodes.len()],
            mapping_cache,
            stack: HashSet::new(),
            budget: Budget::default(),
        }
    }

    /// Start evaluating `key`. Returns `false` when it is already being
    /// evaluated, or when the budget is spent; the caller then reports an
    /// indeterminate value instead of evaluating it. After a `true`, the
    /// caller must call [`Self::leave`] with the same key.
    #[must_use]
    pub(super) fn enter(&mut self, key: EvalKey) -> bool {
        if !self.budget.charge() {
            return false;
        }
        self.stack.insert(key)
    }

    /// Finish evaluating `key`, which the caller entered.
    pub(super) fn leave(&mut self, key: EvalKey) {
        self.stack.remove(&key);
    }

    #[cfg(test)]
    pub(super) fn set_step_limit(&mut self, limit: u64) {
        self.budget.limit = Some(limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Dialect;

    fn context(graph: &RawGraph) -> EvalContext<'_, '_> {
        EvalContext::new(graph, &[], None, ScopeMappingCache::default())
    }

    #[test]
    fn re_entering_a_key_on_the_stack_is_refused_and_finished_keys_can_be_entered_again() {
        let graph = RawGraph::new(Dialect::Generic);
        let mut cx = context(&graph);
        let key = EvalKey::Slot { scope: 0, index: 1 };
        assert!(cx.enter(key));
        assert!(!cx.enter(key));
        // A different key is independent of the one being evaluated.
        assert!(cx.enter(EvalKey::Mappings(0)));
        cx.leave(EvalKey::Mappings(0));
        cx.leave(key);
        assert!(cx.enter(key));
    }

    #[test]
    fn a_spent_budget_refuses_new_evaluations() {
        let graph = RawGraph::new(Dialect::Generic);
        let mut cx = context(&graph);
        cx.set_step_limit(2);
        assert!(cx.enter(EvalKey::Node(0)));
        assert!(cx.enter(EvalKey::Node(1)));
        assert!(!cx.enter(EvalKey::Node(2)));
    }
}
