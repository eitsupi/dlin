//! Evaluation of the columns and segments a scope has published, by id, and
//! of the set-operation merge those ids are looked up in.
//!
//! A reader that resolved a name to a published column holds its id, so what
//! it evaluates is that column and never another one that shares its name or
//! its position.
//!
//! Each column and each segment is evaluated once. A value computed while a
//! cycle was cut is returned and not kept (see the context module), and a
//! cut is always reported as an unresolved value, never as an empty one.

#[cfg(test)]
use std::cell::RefCell;

use super::{Evaluator, Unlisted};
use crate::Ident;
use crate::graph::scope::{OutputPlan, ScopeId};
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, StarPosition, TransformKind};

use super::super::context::{EvalKey, SlotValue};
use super::super::lineage::flatten_mapping_sources;
use super::super::mapping::Cut;
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
        if let Some(value) = self.cx.slot_values.get(&id) {
            return (value.sources.clone(), value.transform.clone());
        }
        let def = self.cx.mapping_cache.models.arena.slot(id);
        let generation = self.cx.cut_generation();
        if !self.cx.enter(EvalKey::Slot(id)) {
            return (cut_sources(&def.name), TransformKind::Direct);
        }
        #[cfg(test)]
        EVALUATED_SLOTS.with(|slots| slots.borrow_mut().push(id));
        let (sources, transform) = if self.is_set_operation(def.owner) {
            self.merged_slot(def.owner, def.index, &def.name)
        } else {
            self.value(&def.value)
        };
        self.cx.leave(EvalKey::Slot(id));
        if self.cx.cut_generation() == generation {
            self.cx.slot_values.insert(
                id,
                SlotValue {
                    sources: sources.clone(),
                    transform: transform.clone(),
                },
            );
        }
        (sources, transform)
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
        let Ok(mappings) = self.scope_mappings(scope) else {
            return (cut_sources(name), TransformKind::Direct);
        };
        let found = if mappings.len() >= model.listed_len() {
            mappings.get(index)
        } else {
            mappings.iter().find(|mapping| {
                mapping.output_name.is_some()
                    && same_column_name(&mapping.target.column, name, self.cx.graph.dialect)
            })
        };
        found.map_or_else(
            || (cut_sources(name), TransformKind::Direct),
            |mapping| (mapping.sources.clone(), mapping.transform.clone()),
        )
    }

    /// The mapping standing for a published segment, at the position its
    /// scope gives it. A segment whose evaluation is already in progress
    /// depends on itself and stands for columns of unknown width.
    pub(super) fn segment_mapping(&mut self, id: SegmentId) -> Option<ColumnMapping> {
        if let Some(mapping) = self.cx.segment_mappings.get(&id) {
            return mapping.clone();
        }
        let def = self.cx.mapping_cache.models.arena.segment(id);
        let generation = self.cx.cut_generation();
        let mapping = if self.is_set_operation(def.owner) {
            self.merged_segment(def.owner, def.index)
        } else if self.cx.enter(EvalKey::Segment(id)) {
            let mapping = self.provider_mapping(&def.provider, def.position);
            self.cx.leave(EvalKey::Segment(id));
            mapping
        } else {
            Some(cut_segment())
        };
        if self.cx.cut_generation() == generation {
            self.cx.segment_mappings.insert(id, mapping.clone());
        }
        mapping
    }

    /// The mapping a set operation publishes for its segment at element
    /// `index`.
    fn merged_segment(&mut self, scope: ScopeId, index: usize) -> Option<ColumnMapping> {
        let model = self.scope_model(scope);
        let Ok(mappings) = self.scope_mappings(scope) else {
            return Some(cut_segment());
        };
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
    ///
    /// The answer is specific to the name, so it is derived from the
    /// segment's definition each time and never kept under the segment alone.
    pub(super) fn segment_sources(&mut self, id: SegmentId, column: &Ident) -> Vec<ColumnOrigin> {
        let def = self.cx.mapping_cache.models.arena.segment(id);
        if !self.cx.enter(EvalKey::Segment(id)) {
            return cut_sources(column);
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
        let dialect = self.cx.graph.dialect;
        let (Some(aligned), Ok(mappings)) = (
            self.aligned_len(tail.scope),
            self.scope_mappings(tail.scope),
        ) else {
            // What the right operand adds cannot be listed.
            return vec![cut_segment()];
        };
        mappings
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
        let dialect = self.cx.graph.dialect;
        let (Some(aligned), Ok(mappings)) = (self.aligned_len(scope), self.scope_mappings(scope))
        else {
            return cut_sources(column);
        };
        let mut sources = Vec::new();
        for mapping in mappings.iter().skip(aligned) {
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
            sources = cut_sources(column);
        }
        sources
    }

    /// How many leading mappings of a scope stand for the columns its model
    /// lists; a set operation appends what its right operand adds after them.
    /// `None` when the mappings cannot be known.
    pub(super) fn aligned_len(&mut self, scope: ScopeId) -> Option<usize> {
        match self.cx.graph.scopes.output_plan(scope) {
            OutputPlan::Projection => self
                .scope_mappings(scope)
                .ok()
                .map(|mappings| mappings.len()),
            OutputPlan::Delegate(child) => self.aligned_len(*child),
            OutputPlan::SetOperation { left, .. } => self.aligned_len(*left),
        }
    }

    pub(super) fn scope_mappings(
        &mut self,
        scope: ScopeId,
    ) -> Result<std::sync::Arc<[ColumnMapping]>, Cut> {
        super::super::mapping::resolve_scope_mappings(scope, self.cx)
    }
}

/// The origin of a column whose value cannot be determined because its
/// evaluation depends on itself.
fn cut_sources(column: &Ident) -> Vec<ColumnOrigin> {
    vec![ColumnOrigin::Ambiguous {
        column: column.clone(),
        candidates: Vec::new(),
    }]
}

/// The mapping of a segment whose evaluation depends on itself: columns the
/// analysis cannot enumerate.
fn cut_segment() -> ColumnMapping {
    ColumnMapping {
        star_position: StarPosition::Inferred,
        output_name: None,
        ordinal: None,
        target: ColumnRef {
            table: None,
            column: Ident::unquoted("*"),
        },
        sources: vec![ColumnOrigin::UnknownStar],
        transform: TransformKind::Direct,
    }
}

#[cfg(test)]
mod cut_tests;
#[cfg(test)]
mod forward_tests;
#[cfg(test)]
mod harness;
