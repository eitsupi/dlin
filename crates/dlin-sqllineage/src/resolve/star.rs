//! Evaluation of star models into column mappings and name lookups.
//!
//! A star's model already records which columns it publishes, in which order
//! and under which names. This module only turns the lazy values of that model
//! into lineage: it never re-reads EXCLUDE/ILIKE/REPLACE/RENAME or join
//! merges.

use std::collections::HashSet;
use std::rc::Rc;

use super::binding::{derive_transform, resolve_captured_binding};
use super::context::{EvalContext, EvalKey};
use super::lineage::{collect_leaf_origins, collect_output_sources, flatten_mapping_sources};
use super::mapping::{
    known_empty_dependency, merge_transform, resolve_scope_mappings, wildcard_mapping,
};
use super::model::{
    BranchTail, Choice, InputLookup, ModelStore, OpaqueReason, OutputElement, OutputModel,
    OutputModels, RangeRef, UnknownProvider, UnknownSegment, ValueId,
};
use super::target::{StarTarget, resolve_star_target, same_column_name};

use crate::graph::node::{NodeId, RawNode, StarBase};
use crate::graph::scope::{Binding, BindingEnv, OutputPlan, ScopeId, VirtualColumnState};
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TransformKind};
use crate::{Candidate, Ident, Unresolved};

/// What a scope publishes under a name that none of its listed columns has.
pub(super) enum Unlisted {
    /// Nothing unlisted could carry the name.
    None,
    /// Unlisted columns exist, but the star modifiers rule the name out.
    Denied,
    /// Origins of the unlisted columns that may carry the name.
    Sources(Vec<ColumnOrigin>),
}

/// Expand a Star node (qualified or unqualified) into `ColumnMapping`s, one
/// per element of its model.
pub(super) fn expand_star(
    node_id: NodeId,
    cx: &mut EvalContext<'_, '_>,
    mappings: &mut Vec<ColumnMapping>,
) {
    let mut evaluator = Evaluator { cx };
    let model = evaluator.star_model(node_id);
    for element in &model.elements {
        match element {
            OutputElement::Column(slot) => {
                let (sources, transform) = evaluator.value(&slot.value);
                mappings.push(ColumnMapping {
                    output_name: Some(slot.output_name.clone()),
                    target: ColumnRef {
                        table: None,
                        column: slot.name.clone(),
                    },
                    sources,
                    transform,
                });
            }
            OutputElement::Unknown(segment) => {
                mappings.extend(evaluator.unknown_mapping(segment));
            }
            OutputElement::Tail(tail) => mappings.extend(evaluator.tail_mappings(tail)),
        }
    }
}

/// The origins of the column a CTE or derived table publishes under `name`,
/// with the transform that scope applies to it.
pub(super) fn named_scope_sources(
    name: &Ident,
    target_scope: ScopeId,
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    Evaluator { cx }.named(target_scope, name)
}

/// What an unqualified name in the FROM clause at `env` denotes.
pub(super) fn lookup_input(
    env: BindingEnv,
    name: &Ident,
    cx: &mut EvalContext<'_, '_>,
) -> InputLookup {
    Evaluator { cx }.models().lookup_input(env, name)
}

/// The origins of an unqualified name in the FROM clause at `env`, read from
/// the same folded model a star over that clause expands. `None` when a
/// single CTE or derived table decides the name and publishes nothing under
/// it, or when the name is read from a virtual source.
pub(super) fn input_name_sources(
    env: BindingEnv,
    name: &Ident,
    cx: &mut EvalContext<'_, '_>,
) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
    let lookup = lookup_input(env, name, cx);
    Evaluator { cx }.input(lookup, name)
}

/// `ColumnOrigin` has no equality of its own; its debug form covers every field.
/// Two relations often reach the same underlying column, and a merged column
/// lists it once. Distinct spellings stay apart.
fn same_origin(left: &ColumnOrigin, right: &ColumnOrigin) -> bool {
    format!("{left:?}") == format!("{right:?}")
}

/// Evaluation of models into lineage. It holds no state of its own: what is
/// being evaluated and what is already computed live in the shared context, so
/// a walk that leaves this evaluator and comes back is still recognized.
struct Evaluator<'x, 'a, 'c> {
    cx: &'x mut EvalContext<'a, 'c>,
}

