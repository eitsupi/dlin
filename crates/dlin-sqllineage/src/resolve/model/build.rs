//! Construction of the models of a statement: the cache that keeps each model
//! built once, the builder that walks scopes and stars, and the re-expression
//! of a scope's output under a reading star's range variables.

use std::collections::HashMap;
use std::rc::Rc;

use super::super::target::{StarTarget, resolve_star_target};
use super::input::{InputModel, Invalidity};
use super::options::apply_options;
use super::slot::{SegmentDef, SlotArena, SlotDef};
use super::{
    BranchTail, ColumnSlot, Forwarding, NameDomain, OpaqueReason, OutputElement, OutputModel,
    RangeRef, SlotOrigin, UnknownProvider, UnknownSegment, ValueId,
};
use crate::OutputName;
use crate::catalog::{CatalogContext, CatalogUnknownSegment, SchemaCoverage};
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode, StarBase, StarOptions};
use crate::graph::scope::{Binding, BindingEnv, OutputPlan, ScopeId, SetCorrespondence};
use crate::types::TableRef;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CacheKey {
    Scope(ScopeId),
    Input(BindingEnv),
    Star(NodeId),
}

/// The models built so far for one statement, each built at most once.
///
/// A model that was built while a cycle was cut (a recursive CTE reading
/// itself) is not kept, because it depends on where the walk started.
///
/// The store also owns the arena that defines every column and segment a scope
/// has published.
#[derive(Default)]
pub(in crate::resolve) struct ModelStore {
    pub(in crate::resolve) arena: SlotArena,
    /// How many times a scope has published its model.
    publications: u32,
    /// Every model a scope published, by publication. A scope whose model is
    /// built again while a cycle is cut publishes again, and a reader holds
    /// the outputs of the publication it read.
    views: HashMap<u32, Rc<OutputModel>>,
    scopes: HashMap<ScopeId, Rc<OutputModel>>,
    pub(super) inputs: HashMap<BindingEnv, Rc<InputModel>>,
    stars: HashMap<NodeId, Rc<OutputModel>>,
    /// Models under construction, each flagged when a cycle was cut beneath it.
    pub(super) active: Vec<(CacheKey, bool)>,
}

#[cfg(test)]
impl ModelStore {
    /// The model a scope has cached, when it has one.
    pub(in crate::resolve) fn cached(&self, scope: ScopeId) -> Option<&Rc<OutputModel>> {
        self.scopes.get(&scope)
    }
}

/// Builds models for a statement into a [`ModelStore`].
pub(in crate::resolve) struct OutputModels<'a, 'c, 's> {
    pub(super) graph: &'a RawGraph,
    pub(super) catalog: Option<&'a CatalogContext<'c>>,
    pub(super) store: &'s mut ModelStore,
}

impl<'a, 'c, 's> OutputModels<'a, 'c, 's> {
    pub(in crate::resolve) fn new(
        graph: &'a RawGraph,
        catalog: Option<&'a CatalogContext<'c>>,
        store: &'s mut ModelStore,
    ) -> Self {
        Self {
            graph,
            catalog,
            store,
        }
    }

