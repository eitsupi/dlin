//! Evaluation of star models into column mappings and name lookups.
//!
//! A star's model already records which columns it publishes, in which order
//! and under which names. This module only turns the lazy values of that model
//! into lineage: it never re-reads EXCLUDE/ILIKE/REPLACE/RENAME or join
//! merges.

mod slot_eval;

use std::collections::HashSet;
use std::rc::Rc;

use super::binding::{derive_transform, resolve_captured_binding, unqualified_column_sources};
use super::context::{EvalContext, EvalKey};
use super::lineage::{collect_leaf_origins, collect_output_sources};
use super::mapping::{known_empty_dependency, merge_transform, wildcard_mapping};
use super::model::{
    Choice, ColumnSlot, FieldLookup, InputLookup, ModelStore, OpaqueReason, OutputElement,
    OutputModel, OutputModels, OutputRef, RangeRef, RowAliasTarget, SegmentId, UnknownProvider,
    ValueId,
};
use super::target::{StarTarget, resolve_star_target};
use slot_eval::cut_segment;

use crate::graph::node::{NodeId, RawNode, StarBase};
use crate::graph::scope::{Binding, BindingEnv, ScopeId, VirtualColumnState};
use crate::types::{
    ColumnMapping, ColumnOrigin, ColumnRef, Evidence, Origin, Sources, TransformKind,
};
use crate::{Candidate, Ident, Unresolved};

/// What a scope publishes under a name that none of its listed columns has.
pub(super) enum Unlisted {
    /// Nothing unlisted could carry the name.
    None,
    /// Unlisted columns exist, but the star modifiers rule the name out.
    Denied,
    /// Origins of the unlisted columns that may carry the name.
    Sources(Sources),
}

pub(super) fn named_segment_sources(
    id: SegmentId,
    column: &Ident,
    cx: &mut EvalContext<'_, '_>,
) -> Sources {
    Evaluator { cx }.segment_sources(id, column)
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
                });
            }
            OutputElement::Unknown(segment) => {
                let id = segment.published();
                mappings.extend(evaluator.segment_mapping(id).map(|mapping| ColumnMapping {
                    slot: Some(OutputRef::Segment(id)),
                    ..mapping
                }));
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
) -> Option<(Sources, bool, TransformKind)> {
    Evaluator { cx }.named(target_scope, name)
}

pub(super) fn field_lookup(reference: NodeId, cx: &mut EvalContext<'_, '_>) -> FieldLookup {
    Evaluator { cx }.models().field_value(reference)
}

pub(super) fn field_sources(
    reference: NodeId,
    value: &ValueId,
    evidence: &Evidence,
    cx: &mut EvalContext<'_, '_>,
) -> (Sources, bool, TransformKind) {
    let key = EvalKey::Node(reference);
    if !cx.enter(key) {
        let origin = ColumnOrigin::Unresolved(Unresolved::Unbound {
            column: cx.graph.field_paths[&reference].last().unwrap().clone(),
        });
        return (Sources::one(origin), false, TransformKind::Direct);
    }
    let mut evaluator = Evaluator { cx };
    let (sources, has_back, transform) = if let ValueId::Expr(node) = value {
        evaluator.expression(*node)
    } else {
        let (sources, transform) = evaluator.value(value);
        (sources, false, transform)
    };
    cx.leave(key);
    (sources.relying_on(evidence), has_back, transform)
}

/// What an unqualified name in the FROM clause at `env` denotes, with the
/// schemas the answer relies on.
pub(super) fn lookup_input(
    env: BindingEnv,
    name: &Ident,
    cx: &mut EvalContext<'_, '_>,
) -> (InputLookup, Evidence) {
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
) -> Option<(Sources, bool, TransformKind)> {
    let (lookup, evidence) = lookup_input(env, name, cx);
    Evaluator { cx }
        .input(lookup, name)
        .map(|(sources, has_back, transform)| (sources.relying_on(&evidence), has_back, transform))
}

