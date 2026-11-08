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
use crate::graph::scope::{OutputPlan, ScopeId};
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, StarPosition, TransformKind};
use crate::{Ident, Unresolved};

use super::super::context::{EvalKey, SlotValue};
use super::super::lineage::flatten_mapping_sources;
use super::super::mapping::Cut;
use super::super::model::{
    BranchTail, OutputElement, OutputModel, OutputRef, SegmentId, SlotId, UnknownProvider,
};
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
            self.merged_slot(def.owner, id, &def.name)
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
        if let Some(OutputRef::Slot(merged)) = self.merged_counterpart(op, OutputRef::Slot(left)) {
            self.slot_value(merged)
        } else {
            let name = self.cx.mapping_cache.models.arena.slot(left).name.clone();
            (cut_sources(&name), TransformKind::Direct)
        }
    }

    /// The output of the set operation `op` that stands for `from`, an output
    /// of its left operand, from the correspondence `op` recorded when it
    /// published. `None` when `op` forwarded another publication of its left
    /// operand, which is a cut.
    pub(super) fn merged_counterpart(&mut self, op: ScopeId, from: OutputRef) -> Option<OutputRef> {
        let model = self.scope_model(op);
        let counterpart = model
            .forwarded
            .iter()
            .find(|pair| pair.from == from)
            .map(|pair| pair.to);
        if counterpart.is_none() {
            let arena = &self.cx.mapping_cache.models.arena;
            let publication = arena.publication(from);
            debug_assert!(
                model
                    .forwarded
                    .iter()
                    .all(|pair| arena.publication(pair.from) != publication),
                "a set operation publishes an output for each output of its left operand"
            );
            self.cx.record_cut();
        }
        counterpart
    }

    /// The column `id` of the set operation `scope`: the mapping the
    /// operation's merge tagged with it.
    fn merged_slot(
        &mut self,
        scope: ScopeId,
        id: SlotId,
        name: &Ident,
    ) -> (Vec<ColumnOrigin>, TransformKind) {
        let output = OutputRef::Slot(id);
        let Ok(mappings) = self.published_mappings(scope, output) else {
            return (cut_sources(name), TransformKind::Direct);
        };
        if let Some(mapping) = mappings.iter().find(|mapping| mapping.slot == Some(output)) {
            return (mapping.sources.clone(), mapping.transform.clone());
        }
        debug_assert!(false, "a set operation's column has no merged mapping");
        (vec![output_correspondence_lost()], TransformKind::Direct)
    }

    /// The mappings of `scope` for the publication of its model that
    /// published `output`.
    fn published_mappings(
        &mut self,
        scope: ScopeId,
        output: OutputRef,
    ) -> Result<std::sync::Arc<[ColumnMapping]>, Cut> {
        let publication = self.cx.mapping_cache.models.arena.publication(output);
        self.publication_mappings(scope, publication)
    }

    /// The mappings a set operation's tail is read from. They may contain
    /// unresolved markers from a cut, which callers filter without caching.
    fn tail_source(
        &mut self,
        scope: ScopeId,
        publication: u32,
    ) -> Result<std::sync::Arc<[ColumnMapping]>, Cut> {
        self.publication_mappings(scope, publication)
    }

    fn publication_mappings(
        &mut self,
        scope: ScopeId,
        publication: u32,
    ) -> Result<std::sync::Arc<[ColumnMapping]>, Cut> {
        let view = self.models().view(publication);
        super::super::mapping::mappings_of(scope, &view, self.cx)
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
            self.merged_segment(def.owner, id)
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

    /// The mapping a set operation publishes for its segment `id`: the one
    /// its merge tagged with it. A segment that stands for no columns has none.
    fn merged_segment(&mut self, scope: ScopeId, id: SegmentId) -> Option<ColumnMapping> {
        let output = OutputRef::Segment(id);
        let Ok(mappings) = self.published_mappings(scope, output) else {
            return Some(cut_segment());
        };
        if let Some(mapping) = mappings.iter().find(|mapping| mapping.slot == Some(output)) {
            return Some(mapping.clone());
        }
        let def = self.cx.mapping_cache.models.arena.segment(id);
        let stands_for_columns = match def.provider {
            UnknownProvider::SetOperation { left, .. } => self.segment_mapping(left).is_some(),
            _ => false,
        };
        if !stands_for_columns {
            return None;
        }
        debug_assert!(false, "a set operation's segment has no merged mapping");
        Some(cut_segment())
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
        // A segment a set operation publishes of its own, rather than one it
        // forwards from its left operand, is still read next to the right
        // operand's columns.
        if !matches!(def.provider, UnknownProvider::SetOperation { .. })
            && let OutputPlan::SetOperation { right, .. } =
                self.cx.graph.scopes.output_plan(def.owner)
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
        let generation = self.cx.cut_generation();
        let mappings = match self.tail_source(tail.scope, tail.publication) {
            Ok(mappings) => mappings,
            // What the right operand adds cannot be listed.
            Err(Cut) => return vec![cut_segment()],
        };
        mappings
            .iter()
            .filter(|mapping| mapping.merged_in)
            // Under a cut, a named right-only column cannot be published
            // against the unknown left shape. Keep only the explicit open
            // segment markers from that tail.
            .filter(|mapping| {
                self.cx.cut_generation() == generation || mapping.output_name.is_none()
            })
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
    pub(super) fn tail_column(
        &mut self,
        scope: ScopeId,
        publication: u32,
        column: &Ident,
    ) -> Vec<ColumnOrigin> {
        let dialect = self.cx.graph.dialect;
        let generation = self.cx.cut_generation();
        let mappings = match self.tail_source(scope, publication) {
            Ok(mappings) => mappings,
            Err(Cut) => return cut_sources(column),
        };
        let mut sources = Vec::new();
        for mapping in mappings.iter().filter(|mapping| mapping.merged_in) {
            if self.cx.cut_generation() != generation && mapping.output_name.is_some() {
                continue;
            }
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
}

/// The origin of a column that a set operation should have a merged mapping
/// for and has none. This is an internal inconsistency and is reported as an
/// unresolved value of its own.
fn output_correspondence_lost() -> ColumnOrigin {
    ColumnOrigin::Unresolved(Unresolved::Unsupported {
        what: "set operation output correspondence",
    })
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
pub(super) fn cut_segment() -> ColumnMapping {
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
        slot: None,
        unpublished_tail: false,
        merged_in: false,
    }
}

#[cfg(test)]
mod cut_tests;
#[cfg(test)]
mod forward_tests;
#[cfg(test)]
mod harness;
#[cfg(test)]
mod projection_tests;