    pub(in crate::resolve) fn graph(&self) -> &'a RawGraph {
        self.graph
    }

    /// The model that was published as `publication`.
    pub(in crate::resolve) fn view(&self, publication: u32) -> Rc<OutputModel> {
        self.store
            .views
            .get(&publication)
            .expect("a publication is registered when its model is built")
            .clone()
    }

    pub(in crate::resolve) fn scope(&mut self, scope: ScopeId) -> Rc<OutputModel> {
        if let Some(model) = self.store.scopes.get(&scope) {
            return model.clone();
        }
        if self.enter(CacheKey::Scope(scope)) {
            let mut model = OutputModel::opaque(OpaqueReason::Recursive, self.graph.dialect);
            let publication = self.next_publication();
            self.publish(scope, publication, &mut model.elements);
            model.publication = Some(publication);
            return self.register(publication, model);
        }
        let model = self.build_scope(scope);
        let publication = model.publication.expect("a scope's model is published");
        let model = self.register(publication, model);
        if self.leave() {
            self.store.scopes.insert(scope, model.clone());
        }
        model
    }

    pub(in crate::resolve) fn star(&mut self, node_id: NodeId) -> Rc<OutputModel> {
        if let Some(model) = self.store.stars.get(&node_id) {
            return model.clone();
        }
        if self.enter(CacheKey::Star(node_id)) {
            return Rc::new(OutputModel::opaque(
                OpaqueReason::Recursive,
                self.graph.dialect,
            ));
        }
        let graph = self.graph;
        let model = match &graph.nodes[node_id] {
            RawNode::Star { base, options, env } => self.build_star(node_id, base, options, *env),
            _ => OutputModel::opaque(OpaqueReason::Recursive, graph.dialect),
        };
        let model = Rc::new(model);
        if self.leave() {
            self.store.stars.insert(node_id, model.clone());
        }
        model
    }

    /// Start building a model. Returns `true` when the key is already under
    /// construction: the cycle is cut and every active model is marked as not
    /// reusable, including callers above the cycle entry that may retain a model
    /// built from the cut result.
    pub(super) fn enter(&mut self, key: CacheKey) -> bool {
        if self.store.active.iter().any(|(active, _)| *active == key) {
            for (_, tainted) in &mut self.store.active[..] {
                *tainted = true;
            }
            return true;
        }
        self.store.active.push((key, false));
        false
    }

    /// Finish the innermost model; returns whether it may be reused.
    pub(super) fn leave(&mut self) -> bool {
        self.store.active.pop().is_some_and(|(_, tainted)| !tainted)
    }

    fn next_publication(&mut self) -> u32 {
        let publication = self.store.publications;
        self.store.publications += 1;
        publication
    }

    fn register(&mut self, publication: u32, model: OutputModel) -> Rc<OutputModel> {
        let model = Rc::new(model);
        self.store.views.insert(publication, model.clone());
        model
    }

    fn build_scope(&mut self, scope: ScopeId) -> OutputModel {
        let graph = self.graph;
        let publication = self.next_publication();
        let mut invalid = Vec::new();
        let mut reads = Vec::new();
        // The scope and the model whose output this scope only re-expresses.
        let mut source = None;
        let mut elements = match graph.scopes.output_plan(scope) {
            OutputPlan::Projection => {
                let mut elements = Vec::new();
                let mut next_ordinal = Some(0usize);
                for column in graph.scopes.output_columns(scope) {
                    if matches!(graph.nodes[column.node_id], RawNode::Star { .. }) {
                        let star = self.star(column.node_id);
                        let width = star.width.exact();
                        let base_ordinal = next_ordinal;
                        for element in &star.elements {
                            let mut element = element.clone();
                            match &mut element {
                                OutputElement::Column(slot) => {
                                    slot.ordinal = slot.ordinal.and_then(|ordinal| {
                                        base_ordinal.and_then(|base| base.checked_add(ordinal))
                                    });
                                }
                                OutputElement::Unknown(segment) => {
                                    segment.position = segment.position.shifted(base_ordinal);
                                }
                                OutputElement::Tail(tail) => {
                                    tail.offset = tail.offset.and_then(|offset| {
                                        base_ordinal.and_then(|base| base.checked_add(offset))
                                    });
                                }
                            }
                            elements.push(element);
                        }
                        next_ordinal = next_ordinal
                            .and_then(|ordinal| width.and_then(|width| ordinal.checked_add(width)));
                        invalid.extend(star.invalid.iter().cloned());
                        reads.extend(star.reads.iter().cloned());
                    } else {
                        elements.push(OutputElement::Column(ColumnSlot {
                            id: None,
                            name: column.name.clone(),
                            output_name: graph
                                .output_names
                                .get(&column.node_id)
                                .cloned()
                                .unwrap_or_else(|| OutputName::Explicit(column.name.clone())),
                            ordinal: next_ordinal,
                            ranges: Rc::from([]),
                            candidate_ranges: Rc::from([]),
                            value: ValueId::Expr(column.node_id),
                            origin: SlotOrigin::Defined,
                        }));
                        next_ordinal = next_ordinal.and_then(|ordinal| ordinal.checked_add(1));
                    }
                }
                elements
            }
            OutputPlan::Delegate(child) => {
                let model = self.scope(*child);
                invalid.push(Invalidity::Whole(model.clone()));
                let elements = forward_scope(*child, &model, &Rc::from([]), None);
                source = Some((*child, model));
                elements
            }
            // The left operand defines the names of a set operation.
            OutputPlan::SetOperation {
                left,
                recursive,
                correspondence,
                ..
            } => {
                let model = self.scope(*left);
                invalid.push(Invalidity::Whole(model.clone()));
                let mut elements = forward_scope(*left, &model, &Rc::from([]), Some(scope));
                if *correspondence == SetCorrespondence::ByName {
                    // No right-operand names are enumerated until name-based
                    // correspondence is implemented. Keep the unknown remainder
                    // separate from the left operand's listed names.
                    elements.retain(|element| !matches!(element, OutputElement::Tail(_)));
                    if !elements
                        .iter()
                        .any(|element| matches!(element, OutputElement::Unknown(_)))
                    {
                        let mut remainder = UnknownSegment::opaque(OpaqueReason::Unsupported(
                            "BY NAME set operation",
                        ));
                        remainder.position = crate::types::StarPosition::Unproven;
                        remainder.domain.removed = elements
                            .iter()
                            .filter_map(|element| match element {
                                OutputElement::Column(slot) => Some(slot.name.clone()),
                                _ => None,
                            })
                            .collect();
                        elements.push(OutputElement::Unknown(remainder));
                    }
                } else if !recursive {
                    // This operation's tail covers whatever the left operand
                    // already carried.
                    elements.retain(|element| !matches!(element, OutputElement::Tail(_)));
                    elements.push(OutputElement::Tail(BranchTail {
                        scope,
                        publication,
                        ranges: Rc::from([]),
                        domain: NameDomain::default(),
                        offset: Some(0),
                    }));
                }
                source = Some((*left, model));
                elements
            }
        };
        // Only the output plan is followed, so a flagged operand shows up here
        // while an unrelated nested query does not.
        if graph.scopes.has_unknown_output_width(scope)
            && !elements
                .iter()
                .any(|element| matches!(element, OutputElement::Unknown(_)))
        {
            elements.push(OutputElement::Unknown(UnknownSegment::opaque(
                OpaqueReason::UnknownWidth,
            )));
        }
        let forwarded = self.publish(scope, publication, &mut elements);
        let mut model = OutputModel::new(
            elements,
            graph.scopes.unknown_output_reasons(scope),
            graph.dialect,
        );
        model.invalid = invalid;
        model.reads = reads;
        model.publication = Some(publication);
        if let Some((source_scope, source)) = source {
            model.forwarded = forwarded;
            if let Err(problem) =
                model.check_forwarding(&self.store.arena, scope, source_scope, &source)
            {
                debug_assert!(false, "scope {scope} publishes inconsistently: {problem}");
            }
        }
        model
    }

    /// Give each column and segment of a scope's model its identity and record
    /// its definition. The elements of star and FROM-clause models are views
    /// of other outputs and are never published.
    ///
    /// An element copied from another model (a star's, or a forwarded
    /// scope's) may still carry an identity that belongs to that model, so
    /// every element gets a new one. Returns which published output each
    /// element that forwards one stands for.
    fn publish(
        &mut self,
        scope: ScopeId,
        publication: u32,
        elements: &mut [OutputElement],
    ) -> Vec<Forwarding> {
        let mut forwarded = Vec::new();
        for element in elements.iter_mut() {
            match element {
                OutputElement::Column(slot) => {
                    slot.id = Some(self.store.arena.add_slot(SlotDef {
                        owner: scope,
                        name: slot.name.clone(),
                        value: slot.value.clone(),
                        publication,
                    }));
                }
                OutputElement::Unknown(segment) => {
                    segment.id = Some(self.store.arena.add_segment(SegmentDef {
                        owner: scope,
                        provider: segment.provider.clone(),
                        position: segment.position,
                        publication,
                    }));
                }
                OutputElement::Tail(_) => {}
            }
            if let (Some(from), Some(to)) = (element.forwards(), element.output_ref()) {
                forwarded.push(Forwarding { from, to });
            }
        }
        forwarded
    }

    fn build_star(
        &mut self,
        node_id: NodeId,
        base: &StarBase,
        options: &StarOptions,
        env: BindingEnv,
    ) -> OutputModel {
        let graph = self.graph;
        let opaque = |reason| vec![OutputElement::Unknown(UnknownSegment::opaque(reason))];
        let mut invalid = Vec::new();
        let mut reads = Vec::new();
        let elements = match resolve_star_target(base, env, graph) {
            StarTarget::All => {
                let input = self.input(env);
                invalid.extend(input.model.invalid.iter().cloned());
                reads.extend(input.model.reads.iter().cloned());
                input.star_elements()
            }
            StarTarget::Binding { name, binding } => {
                let range = RangeRef {
                    index: self.range_index(env, &name, &binding),
                    name,
                    binding,
                };
                if let Some(model) = self.binding_model(&range.binding) {
                    reads.push((range.index, model));
                }
                self.binding_part(&range)
            }
            StarTarget::Unknown(table) => self.relation_part(&Rc::new(table), &Rc::from([])),
            StarTarget::FieldPath { .. } => opaque(OpaqueReason::FieldPath(node_id)),
            StarTarget::Expr => opaque(OpaqueReason::Expression(node_id)),
            StarTarget::Ambiguous(_) => opaque(OpaqueReason::Ambiguous(node_id)),
        };
        let mut model = OutputModel::new(
            apply_options(base, options, elements, graph.dialect),
            Vec::new(),
            graph.dialect,
        );
        model.invalid = invalid;
        model.reads = reads;
        model
    }

    pub(super) fn binding_part(&mut self, range: &RangeRef) -> Vec<OutputElement> {
        let ranges: Rc<[RangeRef]> = Rc::from([range.clone()]);
        match &range.binding {
            Binding::Table(table) => self.relation_part(&Rc::new(table.clone()), &ranges),
            Binding::Cte(child) | Binding::DerivedTable(child) => {
                let model = self.scope(*child);
                forward_scope(*child, &model, &ranges, None)
            }
            Binding::VirtualSource(source) => self
                .graph
                .scopes
                .virtual_source(*source)
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    OutputElement::Column(ColumnSlot {
                        id: None,
                        name: column.name.clone(),
                        output_name: OutputName::Explicit(column.name.clone()),
                        ordinal: Some(index),
                        ranges: ranges.clone(),
                        candidate_ranges: ranges.clone(),
                        value: ValueId::VirtualColumn {
                            source: *source,
                            index,
                        },
                        origin: SlotOrigin::Forwarded(column.name.clone()),
                    })
                })
                .collect(),
        }
    }

    fn relation_part(&self, table: &Rc<TableRef>, ranges: &Rc<[RangeRef]>) -> Vec<OutputElement> {
        let Some(entry) = self.catalog.and_then(|catalog| catalog.entry(table)) else {
            return vec![OutputElement::Unknown(UnknownSegment {
                id: None,
                schema_relations: vec![super::super::analysis::plain_relation(table)],
                position: crate::types::StarPosition::Inferred,
                provider: UnknownProvider::Relation(table.clone()),
                ranges: ranges.clone(),
                domain: NameDomain::default(),
            })];
        };

        let mut columns: Vec<_> = entry.columns.iter().cloned().collect();
        // Individually proven positions also order the known part of a
        // partial schema; an unknown full order does not revoke those proofs.
        columns.sort_by_key(|column| column.ordinal.unwrap_or(usize::MAX));
        let listed_names: Vec<_> = columns.iter().map(|column| column.name.clone()).collect();
        let mut elements = Vec::new();
        for column in &columns {
            elements.push(OutputElement::Column(ColumnSlot {
                id: None,
                name: column.name.clone(),
                output_name: OutputName::Explicit(column.name.clone()),
                ordinal: column.ordinal,
                ranges: ranges.clone(),
                candidate_ranges: ranges.clone(),
                value: ValueId::Physical {
                    table: table.clone(),
                    column: column.name.clone(),
                },
                origin: SlotOrigin::Forwarded(column.name.clone()),
            }));
            if entry.coverage == SchemaCoverage::Partial
                && let Some(after) = column.ordinal
            {
                for segment in entry
                    .unknown_segments
                    .iter()
                    .filter(|segment| segment.after_ordinal == Some(after))
                {
                    elements.push(relation_unknown(
                        table,
                        ranges,
                        &listed_names,
                        *segment,
                        &entry.relation,
                    ));
                }
            }
        }

        if entry.coverage == SchemaCoverage::Partial {
            let pending = entry
                .unknown_segments
                .iter()
                .filter(|segment| {
                    segment.after_ordinal.is_none()
                        || !columns
                            .iter()
                            .any(|column| column.ordinal == segment.after_ordinal)
                })
                .copied()
                .chain(
                    entry
                        .unknown_segments
                        .is_empty()
                        .then_some(CatalogUnknownSegment {
                            after_ordinal: None,
                        }),
                );
            elements.extend(pending.map(|segment| {
                relation_unknown(table, ranges, &listed_names, segment, &entry.relation)
            }));
        }
        elements
    }
}

