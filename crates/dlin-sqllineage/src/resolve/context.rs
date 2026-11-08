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
//!
//! A value computed while a cycle was being cut depends on where the walk
//! started: entering the cycle elsewhere cuts it elsewhere. Such a value must
//! not be kept in a cache, or the answer would depend on the order in which
//! outputs happen to be asked for. Every refused entry is counted, and an
//! evaluation that saw the count change while it ran has seen a cut and keeps
//! nothing. That covers the evaluation that was re-entered and every
//! evaluation above it, because all of them were running when the count
//! changed.

use std::collections::{HashMap, HashSet};

use super::mapping::ScopeMappingCache;
use super::model::{SegmentId, SlotId};

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::node::NodeId;
use crate::graph::scope::ScopeId;
use crate::types::{ColumnMapping, ColumnOrigin, TransformKind};

/// Something whose evaluation can be in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EvalKey {
    /// The sources collected for a node.
    Node(NodeId),
    /// The value of a published column.
    Slot(SlotId),
    /// What a published segment contributes.
    Segment(SegmentId),
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

/// What a published column evaluates to, as its own scope states it.
#[derive(Debug, Clone)]
pub(super) struct SlotValue {
    pub sources: Vec<ColumnOrigin>,
    pub transform: TransformKind,
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
    /// Already computed: the value of each published column.
    pub slot_values: HashMap<SlotId, SlotValue>,
    /// Already computed: the mapping a published segment stands for, `None`
    /// when it stands for no columns. Only the segment as a whole is cached;
    /// what it contributes under one name depends on the name.
    pub segment_mappings: HashMap<SegmentId, Option<ColumnMapping>>,
    /// Currently being evaluated.
    stack: HashSet<EvalKey>,
    /// How many entries were refused so far.
    cuts: u64,
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
            slot_values: HashMap::new(),
            segment_mappings: HashMap::new(),
            stack: HashSet::new(),
            cuts: 0,
            budget: Budget::default(),
        }
    }

    /// Start evaluating `key`. Returns `false` when it is already being
    /// evaluated, or when the budget is spent; the caller then reports an
    /// indeterminate value instead of evaluating it. After a `true`, the
    /// caller must call [`Self::leave`] with the same key.
    #[must_use]
    pub(super) fn enter(&mut self, key: EvalKey) -> bool {
        if self.budget.charge() && self.stack.insert(key) {
            return true;
        }
        self.cuts += 1;
        false
    }

    /// A reading of the cut count. An evaluation that started at one reading
    /// and finished at another has seen a cut, and what it computed may only
    /// be returned, not kept.
    pub(super) fn cut_generation(&self) -> u64 {
        self.cuts
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
        let key = EvalKey::Node(1);
        let generation = cx.cut_generation();
        assert!(cx.enter(key));
        assert_eq!(cx.cut_generation(), generation);
        assert!(!cx.enter(key));
        assert_ne!(cx.cut_generation(), generation);
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
        let generation = cx.cut_generation();
        assert!(!cx.enter(EvalKey::Node(2)));
        // Running out of budget is a cut like re-entry.
        assert_ne!(cx.cut_generation(), generation);
    }
}