impl Evaluator<'_, '_, '_> {
    fn models(&mut self) -> OutputModels<'_, '_, '_> {
        let cx = &mut *self.cx;
        let store: &mut ModelStore = &mut cx.mapping_cache.models;
        OutputModels::new(cx.graph, cx.catalog, store)
    }

    fn star_model(&mut self, node_id: NodeId) -> Rc<OutputModel> {
        self.models().star(node_id)
    }

    fn scope_model(&mut self, scope: ScopeId) -> Rc<OutputModel> {
        self.models().scope(scope)
    }

    fn value(&mut self, value: &ValueId) -> (Vec<ColumnOrigin>, TransformKind) {
        match value {
            ValueId::Physical { table, column } => (
                vec![ColumnOrigin::Concrete {
                    table: (**table).clone(),
                    column: column.clone(),
                }],
                TransformKind::Direct,
            ),
            ValueId::Expr(node_id) => {
                let (sources, edge_kinds, _, inherited) =
                    collect_output_sources(*node_id, self.cx, &mut HashSet::new());
                let transform = merge_transform(
                    &derive_transform(&self.cx.graph.nodes[*node_id], &edge_kinds),
                    &inherited,
                );
                (sources, transform)
            }
            ValueId::ScopeSlot { scope, index } | ValueId::SetOperation { scope, index } => {
                let (sources, _, transform) = self.scope_slot(*scope, *index, false);
                (sources, transform)
            }
            // The same lookup a direct reference to the name in that scope makes.
            ValueId::ScopeName { scope, column } => named_scope_sources(column, *scope, self.cx)
                .map_or_else(
                    || {
                        (
                            vec![ColumnOrigin::Ambiguous {
                                column: column.clone(),
                                candidates: Vec::new(),
                            }],
                            TransformKind::Direct,
                        )
                    },
                    |(sources, _, transform)| (sources, transform),
                ),
            ValueId::Tail { scope, column } => {
                (self.tail_column(*scope, column), TransformKind::Direct)
            }
            ValueId::VirtualColumn { source, index } => {
                (self.virtual_column(*source, *index), TransformKind::Direct)
            }
            ValueId::Named { providers, column } => (
                providers
                    .iter()
                    .flat_map(|provider| self.provider_sources(provider, column))
                    .collect(),
                TransformKind::Direct,
            ),
            ValueId::Merged(left, right) => {
                let (mut sources, left_transform) = self.value(left);
                let (right_sources, right_transform) = self.value(right);
                sources.extend(right_sources);
                (sources, merge_transform(&left_transform, &right_transform))
            }
            ValueId::Uncertain { column, candidates } => (
                vec![ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                    column: column.clone(),
                    candidates: candidates.iter().filter_map(RangeRef::candidate).collect(),
                })],
                TransformKind::Direct,
            ),
        }
    }

    /// The origins and transform of a column that a reference to `name`
    /// reads, with whether they read the recursive CTE the column belongs to.
    /// A column read this way is spelled as the reference spells it.
    fn read_value(
        &mut self,
        value: &ValueId,
        name: &Ident,
    ) -> (Vec<ColumnOrigin>, bool, TransformKind) {
        match value {
            ValueId::Physical { table, .. } => (
                vec![ColumnOrigin::Concrete {
                    table: (**table).clone(),
                    column: name.clone(),
                }],
                false,
                TransformKind::Direct,
            ),
            ValueId::ScopeSlot { scope, index } => self.scope_slot(*scope, *index, true),
            ValueId::Merged(left, right) => {
                let (mut sources, left_back, left_transform) = self.read_value(left, name);
                let (right_sources, right_back, right_transform) = self.read_value(right, name);
                for source in right_sources {
                    if !sources.iter().any(|seen| same_origin(seen, &source)) {
                        sources.push(source);
                    }
                }
                (
                    sources,
                    left_back || right_back,
                    merge_transform(&left_transform, &right_transform),
                )
            }
            other => {
                let (sources, transform) = self.value(other);
                (sources, false, transform)
            }
        }
    }

    /// Evaluate the answer of an input lookup.
    fn input(
        &mut self,
        lookup: InputLookup,
        name: &Ident,
    ) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
        let unbound = || ColumnOrigin::Ambiguous {
            column: name.clone(),
            candidates: Vec::new(),
        };
        match lookup {
            InputLookup::NoSource => None,
            InputLookup::Absent => Some((vec![unbound()], false, TransformKind::Direct)),
            InputLookup::Physical(table) => Some((
                vec![ColumnOrigin::Concrete {
                    table: (*table).clone(),
                    column: name.clone(),
                }],
                false,
                TransformKind::Direct,
            )),
            InputLookup::Scope { scope, .. } => self.named(scope, name),
            InputLookup::Column { value, .. } => Some(self.read_value(&value, name)),
            InputLookup::Ambiguous {
                candidates,
                typed,
                values,
            } => {
                // The value is not known, but what would produce it is: an
                // aggregate over an ambiguous column is still an aggregation.
                let mut transform = TransformKind::Direct;
                for value in &values {
                    let (_, _, listed) = self.read_value(value, name);
                    transform = merge_transform(&transform, &listed);
                }
                let origin = if typed {
                    ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                        column: name.clone(),
                        candidates,
                    })
                } else {
                    ColumnOrigin::Ambiguous {
                        column: name.clone(),
                        candidates: candidates
                            .into_iter()
                            .filter_map(|candidate| match candidate {
                                Candidate::Relation(relation) => Some(crate::types::TableRef {
                                    catalog: relation.catalog,
                                    schema: relation.schema,
                                    table: relation.name,
                                }),
                                _ => None,
                            })
                            .collect(),
                    }
                };
                Some((vec![origin], false, transform))
            }
        }
    }

    /// The origins of the column a CTE or derived table publishes under
    /// `name`: a listed column, a column of a set operation's tail, or
    /// whatever its unknown segments may publish.
    fn named(
        &mut self,
        scope: ScopeId,
        name: &Ident,
    ) -> Option<(Vec<ColumnOrigin>, bool, TransformKind)> {
        let model = self.scope_model(scope);
        match model.choose(name) {
            Choice::Column(index, _) => return Some(self.scope_slot(scope, index, true)),
            Choice::Ambiguous(ambiguity) => {
                let mut candidates = ambiguity.candidates;
                if candidates.is_empty() {
                    // Columns without a range variable are attributed to the
                    // scope itself, as the CTE or derived table that is read.
                    candidates.extend(self.cx.graph.scopes.reading_binding(scope).map(
                        |(name, binding)| match binding {
                            Binding::Cte(_) => Candidate::Cte(name),
                            _ => Candidate::Derived(Some(name)),
                        },
                    ));
                }
                let mut transform = TransformKind::Direct;
                for &index in &ambiguity.listed {
                    let (_, _, listed) = self.scope_slot(scope, index, true);
                    transform = merge_transform(&transform, &listed);
                }
                let ambiguous = ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                    column: name.clone(),
                    candidates,
                });
                return Some((vec![ambiguous], false, transform));
            }
            Choice::Possible(..) | Choice::Absent => {}
        }
        if let Some(tail) = self.tail_named(&model, name) {
            return Some(tail);
        }
        match self.unlisted(scope, name) {
            Unlisted::Denied => Some((
                vec![ColumnOrigin::Ambiguous {
                    column: name.clone(),
                    candidates: Vec::new(),
                }],
                false,
                TransformKind::Direct,
            )),
            Unlisted::Sources(sources) => Some((sources, false, TransformKind::Direct)),
            Unlisted::None => None,
        }
    }

    /// The value of a scope's output column at the position its model lists
    /// it, with whether it reads the recursive CTE it belongs to. A set
    /// operation can merge more or fewer mappings than its left operand lists
    /// columns, so its mappings are matched by name instead. `flatten` unwraps
    /// the recursive marker of a set operation's mapping.
    fn scope_slot(
        &mut self,
        scope: ScopeId,
        index: usize,
        flatten: bool,
    ) -> (Vec<ColumnOrigin>, bool, TransformKind) {
        let model = self.scope_model(scope);
        let Some(OutputElement::Column(slot)) = model.elements.get(index) else {
            return (Vec::new(), false, TransformKind::Direct);
        };
        let ambiguous = || {
            (
                vec![ColumnOrigin::Ambiguous {
                    column: slot.name.clone(),
                    candidates: Vec::new(),
                }],
                false,
                TransformKind::Direct,
            )
        };
        if !self.cx.enter(EvalKey::Slot { scope, index }) {
            return ambiguous();
        }
        let result = if matches!(
            self.cx.graph.scopes.output_plan(scope),
            OutputPlan::SetOperation { .. }
        ) {
            let mappings = self.scope_mappings(scope);
            let found = if mappings.len() >= model.listed_len() {
                mappings.get(index)
            } else {
                mappings.iter().find(|mapping| {
                    mapping.output_name.is_some()
                        && same_column_name(
                            &mapping.target.column,
                            &slot.name,
                            self.cx.graph.dialect,
                        )
                })
            };
            found.map_or_else(ambiguous, |mapping| {
                if flatten {
                    let (sources, has_back) = flatten_mapping_sources(&mapping.sources);
                    (sources, has_back, mapping.transform.clone())
                } else {
                    (mapping.sources.clone(), false, mapping.transform.clone())
                }
            })
        } else {
            let (sources, transform) = self.value(&slot.value);
            (sources, false, transform)
        };
        self.cx.leave(EvalKey::Slot { scope, index });
        result
    }

    /// The origins of the first right-operand column of a set operation that
    /// has the name `name`.
    fn tail_named(
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
    fn tail_mappings(&mut self, tail: &BranchTail) -> Vec<ColumnMapping> {
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
    fn tail_column(&mut self, scope: ScopeId, column: &Ident) -> Vec<ColumnOrigin> {
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

    fn scope_mappings(&mut self, scope: ScopeId) -> std::sync::Arc<[ColumnMapping]> {
        resolve_scope_mappings(scope, self.cx)
    }

    /// The mapping standing for an unknown element of a scope's output.
    fn unknown_slot(&mut self, scope: ScopeId, index: usize) -> Option<ColumnMapping> {
        let model = self.scope_model(scope);
        let Some(OutputElement::Unknown(segment)) = model.elements.get(index) else {
            return None;
        };
        if !matches!(
            self.cx.graph.scopes.output_plan(scope),
            OutputPlan::SetOperation { .. }
        ) {
            if !self.cx.enter(EvalKey::Slot { scope, index }) {
                return None;
            }
            let mapping = self.unknown_mapping(segment);
            self.cx.leave(EvalKey::Slot { scope, index });
            return mapping;
        }
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

    fn unknown_mapping(&mut self, segment: &UnknownSegment) -> Option<ColumnMapping> {
        match &segment.provider {
            UnknownProvider::Relation(table) => Some(wildcard_mapping(None, (**table).clone())),
            UnknownProvider::Scope { scope, index } => self.unknown_slot(*scope, *index),
            UnknownProvider::Opaque(
                reason @ (OpaqueReason::FieldPath(star)
                | OpaqueReason::Expression(star)
                | OpaqueReason::Ambiguous(star)),
            ) => Some(self.opaque_star(*star, *reason)),
            UnknownProvider::Opaque(OpaqueReason::UnknownWidth | OpaqueReason::Recursive) => None,
        }
    }

    /// A star the analysis cannot attribute to a relation: an ambiguous
    /// qualifier, a field path or an expression.
    fn opaque_star(&mut self, star: NodeId, reason: OpaqueReason) -> ColumnMapping {
        let RawNode::Star { base, env, .. } = &self.cx.graph.nodes[star] else {
            unreachable!("only star models contain opaque stars");
        };
        let star_name = || ColumnRef {
            table: None,
            column: Ident::unquoted("*"),
        };
        let sources = match (reason, resolve_star_target(base, *env, self.cx.graph)) {
            (OpaqueReason::Ambiguous(_), StarTarget::Ambiguous(candidates)) => {
                vec![ColumnOrigin::Unresolved(crate::Unresolved::Ambiguous {
                    column: Ident::unquoted("*"),
                    candidates,
                })]
            }
            (OpaqueReason::FieldPath(_), StarTarget::FieldPath { binding, path }) => {
                let mut sources = Vec::new();
                if let Some(field) = path.first()
                    && let Some(origin) =
                        resolve_captured_binding(field, binding, self.cx, &mut HashSet::new())
                {
                    sources.push(origin);
                }
                sources.push(ColumnOrigin::UnknownStar);
                sources
            }
            _ => {
                let mut sources = Vec::new();
                if let StarBase::Expr(dependencies) = base {
                    for &dependency in dependencies {
                        let (origins, _, _) =
                            collect_leaf_origins(dependency, self.cx, &mut HashSet::new());
                        sources.extend(origins);
                    }
                }
                if !sources.iter().any(ColumnOrigin::is_unknown_star) {
                    sources.push(ColumnOrigin::UnknownStar);
                }
                sources
            }
        };
        ColumnMapping {
            output_name: None,
            target: star_name(),
            sources,
            transform: TransformKind::Direct,
        }
    }

    fn virtual_column(
        &mut self,
        source: crate::graph::scope::VirtualSourceId,
        index: usize,
    ) -> Vec<ColumnOrigin> {
        let graph = self.cx.graph;
        let column = &graph.scopes.virtual_source(source).columns[index];
        let mut origins = Vec::new();
        let mut visited = HashSet::new();
        for &dependency in &column.dependencies {
            let (dependency_origins, _, _) =
                collect_leaf_origins(dependency, self.cx, &mut visited);
            origins.extend(dependency_origins);
        }
        if origins.is_empty()
            && matches!(column.state, VirtualColumnState::Unknown)
            && !column
                .dependencies
                .iter()
                .all(|&dependency| known_empty_dependency(dependency, graph))
        {
            origins.push(ColumnOrigin::Ambiguous {
                column: column.name.clone(),
                candidates: Vec::new(),
            });
        }
        origins
    }

    /// The wildcard origins a segment contributes to `column`.
    fn provider_sources(
        &mut self,
        provider: &UnknownProvider,
        column: &Ident,
    ) -> Vec<ColumnOrigin> {
        match provider {
            UnknownProvider::Relation(table) => vec![ColumnOrigin::NamedWildcard {
                table: (**table).clone(),
                column: column.clone(),
            }],
            UnknownProvider::Scope { scope, index } => {
                if !self.cx.enter(EvalKey::Slot {
                    scope: *scope,
                    index: *index,
                }) {
                    return Vec::new();
                }
                let model = self.scope_model(*scope);
                let mut sources = match model.elements.get(*index) {
                    Some(OutputElement::Unknown(segment)) => {
                        self.provider_sources(&segment.provider, column)
                    }
                    _ => Vec::new(),
                };
                // The right operand of a set operation contributes to every
                // position, though the model lists only the left one.
                if let OutputPlan::SetOperation { right, .. } =
                    self.cx.graph.scopes.output_plan(*scope)
                    && let Unlisted::Sources(more) = self.unlisted(*right, column)
                {
                    sources.extend(more);
                }
                self.cx.leave(EvalKey::Slot {
                    scope: *scope,
                    index: *index,
                });
                sources
            }
            UnknownProvider::Opaque(_) => Vec::new(),
        }
    }

    fn unlisted(&mut self, scope: ScopeId, name: &Ident) -> Unlisted {
        let graph = self.cx.graph;
        match graph.scopes.output_plan(scope) {
            OutputPlan::Projection => {
                let model = self.scope_model(scope);
                let mut admitted = false;
                let mut unknown_seen = false;
                let mut sources = Vec::new();
                for element in &model.elements {
                    let OutputElement::Unknown(segment) = element else {
                        continue;
                    };
                    unknown_seen = true;
                    if segment.domain.admits(name, graph.dialect) {
                        admitted = true;
                        sources.extend(self.provider_sources(&segment.provider, name));
                    }
                }
                if admitted {
                    if sources.is_empty() {
                        Unlisted::None
                    } else {
                        Unlisted::Sources(sources)
                    }
                } else if unknown_seen {
                    Unlisted::Denied
                } else {
                    Unlisted::None
                }
            }
            OutputPlan::Delegate(child) => self.unlisted(*child, name),
            // The left operand defines the names of a set operation, but
            // provenance is collected from both operands. An operand that
            // rules the name out stays an explicit uncertainty marker because
            // its unknown-width slot cannot be aligned safely.
            OutputPlan::SetOperation { left, right, .. } => {
                let mut sources = match self.unlisted(*left, name) {
                    Unlisted::Denied => return Unlisted::Denied,
                    Unlisted::Sources(sources) => sources,
                    Unlisted::None => Vec::new(),
                };
                match self.unlisted(*right, name) {
                    Unlisted::Sources(more) => sources.extend(more),
                    Unlisted::Denied => sources.push(ColumnOrigin::Ambiguous {
                        column: name.clone(),
                        candidates: Vec::new(),
                    }),
                    Unlisted::None => {}
                }
                if sources.is_empty() {
                    Unlisted::None
                } else {
                    Unlisted::Sources(sources)
                }
            }
        }
    }
}
