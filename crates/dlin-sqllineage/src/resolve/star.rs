//! Evaluation of star models into column mappings and name lookups.
//!
//! A star's model already records which columns it publishes, in which order
//! and under which names. This module only turns the lazy values of that model
//! into lineage: it never re-reads EXCLUDE/ILIKE/REPLACE/RENAME or join
//! merges.

mod slot_eval;

use std::collections::HashSet;
use std::rc::Rc;

use super::binding::{derive_transform, resolve_captured_binding};
use super::context::EvalContext;
use super::lineage::{collect_leaf_origins, collect_output_sources};
use super::mapping::{known_empty_dependency, merge_transform, wildcard_mapping};
use super::model::{
    Choice, ColumnSlot, InputLookup, ModelStore, OpaqueReason, OutputElement, OutputModel,
    OutputModels, OutputRef, RangeRef, UnknownProvider, ValueId,
};
use super::target::{StarTarget, resolve_star_target};
use slot_eval::cut_segment;

use crate::graph::node::{NodeId, RawNode, StarBase};
use crate::graph::scope::{
    Binding, BindingEnv, OutputPlan, ScopeId, SetCorrespondence, VirtualColumnState,
};
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

/// The mappings of a projection scope, one per element of its published
/// model: a listed column, an unknown segment (when it stands for columns at
/// all) or the columns a set operation merges in.
pub(super) fn projection_mappings(
    scope: ScopeId,
    model: &OutputModel,
    cx: &mut EvalContext<'_, '_>,
) -> Vec<ColumnMapping> {
    let mut evaluator = Evaluator { cx };
    let mut mappings = Vec::with_capacity(model.elements.len());
    for element in &model.elements {
        match element {
            OutputElement::Column(slot) => {
                let (sources, transform) = evaluator.projected_value(scope, slot);
                mappings.push(ColumnMapping {
                    star_position: crate::types::StarPosition::Inferred,
                    output_name: Some(slot.output_name.clone()),
                    ordinal: slot.ordinal,
                    target: ColumnRef {
                        table: None,
                        column: slot.name.clone(),
                    },
                    sources,
                    transform,
                    slot: Some(OutputRef::Slot(slot.published())),
                    unpublished_tail: false,
                    merged_in: false,
                });
            }
            OutputElement::Unknown(segment) => {
                let id = segment.published();
                mappings.extend(evaluator.segment_mapping(id).map(|mapping| ColumnMapping {
                    slot: Some(OutputRef::Segment(id)),
                    unpublished_tail: false,
                    merged_in: false,
                    ..mapping
                }));
            }
            OutputElement::Tail(tail) => {
                for mut mapping in evaluator.tail_mappings(tail) {
                    mapping.merged_in = false;
                    mapping.unpublished_tail = true;
                    mapping.star_position = mapping.star_position.shifted(tail.offset);
                    mapping.ordinal = mapping.ordinal.and_then(|ordinal| {
                        tail.offset.and_then(|offset| offset.checked_add(ordinal))
                    });
                    mappings.push(mapping);
                }
            }
        }
    }
    mappings
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

/// The id of the column at `index` of a published model, which the model's
/// name lookup returned.
fn listed_slot(model: &OutputModel, index: usize) -> super::model::SlotId {
    match &model.elements[index] {
        OutputElement::Column(slot) => slot.published(),
        _ => unreachable!("a name lookup returns the index of a listed column"),
    }
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
                let (sources, _, transform) = self.expression(*node_id);
                (sources, transform)
            }
            ValueId::Forward(id) => self.slot_value(*id),
            ValueId::SetMerged { op, left } => self.merged_slot_value(*op, *left),
            ValueId::SchemaDependent { value, relations } => {
                let (mut sources, transform) = value.as_deref().map_or_else(
                    || (Vec::new(), TransformKind::Direct),
                    |value| self.value(value),
                );
                sources.push(ColumnOrigin::Unresolved(Unresolved::SchemaDependent {
                    what: crate::SchemaDependencyKind::NaturalJoinColumns,
                    relations: relations.clone(),
                }));
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
            ValueId::Tail {
                scope,
                publication,
                column,
            } => (
                self.tail_column(*scope, *publication, column),
                TransformKind::Direct,
            ),
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

    /// The origins and transform of the expression of an output node, with
    /// whether it reads the recursive CTE it belongs to.
    fn expression(&mut self, node_id: NodeId) -> (Vec<ColumnOrigin>, bool, TransformKind) {
        let (sources, edge_kinds, has_back, inherited) =
            collect_output_sources(node_id, self.cx, &mut HashSet::new());
        let transform = merge_transform(
            &derive_transform(&self.cx.graph.nodes[node_id], &edge_kinds),
            &inherited,
        );
        (sources, has_back, transform)
    }

    /// The origins and transform of a column in the mappings of the
    /// projection `scope` that published it. A column the projection lists
    /// itself that reads its own recursive CTE is marked as recursive in the
    /// mappings, while a reader that names the column does not see the mark.
    fn projected_value(
        &mut self,
        scope: ScopeId,
        slot: &ColumnSlot,
    ) -> (Vec<ColumnOrigin>, TransformKind) {
        if let ValueId::Expr(node_id) = slot.value
            && self
                .cx
                .graph
                .scopes
                .output_columns(scope)
                .iter()
                .any(|column| column.node_id == node_id)
        {
            let (sources, has_back, transform) = self.expression(node_id);
            let sources = if has_back {
                vec![ColumnOrigin::Recursive {
                    base_sources: sources,
                }]
            } else {
                sources
            };
            return (sources, transform);
        }
        self.slot_value(slot.published())
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
            ValueId::Forward(id) => self.slot_read(*id),
            ValueId::SchemaDependent { value, relations } => {
                let (mut sources, has_back, transform) = value.as_deref().map_or_else(
                    || (Vec::new(), false, TransformKind::Direct),
                    |value| self.read_value(value, name),
                );
                sources.push(ColumnOrigin::Unresolved(Unresolved::SchemaDependent {
                    what: crate::SchemaDependencyKind::NaturalJoinColumns,
                    relations: relations.clone(),
                }));
                (sources, has_back, transform)
            }
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
                let mut by_name = false;
                for value in &values {
                    let (sources, _, listed) = self.read_value(value, name);
                    by_name |= has_by_name_reason(&sources);
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
                let mut sources = vec![origin];
                if by_name {
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported {
                        what: "BY NAME set operation",
                    }));
                }
                Some((sources, false, transform))
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
            Choice::Column(index, _) => {
                return Some(self.slot_read(listed_slot(&model, index)));
            }
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
                let mut by_name = model.unknown_reasons.contains(&"BY NAME set operation")
                    || model.matches(name).unknown.iter().any(|&index| {
                        matches!(&model.elements[index], OutputElement::Unknown(segment)
                            if segment.provider.has_by_name_reason(self.cx.graph, &self.cx.mapping_cache.models.arena))
                    });
                for &index in &ambiguity.listed {
                    let (sources, _, listed) = self.slot_read(listed_slot(&model, index));
                    by_name |= has_by_name_reason(&sources);
                    transform = merge_transform(&transform, &listed);
                }
                let ambiguous = ColumnOrigin::Unresolved(Unresolved::Ambiguous {
                    column: name.clone(),
                    candidates,
                });
                let mut sources = vec![ambiguous];
                if by_name {
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported {
                        what: "BY NAME set operation",
                    }));
                }
                return Some((sources, false, transform));
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

    /// The mapping a segment with this provider stands for, at `position`
    /// unless the provider knows a position of its own.
    fn provider_mapping(
        &mut self,
        provider: &UnknownProvider,
        position: crate::types::StarPosition,
    ) -> Option<ColumnMapping> {
        let mut mapping = match provider {
            UnknownProvider::Relation(table) => Some(wildcard_mapping(None, (**table).clone())),
            UnknownProvider::Segment { id, .. } => self.segment_mapping(*id),
            UnknownProvider::SetOperation { op, left } => {
                match self.merged_counterpart(*op, OutputRef::Segment(*left)) {
                    Some(OutputRef::Segment(merged)) => self.segment_mapping(merged),
                    _ => Some(cut_segment()),
                }
            }
            UnknownProvider::Opaque(
                reason @ (OpaqueReason::FieldPath(star)
                | OpaqueReason::Expression(star)
                | OpaqueReason::Ambiguous(star)),
            ) => Some(self.opaque_star(*star, *reason)),
            UnknownProvider::Opaque(OpaqueReason::Unsupported(what)) => {
                let mut mapping = cut_segment();
                mapping.sources = vec![ColumnOrigin::Unresolved(Unresolved::Unsupported { what })];
                Some(mapping)
            }
            UnknownProvider::Opaque(OpaqueReason::UnknownWidth | OpaqueReason::Recursive) => None,
        };
        if let Some(mapping) = &mut mapping
            && !matches!(position, crate::types::StarPosition::Inferred)
        {
            mapping.star_position = position;
        }
        mapping
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
                    && let Some(origin) = resolve_captured_binding(field, binding, self.cx)
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
            star_position: crate::types::StarPosition::Inferred,
            output_name: None,
            ordinal: None,
            target: star_name(),
            sources,
            transform: TransformKind::Direct,
            slot: None,
            unpublished_tail: false,
            merged_in: false,
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
            UnknownProvider::Segment { id, .. } => self.segment_sources(*id, column),
            UnknownProvider::SetOperation { op, left } => {
                match self.merged_counterpart(*op, crate::resolve::OutputRef::Segment(*left)) {
                    Some(crate::resolve::OutputRef::Segment(merged)) => self
                        .segment_mapping(merged)
                        .map_or_else(Vec::new, |mapping| {
                            named_wildcard_sources(mapping.sources, column)
                        }),
                    _ => vec![ColumnOrigin::Ambiguous {
                        column: column.clone(),
                        candidates: Vec::new(),
                    }],
                }
            }
            UnknownProvider::Opaque(OpaqueReason::Unsupported(what)) => {
                vec![ColumnOrigin::Unresolved(Unresolved::Unsupported { what })]
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
            OutputPlan::SetOperation {
                left,
                right,
                correspondence,
                ..
            } => {
                if *correspondence == SetCorrespondence::ByName {
                    let mut sources = match self.unlisted(*left, name) {
                        Unlisted::Sources(sources) => sources,
                        Unlisted::Denied | Unlisted::None => Vec::new(),
                    };
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported {
                        what: "BY NAME set operation",
                    }));
                    return Unlisted::Sources(sources);
                }
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

/// Give the wildcard sources in a merged segment the name the reader asked
/// for, keeping unresolved origins and recursive markers from the merge.
fn named_wildcard_sources(sources: Vec<ColumnOrigin>, column: &Ident) -> Vec<ColumnOrigin> {
    sources
        .into_iter()
        .map(|source| match source {
            ColumnOrigin::Wildcard { table } => ColumnOrigin::NamedWildcard {
                table,
                column: column.clone(),
            },
            ColumnOrigin::Recursive { base_sources } => ColumnOrigin::Recursive {
                base_sources: named_wildcard_sources(base_sources, column),
            },
            ColumnOrigin::UnknownStar => ColumnOrigin::Unresolved(crate::Unresolved::Unbound {
                column: column.clone(),
            }),
            source => source,
        })
        .collect()
}

/// Ambiguity discards possible value sources, but keeps the reason that a
/// candidate's set-operation correspondence could not be evaluated.
fn has_by_name_reason(sources: &[ColumnOrigin]) -> bool {
    sources.iter().any(|source| match source {
        ColumnOrigin::Unresolved(Unresolved::Unsupported {
            what: "BY NAME set operation",
        }) => true,
        ColumnOrigin::Recursive { base_sources } => has_by_name_reason(base_sources),
        _ => false,
    })
}
