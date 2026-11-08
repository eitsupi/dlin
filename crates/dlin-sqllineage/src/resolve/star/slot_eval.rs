//! Evaluation of the columns and segments a scope has published, by id, and
//! of the set-operation merge those ids are looked up in.
//!
//! A reader that resolved a name to a published column holds its id, so what
//! it evaluates is that column and never another one that shares its name or
//! its position.

#[cfg(test)]
use std::cell::RefCell;

use super::{Evaluator, Unlisted};
use crate::Ident;
use crate::graph::scope::{OutputPlan, ScopeId};
use crate::types::{ColumnMapping, ColumnOrigin, TransformKind};

use super::super::context::EvalKey;
use super::super::lineage::flatten_mapping_sources;
use super::super::model::{BranchTail, OutputElement, OutputModel, SegmentId, SlotId};
use super::super::target::same_column_name;

#[cfg(test)]
thread_local! {
    static EVALUATED_SLOTS: RefCell<Vec<SlotId>> = const { RefCell::new(Vec::new()) };
}

/// The columns whose value was computed, in the order they were entered, since
/// the last call.
#[cfg(test)]
fn take_evaluated_slots() -> Vec<SlotId> {
    EVALUATED_SLOTS.with(|slots| std::mem::take(&mut *slots.borrow_mut()))
}