/// Resolve the same bare-name binding used by STRUCT shape inference.
pub(super) fn row_alias_sources(
    env: BindingEnv,
    name: &Ident,
    binding: Option<&crate::graph::scope::Binding>,
    cx: &mut EvalContext<'_, '_>,
) -> Sources {
    let mut evaluator = Evaluator { cx };
    let (target, mut evidence) = evaluator.models().row_alias_target(env, name, binding);
    let row_value = || {
        Sources::one(ColumnOrigin::Unresolved(Unresolved::RowValue {
            alias: name.clone(),
        }))
    };
    let sources = match target {
        Some(RowAliasTarget::Column(column)) => {
            evaluator
                .input(column, name)
                .map_or_else(row_value, |(sources, has_back, _)| {
                    if has_back {
                        sources.recursive()
                    } else {
                        sources
                    }
                })
        }
        Some(RowAliasTarget::Row(binding)) => {
            if let Some((fields, shape_evidence)) =
                evaluator.models().row_range_fields(name, &binding)
            {
                evidence.extend(&shape_evidence);
                let mut sources = Sources::default();
                for field in fields {
                    if let OutputElement::Column(slot) = field {
                        sources.extend(evaluator.value(&slot.value).0);
                    }
                }
                sources
            } else {
                evidence.extend(&evaluator.models().binding_schema(&binding));
                row_value()
            }
        }
        None => row_value(),
    };
    sources.relying_on(&evidence)
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

/// A value that more columns of `relations` could change, relying on the
/// schemas that leave them open.
fn schema_dependent(relations: &[crate::Relation], evidence: &Evidence) -> Origin {
    Origin::new(
        ColumnOrigin::Unresolved(Unresolved::SchemaDependent {
            what: crate::SchemaDependencyKind::NaturalJoinColumns,
            relations: relations.to_vec(),
        }),
        evidence.clone(),
    )
}

/// A column of a physical relation the catalog lists, which relies on that
/// relation's schema.
fn physical(table: &crate::types::TableRef, column: &Ident) -> Origin {
    Origin::new(
        ColumnOrigin::Concrete {
            table: table.clone(),
            column: column.clone(),
        },
        Evidence::relation(table),
    )
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

    fn value(&mut self, value: &ValueId) -> (Sources, TransformKind) {
        match value {
            ValueId::Physical { table, column } => {
                (Sources::one(physical(table, column)), TransformKind::Direct)
            }
            ValueId::Expr(node_id) => {
                let (sources, _, transform) = self.expression(*node_id);
                (sources, transform)
            }
            ValueId::AggregateElement { field, index } => {
                let (mut sources, _) = self.value(field);
                let mut visited = HashSet::new();
                for &node in index.iter() {
                    let (index_sources, _, _) = collect_leaf_origins(node, self.cx, &mut visited);
                    sources.extend(index_sources);
                }
                (sources, TransformKind::Aggregation)
            }
            ValueId::Forward(id) => self.slot_value(*id),
            ValueId::SetMerged { op, left } => self.merged_slot_value(*op, *left),
            ValueId::SchemaDependent {
                value,
                relations,
                evidence,
            } => {
                let (mut sources, transform) = value.as_deref().map_or_else(
                    || (Sources::default(), TransformKind::Direct),
                    |value| self.value(value),
                );
                sources.push(schema_dependent(relations, evidence));
                (sources, transform)
            }
            // The same lookup a direct reference to the name in that scope makes.
            ValueId::ScopeName { scope, column } => named_scope_sources(column, *scope, self.cx)
                .map_or_else(
                    || {
                        (
                            Sources::one(ColumnOrigin::Ambiguous {
                                column: column.clone(),
                                candidates: Vec::new(),
                            }),
                            TransformKind::Direct,
                        )
                    },
                    |(sources, _, transform)| (sources, transform),
                ),
            ValueId::VirtualColumn { source, index } => {
                (self.virtual_column(*source, *index), TransformKind::Direct)
            }
            ValueId::Named { providers, column } => {
                let mut sources = Sources::default();
                for provider in providers {
                    sources.extend(self.provider_sources(provider, column));
                }
                (sources, TransformKind::Direct)
            }
            ValueId::Merged(left, right) => {
                let (mut sources, left_transform) = self.value(left);
                let (right_sources, right_transform) = self.value(right);
                sources.extend(right_sources);
                (sources, merge_transform(&left_transform, &right_transform))
            }
            ValueId::UnsupportedReasons { providers, column } => {
                let mut reasons = Vec::new();
                for provider in providers {
                    let sources = self.provider_sources(provider, column);
                    unsupported_reasons(&sources.origins, &mut reasons);
                }
                let mut sources = Sources::default();
                for what in reasons {
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }));
                }
                (sources, TransformKind::Direct)
            }
            ValueId::Uncertain {
                column,
                candidates,
                present,
            } => (
                Sources::one(ColumnOrigin::Unresolved(Unresolved::Unverified {
                    column: column.clone(),
                    present_candidates: candidates[..*present]
                        .iter()
                        .filter_map(RangeRef::candidate)
                        .collect(),
                    possible_candidates: candidates[*present..]
                        .iter()
                        .filter_map(RangeRef::candidate)
                        .collect(),
                })),
                TransformKind::Direct,
            ),
        }
    }

    /// The origins and transform of the expression of an output node, with
    /// whether it reads the recursive CTE it belongs to.
    fn expression(&mut self, node_id: NodeId) -> (Sources, bool, TransformKind) {
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
    fn projected_value(&mut self, scope: ScopeId, slot: &ColumnSlot) -> (Sources, TransformKind) {
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
                sources.recursive()
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
    fn read_value(&mut self, value: &ValueId, name: &Ident) -> (Sources, bool, TransformKind) {
        match value {
            ValueId::Physical { table, .. } => (
                Sources::one(physical(table, name)),
                false,
                TransformKind::Direct,
            ),
            ValueId::Forward(id) => self.slot_read(*id),
            ValueId::SchemaDependent {
                value,
                relations,
                evidence,
            } => {
                let (mut sources, has_back, transform) = value.as_deref().map_or_else(
                    || (Sources::default(), false, TransformKind::Direct),
                    |value| self.read_value(value, name),
                );
                sources.push(schema_dependent(relations, evidence));
                (sources, has_back, transform)
            }
            ValueId::Merged(left, right) => {
                let (mut sources, left_back, left_transform) = self.read_value(left, name);
                let (right_sources, right_back, right_transform) = self.read_value(right, name);
                sources.evidence.extend(&right_sources.evidence);
                for source in right_sources.origins {
                    match sources
                        .origins
                        .iter_mut()
                        .find(|seen| same_origin(&seen.kind, &source.kind))
                    {
                        Some(seen) => seen.evidence.extend(&source.evidence),
                        None => sources.origins.push(source),
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
    ) -> Option<(Sources, bool, TransformKind)> {
        let unbound = || ColumnOrigin::Ambiguous {
            column: name.clone(),
            candidates: Vec::new(),
        };
        match lookup {
            InputLookup::NoSource => None,
            InputLookup::Absent => Some((Sources::one(unbound()), false, TransformKind::Direct)),
            InputLookup::Physical(table) => Some((
                Sources::one(physical(&table, name)),
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
                let mut reasons = Vec::new();
                for value in &values {
                    let (sources, _, listed) = self.read_value(value, name);
                    unsupported_reasons(&sources.origins, &mut reasons);
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
                                    namespace: relation.namespace,
                                    table: relation.name,
                                }),
                                _ => None,
                            })
                            .collect(),
                    }
                };
                let mut sources = Sources::one(origin);
                for what in reasons {
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }));
                }
                Some((sources, false, transform))
            }
            InputLookup::Unverified {
                present,
                possible,
                values,
            } => {
                let mut transform = TransformKind::Direct;
                let mut reasons = Vec::new();
                for value in &values {
                    let (sources, _, listed) = self.read_value(value, name);
                    unsupported_reasons(&sources.origins, &mut reasons);
                    transform = merge_transform(&transform, &listed);
                }
                let mut sources = Sources::one(ColumnOrigin::Unresolved(Unresolved::Unverified {
                    column: name.clone(),
                    present_candidates: self.with_array_sources(present),
                    possible_candidates: self.with_array_sources(possible),
                }));
                for what in reasons {
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }));
                }
                Some((sources, false, transform))
            }
        }
    }

    /// Fill in the columns each array element candidate's array reads.
    fn with_array_sources(&mut self, mut candidates: Vec<Candidate>) -> Vec<Candidate> {
        for candidate in &mut candidates {
            let Candidate::ArrayElement(element) = candidate else {
                continue;
            };
            let Some(source) = element.source else {
                continue;
            };
            let graph = self.cx.graph;
            let dependencies = graph
                .scopes
                .virtual_source(source)
                .open_element
                .clone()
                .unwrap_or_default();
            for dependency in dependencies {
                let (origins, _, _) =
                    collect_leaf_origins(dependency, self.cx, &mut HashSet::new());
                for origin in origins.origins {
                    if let ColumnOrigin::Concrete { table, column } = origin.kind {
                        let catalog = self.cx.catalog;
                        element.array_sources.push(crate::SourceColumn {
                            relation: catalog
                                .and_then(|catalog| catalog.entry(&table))
                                .map_or_else(
                                    || super::analysis::plain_relation(&table),
                                    |entry| entry.relation.clone(),
                                ),
                            column: catalog
                                .and_then(|catalog| catalog.column(&table, &column))
                                .unwrap_or(column),
                        });
                    }
                }
            }
        }
        candidates
    }

    /// The origins of the column a CTE or derived table publishes under
    /// `name`: a listed column or whatever its unknown segments may publish.
    /// The answer relies on the schemas that decide what the scope
    /// publishes, whichever column it settles on.
    fn named(&mut self, scope: ScopeId, name: &Ident) -> Option<(Sources, bool, TransformKind)> {
        let model = self.scope_model(scope);
        self.named_in(scope, &model, name)
            .map(|(sources, has_back, transform)| {
                (sources.relying_on(&model.schema), has_back, transform)
            })
    }

    fn named_in(
        &mut self,
        scope: ScopeId,
        model: &Rc<OutputModel>,
        name: &Ident,
    ) -> Option<(Sources, bool, TransformKind)> {
        match model.choose(name) {
            Choice::Column(index, _) => {
                return Some(self.slot_read(listed_slot(model, index)));
            }
            Choice::Ambiguous(ambiguity) => {
                let unverified = model.unverified(&ambiguity);
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
                let mut reasons = Vec::new();
                if model.unknown_reasons.contains(&"BY NAME set operation") {
                    reasons.push("BY NAME set operation");
                }
                for &index in &model.matches(name).unknown {
                    if let OutputElement::Unknown(segment) = &model.elements[index] {
                        let sources = self.provider_sources(&segment.provider, name);
                        unsupported_reasons(&sources.origins, &mut reasons);
                    }
                }
                for &index in &ambiguity.listed {
                    let (sources, _, listed) = self.slot_read(listed_slot(model, index));
                    unsupported_reasons(&sources.origins, &mut reasons);
                    transform = merge_transform(&transform, &listed);
                }
                let origin = match unverified {
                    Some((mut present, possible)) => {
                        // A column the scope defines has no range variable;
                        // the CTE or derived table that is read stands for it.
                        if ambiguity.defined {
                            present.extend(self.cx.graph.scopes.reading_binding(scope).map(
                                |(name, binding)| match binding {
                                    Binding::Cte(_) => Candidate::Cte(name),
                                    _ => Candidate::Derived(Some(name)),
                                },
                            ));
                        }
                        Unresolved::Unverified {
                            column: name.clone(),
                            present_candidates: self.with_array_sources(present),
                            possible_candidates: self.with_array_sources(possible),
                        }
                    }
                    None => Unresolved::Ambiguous {
                        column: name.clone(),
                        candidates,
                    },
                };
                let mut sources = Sources::one(ColumnOrigin::Unresolved(origin));
                for what in reasons {
                    sources.push(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }));
                }
                return Some((sources, false, transform));
            }
            Choice::Possible(segment) => {
                let sources = self.segment_sources(segment.published(), name);
                if !sources.is_empty() {
                    return Some((sources, false, TransformKind::Direct));
                }
            }
            Choice::Absent => {}
        }
        match self.unlisted(scope, name) {
            Unlisted::Denied => Some((
                Sources::one(ColumnOrigin::Ambiguous {
                    column: name.clone(),
                    candidates: Vec::new(),
                }),
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
            UnknownProvider::Opaque(
                OpaqueReason::Unsupported(what) | OpaqueReason::UnmodelledSource(what),
            ) => {
                let mut mapping = cut_segment();
                mapping.sources =
                    Sources::one(ColumnOrigin::Unresolved(Unresolved::Unsupported { what }));
                Some(mapping)
            }
            UnknownProvider::Opaque(OpaqueReason::ArrayElement) => {
                let mut mapping = cut_segment();
                mapping.sources = Sources::one(ColumnOrigin::Unresolved(Unresolved::Unsupported {
                    what: "array element",
                }));
                Some(mapping)
            }
            UnknownProvider::Opaque(OpaqueReason::Recursive) => None,
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
                Sources::one(ColumnOrigin::Unresolved(crate::Unresolved::Ambiguous {
                    column: Ident::unquoted("*"),
                    candidates,
                }))
            }
            (OpaqueReason::FieldPath(_), StarTarget::FieldPath { binding, path }) => {
                let mut sources = Sources::default();
                if let Some(field) = path.first() {
                    let field = resolve_captured_binding(field, binding, self.cx);
                    // A field path reads one column of the relation.
                    let evidence = field.all_evidence();
                    sources.origins.extend(field.origins.into_iter().take(1));
                    sources.evidence.extend(&evidence);
                }
                sources.push(ColumnOrigin::UnknownStar);
                sources
            }
            (OpaqueReason::FieldPath(_), StarTarget::ColumnField { path }) => {
                let mut sources = Sources::default();
                if let Some(field) = path.first() {
                    sources.extend(unqualified_column_sources(field, *env, self.cx));
                }
                sources.push(ColumnOrigin::UnknownStar);
                sources
            }
            _ => {
                let mut sources = Sources::default();
                if let StarBase::Expr(dependencies) = base {
                    for &dependency in dependencies {
                        let (origins, _, _) =
                            collect_leaf_origins(dependency, self.cx, &mut HashSet::new());
                        sources.extend(origins);
                    }
                }
                if !sources
                    .origins
                    .iter()
                    .any(|origin| origin.kind.is_unknown_star())
                {
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
        }
    }

    fn virtual_column(
        &mut self,
        source: crate::graph::scope::VirtualSourceId,
        index: usize,
    ) -> Sources {
        let graph = self.cx.graph;
        let column = &graph.scopes.virtual_source(source).columns[index];
        let mut origins = Sources::default();
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
    fn provider_sources(&mut self, provider: &UnknownProvider, column: &Ident) -> Sources {
        match provider {
            UnknownProvider::Relation(table) => Sources::one(Origin::new(
                ColumnOrigin::NamedWildcard {
                    table: (**table).clone(),
                    column: column.clone(),
                },
                Evidence::relation(table),
            )),
            UnknownProvider::Segment { id, .. } => self.segment_sources(*id, column),
            UnknownProvider::SetOperation { op, left } => {
                super::set_operation::set_operation_segment_sources(*op, *left, column, self.cx)
            }
            UnknownProvider::Opaque(
                OpaqueReason::Unsupported(what) | OpaqueReason::UnmodelledSource(what),
            ) => Sources::one(ColumnOrigin::Unresolved(Unresolved::Unsupported { what })),
            UnknownProvider::Opaque(OpaqueReason::ArrayElement) => {
                Sources::one(ColumnOrigin::Unresolved(Unresolved::Unsupported {
                    what: "array element",
                }))
            }
            // What an opaque segment provides is not known: a value read
            // through it is unresolved, never source-free.
            UnknownProvider::Opaque(OpaqueReason::Recursive) => {
                Sources::one(ColumnOrigin::Unresolved(Unresolved::Recursive))
            }
            UnknownProvider::Opaque(
                OpaqueReason::FieldPath(_)
                | OpaqueReason::Expression(_)
                | OpaqueReason::Ambiguous(_),
            ) => Sources::one(ColumnOrigin::UnknownStar),
        }
    }

    fn unlisted(&mut self, scope: ScopeId, name: &Ident) -> Unlisted {
        let model = self.scope_model(scope);
        let mut admitted = false;
        let mut unknown_seen = false;
        let mut sources = Sources::default();
        for element in &model.elements {
            let OutputElement::Unknown(segment) = element else {
                continue;
            };
            unknown_seen = true;
            if segment.domain.admits(name, self.cx.graph.dialect) {
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
}

/// Ambiguity discards possible value sources, but keeps the reasons that a
/// candidate's value could not be evaluated: a set-operation correspondence
/// or a source the analysis does not model. Adds each to `reasons` once.
pub(super) fn unsupported_reasons(sources: &[Origin], reasons: &mut Vec<&'static str>) {
    for source in sources {
        match &source.kind {
            ColumnOrigin::Unresolved(Unresolved::Unsupported { what }) => {
                if !reasons.contains(what) {
                    reasons.push(what);
                }
            }
            ColumnOrigin::Recursive { base_sources } => {
                unsupported_reasons(base_sources, reasons);
            }
            _ => {}
        }
    }
}