fn relation_unknown(
    table: &Rc<TableRef>,
    ranges: &Rc<[RangeRef]>,
    listed_names: &[crate::Ident],
    segment: CatalogUnknownSegment,
    canonical_relation: &crate::Relation,
) -> OutputElement {
    OutputElement::Unknown(UnknownSegment {
        id: None,
        schema_relations: vec![canonical_relation.clone()],
        position: segment.after_ordinal.map_or(
            crate::types::StarPosition::Unproven,
            crate::types::StarPosition::After,
        ),
        provider: UnknownProvider::Relation(table.clone()),
        ranges: ranges.clone(),
        domain: NameDomain {
            filters: Vec::new(),
            removed: listed_names.to_vec(),
        },
    })
}

/// Re-express the output of a scope under the range variables of the star
/// that reads it. Every element names the published output it forwards.
/// `merged_by` is the set operation whose left operand `scope` is, when the
/// elements are that operation's own.
pub(super) fn forward_scope(
    scope: ScopeId,
    model: &OutputModel,
    ranges: &Rc<[RangeRef]>,
    merged_by: Option<ScopeId>,
) -> Vec<OutputElement> {
    model
        .elements
        .iter()
        .map(|element| match element {
            OutputElement::Column(slot) => {
                let id = slot.published();
                OutputElement::Column(ColumnSlot {
                    id: None,
                    name: slot.name.clone(),
                    output_name: slot.output_name.clone(),
                    ordinal: slot.ordinal,
                    ranges: ranges.clone(),
                    candidate_ranges: ranges.clone(),
                    value: match merged_by {
                        Some(op) => ValueId::SetMerged { op, left: id },
                        None => ValueId::Forward(id),
                    },
                    origin: SlotOrigin::Forwarded(slot.name.clone()),
                })
            }
            OutputElement::Unknown(segment) => {
                let id = segment.published();
                OutputElement::Unknown(UnknownSegment {
                    id: None,
                    schema_relations: segment.schema_relations.clone(),
                    position: segment.position,
                    provider: match merged_by {
                        Some(op) => UnknownProvider::SetOperation { op, left: id },
                        None => UnknownProvider::Segment { scope, id },
                    },
                    ranges: ranges.clone(),
                    domain: segment.domain.clone(),
                })
            }
            OutputElement::Tail(tail) => OutputElement::Tail(BranchTail {
                scope: tail.scope,
                publication: tail.publication,
                ranges: ranges.clone(),
                domain: tail.domain.clone(),
                offset: Some(0),
            }),
        })
        .collect()
}