impl Evaluator<'_, '_, '_> {
    /// The origins and transform of a published column. A column whose
    /// evaluation is already in progress depends on itself and is reported as
    /// an ambiguous column.
    pub(super) fn slot_value(&mut self, id: SlotId) -> (Vec<ColumnOrigin>, TransformKind) {
        let def = self.cx.mapping_cache.models.arena.slot(id);
        if !self.cx.enter(EvalKey::Slot(id)) {
            return (
                vec![ColumnOrigin::Ambiguous {
                    column: def.name.clone(),
                    candidates: Vec::new(),
                }],
                TransformKind::Direct,
            );
        }
        #[cfg(test)]
        EVALUATED_SLOTS.with(|slots| slots.borrow_mut().push(id));
        let result = if self.is_set_operation(def.owner) {
            self.merged_slot(def.owner, def.index, &def.name)
        } else {
            self.value(&def.value)
        };
        self.cx.leave(EvalKey::Slot(id));
        result
    }

    /// The same as [`Self::slot_value`] for a reader that names the column:
    /// the recursive marker of a set operation's column is unwrapped, and
    /// whether it was there is reported.
    pub(super) fn slot_read(&mut self, id: SlotId) -> (Vec<ColumnOrigin>, bool, TransformKind) {
        let owner = self.cx.mapping_cache.models.arena.slot(id).owner;
        let (sources, transform) = self.slot_value(id);
        if self.is_set_operation(owner) {
            let (sources, has_back) = flatten_mapping_sources(&sources);
            (sources, has_back, transform)
        } else {
            (sources, false, transform)
        }
    }

    /// The column of a set operation that stands for `left`, a column of its
    /// left operand.
    pub(super) fn merged_slot_value(
        &mut self,
        op: ScopeId,
        left: SlotId,
    ) -> (Vec<ColumnOrigin>, TransformKind) {
        let owner = self.cx.mapping_cache.models.arena.slot(left).owner;
        debug_assert!(
            matches!(
                self.cx.graph.scopes.output_plan(op),
                OutputPlan::SetOperation { left: operand, .. } if *operand == owner
            ),
            "a merged column stands for a column of the left operand of its operation"
        );
        self.slot_value(left)
    }

    /// The column a set operation publishes at element `index` of its model.
    /// The operation can merge more or fewer mappings than its left operand
    /// lists columns, so then its mappings are matched by name instead.
    fn merged_slot(
        &mut self,
        scope: ScopeId,
        index: usize,
        name: &Ident,
    ) -> (Vec<ColumnOrigin>, TransformKind) {
        let model = self.scope_model(scope);
        let mappings = self.scope_mappings(scope);
        let found = if mappings.len() >= model.listed_len() {
            mappings.get(index)
        } else {
            mappings.iter().find(|mapping| {
                mapping.output_name.is_some()
                    && same_column_name(&mapping.target.column, name, self.cx.graph.dialect)
            })
        };
        found.map_or_else(
            || {
                (
                    vec![ColumnOrigin::Ambiguous {
                        column: name.clone(),
                        candidates: Vec::new(),
                    }],
                    TransformKind::Direct,
                )
            },
            |mapping| (mapping.sources.clone(), mapping.transform.clone()),
        )
    }

    /// The mapping standing for a published segment.
    pub(super) fn segment_mapping(&mut self, id: SegmentId) -> Option<ColumnMapping> {
        let def = self.cx.mapping_cache.models.arena.segment(id);
        if self.is_set_operation(def.owner) {
            return self.merged_segment(def.owner, def.index);
        }
        // TODO: re-entry must surface as an unresolved value, not as a
        // missing mapping that reads as "nothing published".
        if !self.cx.enter(EvalKey::Segment(id)) {
            return None;
        }
        let mapping = self.provider_mapping(&def.provider, def.position);
        self.cx.leave(EvalKey::Segment(id));
        mapping
    }

    /// The mapping a set operation publishes for its segment at element
    /// `index`.
    fn merged_segment(&mut self, scope: ScopeId, index: usize) -> Option<ColumnMapping> {
        let model = self.scope_model(scope);
        let mappings = self.scope_mappings(scope);
        if mappings.len() >= model.listed_len() {
            return mappings.get(index).cloned();
        }
        let ordinal = model.elements[..index]
            .iter()
            .filter(|element| matches!(element, OutputElement::Unknown(_)))
            .count();
        mappings
            .iter()
            .filter(|mapping| mapping.output_name.is_none())
            .nth(ordinal)
            .cloned()
    }

    /// What a published segment contributes to the column `column`: the
    /// wildcard origins of its provider, and for a segment of a set operation
    /// what the right operand contributes under that name too.
    pub(super) fn segment_sources(&mut self, id: SegmentId, column: &Ident) -> Vec<ColumnOrigin> {
        let def = self.cx.mapping_cache.models.arena.segment(id);
        // TODO: re-entry must surface as an unresolved value, not as an
        // empty source list.
        if !self.cx.enter(EvalKey::Segment(id)) {
            return Vec::new();
        }
        let mut sources = self.provider_sources(&def.provider, column);
        // The right operand of a set operation contributes to every
        // position, though the model lists only the left one.
        if let OutputPlan::SetOperation { right, .. } = self.cx.graph.scopes.output_plan(def.owner)
            && let Unlisted::Sources(more) = self.unlisted(*right, column)
        {
            sources.extend(more);
        }
        self.cx.leave(EvalKey::Segment(id));
        sources
    }

    fn is_set_operation(&self, scope: ScopeId) -> bool {
        matches!(
            self.cx.graph.scopes.output_plan(scope),
            OutputPlan::SetOperation { .. }
        )
    }

    /// The origins of the first right-operand column of a set operation that
    /// has the name `name`.
    pub(super) fn tail_named(
        &mut self,
        model: &OutputModel,
        name: &Ident,
    ) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
        let dialect = self.cx.graph.dialect;
        for element in &model.elements {
            let OutputElement::Tail(tail) = element else {
                continue;
            };
            if let Some(mapping) = self.tail_mappings(tail).into_iter().find(|mapping| {
                mapping.output_name.is_some()
                    && same_column_name(&mapping.target.column, name, dialect)
            }) {
                let (sources, has_back) = flatten_mapping_sources(&mapping.sources);
                return Some((sources, has_back, mapping.transform));
            }
        }
        None
    }

    /// The mappings a set operation merges in beyond those its left operand
    /// lists, minus the names the star's modifiers rule out.
    pub(super) fn tail_mappings(&mut self, tail: &BranchTail) -> Vec<ColumnMapping> {
        let aligned = self.aligned_len(tail.scope);
        let dialect = self.cx.graph.dialect;
        self.scope_mappings(tail.scope)
            .iter()
            .skip(aligned)
            .filter(|mapping| {
                mapping.output_name.is_none() || tail.domain.admits(&mapping.target.column, dialect)
            })
            .cloned()
            .collect()
    }

    /// What the right operand of a set operation merges in under `column`:
    /// the origins of mappings with that name, and a named wildcard for each
    /// relation it hides behind a star. A tail that has neither leaves the
    /// column unresolved.
    pub(super) fn tail_column(&mut self, scope: ScopeId, column: &Ident) -> Vec<ColumnOrigin> {
        let aligned = self.aligned_len(scope);
        let dialect = self.cx.graph.dialect;
        let mut sources = Vec::new();
        for mapping in self.scope_mappings(scope).iter().skip(aligned) {
            if mapping.output_name.is_none() {
                for source in &mapping.sources {
                    if let ColumnOrigin::Wildcard { table } = source {
                        sources.push(ColumnOrigin::NamedWildcard {
                            table: table.clone(),
                            column: column.clone(),
                        });
                    }
                }
            } else if same_column_name(&mapping.target.column, column, dialect) {
                sources.extend(mapping.sources.iter().cloned());
            }
        }
        if sources.is_empty() {
            sources.push(ColumnOrigin::Ambiguous {
                column: column.clone(),
                candidates: Vec::new(),
            });
        }
        sources
    }

    /// How many leading mappings of a scope stand for the columns its model
    /// lists; a set operation appends what its right operand adds after them.
    fn aligned_len(&mut self, scope: ScopeId) -> usize {
        match self.cx.graph.scopes.output_plan(scope) {
            OutputPlan::Projection => self.scope_mappings(scope).len(),
            OutputPlan::Delegate(child) => self.aligned_len(*child),
            OutputPlan::SetOperation { left, .. } => self.aligned_len(*left),
        }
    }

    pub(super) fn scope_mappings(&mut self, scope: ScopeId) -> std::sync::Arc<[ColumnMapping]> {
        super::super::mapping::resolve_scope_mappings(scope, self.cx)
    }
}

#[cfg(test)]
mod forward_tests;
#[cfg(test)]
mod harness;
