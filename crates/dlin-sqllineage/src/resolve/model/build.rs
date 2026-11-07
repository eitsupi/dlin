//! Construction of the models of a statement: the cache that keeps each model
//! built once, the builder that walks scopes and stars, and the re-expression
//! of a scope's output under a reading star's range variables.

use std::collections::HashMap;
use std::rc::Rc;

use super::super::target::{StarTarget, resolve_star_target};
use super::input::{InputModel, Invalidity};
use super::options::apply_options;
use super::{
    BranchTail, ColumnSlot, NameDomain, OpaqueReason, OutputElement, OutputModel, RangeRef,
    SlotOrigin, UnknownProvider, UnknownSegment, ValueId,
};
use crate::OutputName;
use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode, StarBase, StarOptions};
use crate::graph::scope::{Binding, BindingEnv, OutputPlan, ScopeId};
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
#[derive(Default)]
pub(in crate::resolve) struct ModelStore {
    scopes: HashMap<ScopeId, Rc<OutputModel>>,
    pub(super) inputs: HashMap<BindingEnv, Rc<InputModel>>,
    stars: HashMap<NodeId, Rc<OutputModel>>,
    /// Models under construction, each flagged when a cycle was cut beneath it.
    pub(super) active: Vec<(CacheKey, bool)>,
}

/// Builds models for a statement into a [`ModelStore`].
pub(in crate::resolve) struct OutputModels<'a, 'c, 's> {
    pub(super) graph: &'a RawGraph,
    catalog: Option<&'a CatalogContext<'c>>,
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

    pub(in crate::resolve) fn scope(&mut self, scope: ScopeId) -> Rc<OutputModel> {
        if let Some(model) = self.store.scopes.get(&scope) {
            return model.clone();
        }
        if self.enter(CacheKey::Scope(scope)) {
            return Rc::new(OutputModel::opaque(
                OpaqueReason::Recursive,
                self.graph.dialect,
            ));
        }
        let model = Rc::new(self.build_scope(scope));
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
    /// construction: the cycle is cut and every model built since then is
    /// marked as not reusable.
    pub(super) fn enter(&mut self, key: CacheKey) -> bool {
        if let Some(position) = self
            .store
            .active
            .iter()
            .position(|(active, _)| *active == key)
        {
            for (_, tainted) in &mut self.store.active[position + 1..] {
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

    fn build_scope(&mut self, scope: ScopeId) -> OutputModel {
        let graph = self.graph;
        let mut invalid = Vec::new();
        let mut reads = Vec::new();
        let mut elements = match graph.scopes.output_plan(scope) {
            OutputPlan::Projection => {
                let mut elements = Vec::new();
                for column in graph.scopes.output_columns(scope) {
                    if matches!(graph.nodes[column.node_id], RawNode::Star { .. }) {
                        let star = self.star(column.node_id);
                        elements.extend(star.elements.iter().cloned());
                        invalid.extend(star.invalid.iter().cloned());
                        reads.extend(star.reads.iter().cloned());
                    } else {
                        elements.push(OutputElement::Column(ColumnSlot {
                            name: column.name.clone(),
                            output_name: graph
                                .output_names
                                .get(&column.node_id)
                                .cloned()
                                .unwrap_or_else(|| OutputName::Explicit(column.name.clone())),
                            ranges: Rc::from([]),
                            candidate_ranges: Rc::from([]),
                            value: ValueId::Expr(column.node_id),
                            origin: SlotOrigin::Defined,
                        }));
                    }
                }
                elements
            }
            OutputPlan::Delegate(child) => {
                let model = self.scope(*child);
                invalid.push(Invalidity::Whole(model.clone()));
                forward_scope(*child, &model, &Rc::from([]), false)
            }
            // The left operand defines the names of a set operation.
            OutputPlan::SetOperation {
                left, recursive, ..
            } => {
                let model = self.scope(*left);
                invalid.push(Invalidity::Whole(model.clone()));
                let mut elements = forward_scope(*left, &model, &Rc::from([]), true);
                if !recursive {
                    // This operation's tail covers whatever the left operand
                    // already carried.
                    elements.retain(|element| !matches!(element, OutputElement::Tail(_)));
                    elements.push(OutputElement::Tail(BranchTail {
                        scope,
                        ranges: Rc::from([]),
                        domain: NameDomain::default(),
                    }));
                }
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
        let mut model = OutputModel::new(
            elements,
            graph.scopes.unknown_output_reasons(scope),
            graph.dialect,
        );
        model.invalid = invalid;
        model.reads = reads;
        model
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
                forward_scope(*child, &model, &ranges, false)
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
                        name: column.name.clone(),
                        output_name: OutputName::Explicit(column.name.clone()),
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
        match self.catalog.and_then(|catalog| catalog.list_columns(table)) {
            Some(columns) => columns
                .into_iter()
                .map(|column| {
                    OutputElement::Column(ColumnSlot {
                        name: column.clone(),
                        output_name: OutputName::Explicit(column.clone()),
                        ranges: ranges.clone(),
                        candidate_ranges: ranges.clone(),
                        value: ValueId::Physical {
                            table: table.clone(),
                            column: column.clone(),
                        },
                        origin: SlotOrigin::Forwarded(column),
                    })
                })
                .collect(),
            None => vec![OutputElement::Unknown(UnknownSegment {
                provider: UnknownProvider::Relation(table.clone()),
                ranges: ranges.clone(),
                domain: NameDomain::default(),
            })],
        }
    }
}

/// Re-express the output of a scope under the range variables of the star
/// that reads it. `set_operation` marks slots that combine several operands.
pub(super) fn forward_scope(
    scope: ScopeId,
    model: &OutputModel,
    ranges: &Rc<[RangeRef]>,
    set_operation: bool,
) -> Vec<OutputElement> {
    model
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| match element {
            OutputElement::Column(slot) => OutputElement::Column(ColumnSlot {
                name: slot.name.clone(),
                output_name: slot.output_name.clone(),
                ranges: ranges.clone(),
                candidate_ranges: ranges.clone(),
                value: if set_operation {
                    ValueId::SetOperation { scope, index }
                } else {
                    ValueId::ScopeSlot { scope, index }
                },
                origin: SlotOrigin::Forwarded(slot.name.clone()),
            }),
            OutputElement::Unknown(segment) => OutputElement::Unknown(UnknownSegment {
                provider: UnknownProvider::Scope { scope, index },
                ranges: ranges.clone(),
                domain: segment.domain.clone(),
            }),
            OutputElement::Tail(tail) => OutputElement::Tail(BranchTail {
                scope: tail.scope,
                ranges: ranges.clone(),
                domain: tail.domain.clone(),
            }),
        })
        .collect()
}
