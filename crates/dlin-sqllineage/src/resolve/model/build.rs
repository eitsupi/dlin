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
    AlignedSegment, AlignedSlot, ColumnSlot, Forwarding, NameDomain, OpaqueReason, OutputElement,
    OutputModel, OutputRef, PositionRange, RangeRef, SetOperationModel, SlotMatch, SlotMatchKind,
    SlotOrigin, UnknownProvider, UnknownSegment, ValueId,
};
use crate::OutputName;
use crate::catalog::{CatalogContext, CatalogUnknownSegment, SchemaCoverage};
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode, StarBase, StarOptions};
use crate::graph::scope::{Binding, BindingEnv, OutputPlan, ScopeId, ScopeKind, SetCorrespondence};
use crate::types::{Dialect, Evidence, TableRef};

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
        let mut skip_forward_check = false;
        let mut set_operation_source = None;
        let mut schema = Evidence::default();
        let mut reasons = graph.scopes.unknown_output_reasons(scope);
        let mut elements = match graph.scopes.output_plan(scope) {
            OutputPlan::Projection => {
                let mut elements = Vec::new();
                let mut next_ordinal = Some(0usize);
                let mut minimum_width = 0usize;
                // The widths of the stars before a column decide its position.
                let mut prefix = Evidence::default();
                for column in graph.scopes.output_columns(scope) {
                    if matches!(graph.nodes[column.node_id], RawNode::Star { .. }) {
                        let star = self.star(column.node_id);
                        let width = star.width.exact();
                        let base_ordinal = next_ordinal;
                        for element in &star.elements {
                            let mut element = element.clone();
                            element.rely_on(&prefix);
                            match &mut element {
                                OutputElement::Column(slot) => {
                                    slot.ordinal = slot.ordinal.and_then(|ordinal| {
                                        base_ordinal.and_then(|base| base.checked_add(ordinal))
                                    });
                                    slot.position =
                                        slot.position.shifted(minimum_width, base_ordinal);
                                }
                                OutputElement::Unknown(segment) => {
                                    segment.position = segment.position.shifted(base_ordinal);
                                    segment.position_range =
                                        segment.position_range.shifted(minimum_width, base_ordinal);
                                }
                            }
                            elements.push(element);
                        }
                        minimum_width =
                            minimum_width.saturating_add(super::minimum_width(&star.elements));
                        next_ordinal = next_ordinal
                            .and_then(|ordinal| width.and_then(|width| ordinal.checked_add(width)));
                        invalid.extend(star.invalid.iter().cloned());
                        reads.extend(star.reads.iter().cloned());
                        prefix.extend(&star.schema);
                        schema.extend(&star.schema);
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
                            position: next_ordinal.map_or(
                                PositionRange {
                                    min: minimum_width,
                                    max: None,
                                },
                                PositionRange::exact,
                            ),
                            ranges: Rc::from([]),
                            candidate_ranges: Rc::from([]),
                            value: ValueId::Expr(column.node_id),
                            origin: SlotOrigin::Defined,
                            structure: prefix.clone(),
                        }));
                        next_ordinal = next_ordinal.and_then(|ordinal| ordinal.checked_add(1));
                        minimum_width = minimum_width.saturating_add(1);
                    }
                }
                elements
            }
            OutputPlan::Delegate(child) => {
                let model = self.scope(*child);
                invalid.push(Invalidity::Whole(model.clone()));
                let elements = forward_scope(*child, &model, &Rc::from([]), None);
                schema.extend(&model.schema);
                source = Some((*child, model));
                elements
            }
            // The left operand defines the names of a set operation.
            OutputPlan::SetOperation {
                left,
                right,
                recursive,
                operator,
                quantifier,
                correspondence,
                strict_by_name,
            } => {
                let model = self.scope(*left);
                invalid.push(Invalidity::Whole(model.clone()));
                let elements = forward_scope(*left, &model, &Rc::from([]), Some(scope));
                let right_model = (!recursive).then(|| self.scope(*right));
                // The operands' widths decide whether the operation is valid
                // and which outputs correspond.
                schema.extend(&model.schema);
                if let Some(right) = &right_model {
                    schema.extend(&right.schema);
                }
                // Names correspond only in a strict form whose operands list
                // the same names; otherwise which output a right column
                // feeds, and whether it adds one, is not modelled.
                let by_name_aligned = *correspondence == SetCorrespondence::ByName
                    && *strict_by_name
                    && right_model
                        .as_deref()
                        .is_some_and(|right| same_listed_names(&model, right, graph.dialect));
                let mut elements = elements;
                if *correspondence == SetCorrespondence::ByName && !by_name_aligned {
                    reasons.push("BY NAME set operation");
                    // No right-operand names are enumerated until name-based
                    // correspondence is implemented. Keep the unknown remainder
                    // separate from the left operand's listed names.
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
                }
                set_operation_source = Some((
                    crate::SetOperationKind {
                        operator: *operator,
                        quantifier: *quantifier,
                        correspondence: *correspondence,
                    },
                    *recursive,
                    by_name_aligned,
                    model.clone(),
                    right_model,
                ));
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
            let reason = graph
                .scopes
                .unknown_output_reasons(scope)
                .first()
                .copied()
                .unwrap_or("unknown output width");
            elements.push(OutputElement::Unknown(UnknownSegment::opaque(
                OpaqueReason::UnmodelledSource(reason),
            )));
        }
        if let Some(aliases) = graph.scopes.output_aliases(scope)
            && !aliases.is_empty()
        {
            let width_known = !graph.scopes.has_unknown_output_width(scope)
                && elements
                    .iter()
                    .all(|element| matches!(element, OutputElement::Column(_)));
            let cte = graph.scopes.kind(scope) == ScopeKind::Cte;
            match apply_output_aliases(&mut elements, aliases, graph.dialect, cte, width_known) {
                AliasApplication::Applied {
                    invalidated_forwarding,
                } => {
                    if invalidated_forwarding {
                        skip_forward_check = true;
                    }
                }
                AliasApplication::Unsupported(reason) => {
                    elements = vec![OutputElement::Unknown(opaque_alias_segment(
                        aliases.to_vec(),
                        reason,
                        &schema,
                        PositionRange { min: 0, max: None },
                    ))];
                    skip_forward_check = true;
                }
            }
        }
        let forwarded = self.publish(scope, publication, &mut elements);
        let mut model = OutputModel::new(elements, reasons, graph.dialect);
        model.invalid = invalid;
        model.reads = reads;
        model.schema = schema;
        model.publication = Some(publication);
        model.set_operation =
            set_operation_source.map(|(kind, recursive, by_name_aligned, left, right)| {
                build_set_operation_model(kind, recursive, by_name_aligned, &left, right.as_deref())
            });
        if let Some((source_scope, source)) = source {
            model.forwarded = forwarded;
            if !skip_forward_check
                && let Err(problem) =
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
                        structure: slot.structure.clone(),
                    }));
                }
                OutputElement::Unknown(segment) => {
                    segment.id = Some(self.store.arena.add_segment(SegmentDef {
                        owner: scope,
                        provider: segment.provider.clone(),
                        position: segment.position,
                        publication,
                        structure: segment.structure.clone(),
                    }));
                }
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
        let mut schema = Evidence::default();
        let elements = match resolve_star_target(base, env, graph) {
            StarTarget::All => {
                let input = self.input(env);
                invalid.extend(input.model.invalid.iter().cloned());
                reads.extend(input.model.reads.iter().cloned());
                schema.extend(&input.model.schema);
                input.star_elements()
            }
            StarTarget::Binding { name, binding } => {
                let range = RangeRef {
                    index: self.range_index(env, &name, &binding),
                    name,
                    binding,
                    anonymous: false,
                };
                if let Some(model) = self.binding_model(&range.binding) {
                    reads.push((range.index, model));
                }
                schema.extend(&self.binding_schema(&range.binding));
                self.binding_part(&range)
            }
            StarTarget::Unknown(table) => {
                schema.add(&table);
                self.relation_part(&Rc::new(table), &Rc::from([]))
            }
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
        model.schema = schema;
        model
    }

    /// The schemas that decide what a binding publishes: a physical
    /// relation's own, or those of the CTE or derived table it reads.
    pub(super) fn binding_schema(&mut self, binding: &Binding) -> Evidence {
        match binding {
            Binding::Table(table) => Evidence::relation(table),
            Binding::Cte(child) | Binding::DerivedTable(child) => self.scope(*child).schema.clone(),
            Binding::VirtualSource(_) => Evidence::default(),
        }
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
                        position: PositionRange::exact(index),
                        ranges: ranges.clone(),
                        candidate_ranges: ranges.clone(),
                        value: ValueId::VirtualColumn {
                            source: *source,
                            index,
                        },
                        origin: SlotOrigin::Forwarded(column.name.clone()),
                        structure: Evidence::default(),
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
                position_range: PositionRange { min: 0, max: None },
                provider: UnknownProvider::Relation(table.clone()),
                ranges: ranges.clone(),
                domain: NameDomain::default(),
                structure: Evidence::relation(table),
            })];
        };

        let mut columns: Vec<_> = entry.columns.iter().cloned().collect();
        // Individually proven positions also order the known part of a
        // partial schema; an unknown full order does not revoke those proofs.
        columns.sort_by_key(|column| column.ordinal.unwrap_or(usize::MAX));
        let listed_names: Vec<_> = columns.iter().map(|column| column.name.clone()).collect();
        let exact_width = (entry.coverage == SchemaCoverage::Exhaustive).then_some(columns.len());
        let interval_end = |segment: CatalogUnknownSegment| {
            segment.after_ordinal.and_then(|after| {
                columns
                    .iter()
                    .filter_map(|column| column.ordinal)
                    .filter(|ordinal| *ordinal > after)
                    .min()
                    .and_then(|next| next.checked_sub(1))
            })
        };
        let mut elements = Vec::new();
        for column in &columns {
            elements.push(OutputElement::Column(ColumnSlot {
                id: None,
                name: column.name.clone(),
                output_name: OutputName::Explicit(column.name.clone()),
                ordinal: column.ordinal,
                position: column.ordinal.map_or_else(
                    || PositionRange {
                        min: 0,
                        max: exact_width.and_then(|width| width.checked_sub(1)),
                    },
                    PositionRange::exact,
                ),
                ranges: ranges.clone(),
                candidate_ranges: ranges.clone(),
                value: ValueId::Physical {
                    table: table.clone(),
                    column: column.name.clone(),
                },
                origin: SlotOrigin::Forwarded(column.name.clone()),
                structure: Evidence::relation(table),
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
                        interval_end(*segment),
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
                relation_unknown(
                    table,
                    ranges,
                    &listed_names,
                    segment,
                    interval_end(segment),
                    &entry.relation,
                )
            }));
        }
        elements
    }
}

enum AliasApplication {
    Applied { invalidated_forwarding: bool },
    Unsupported(&'static str),
}

/// Apply range-variable aliases to output positions. A complete prefix can be
/// renamed directly; when an output's position is uncertain, publish an
/// opaque segment for the names it may have instead of guessing its value.
fn apply_output_aliases(
    elements: &mut Vec<OutputElement>,
    aliases: &[crate::Ident],
    dialect: Dialect,
    is_cte: bool,
    width_known: bool,
) -> AliasApplication {
    if dialect == Dialect::BigQuery {
        return AliasApplication::Unsupported("column aliases unsupported by dialect");
    }

    let prefix_allowed = matches!(
        dialect,
        Dialect::Generic | Dialect::PostgreSql | Dialect::DuckDb | Dialect::Snowflake
    );
    let mut aliases = aliases;
    if width_known && aliases.len() > elements.len() {
        if dialect == Dialect::DuckDb && is_cte {
            aliases = &aliases[..elements.len()];
        } else {
            return AliasApplication::Unsupported("column alias count mismatch");
        }
    }
    if width_known && !prefix_allowed && aliases.len() != elements.len() {
        return AliasApplication::Unsupported("column alias count mismatch");
    }
    if !width_known && !prefix_allowed {
        return AliasApplication::Unsupported("column alias output shape unknown");
    }

    let mut renamed = Vec::with_capacity(elements.len());
    // A wide unknown segment can conservatively overlap positions that are
    // already occupied by explicitly enumerated outputs. Those positions
    // cannot also contribute a name from the alias list.
    let occupied_positions: Vec<_> = elements
        .iter()
        .filter_map(|element| match element {
            OutputElement::Column(slot) => slot.ordinal.or_else(|| {
                (slot.position.max == Some(slot.position.min)).then_some(slot.position.min)
            }),
            OutputElement::Unknown(_) => None,
        })
        .collect();
    let mut invalidated_forwarding = false;
    for element in std::mem::take(elements) {
        match element {
            OutputElement::Column(mut slot) => {
                let exact_position = slot.ordinal.or_else(|| {
                    (slot.position.max == Some(slot.position.min)).then_some(slot.position.min)
                });
                if let Some(position) = exact_position {
                    if let Some(alias) = aliases.get(position) {
                        slot.name = alias.clone();
                        slot.output_name = OutputName::Explicit(alias.clone());
                    }
                    renamed.push(OutputElement::Column(slot));
                } else if slot.position.min >= aliases.len() {
                    renamed.push(OutputElement::Column(slot));
                } else {
                    let mut possible_names = aliases_in_range(aliases, slot.position);
                    let may_be_after_prefix =
                        slot.position.max.is_none_or(|end| end >= aliases.len());
                    if may_be_after_prefix {
                        possible_names.push(slot.name.clone());
                    }
                    renamed.push(OutputElement::Unknown(opaque_alias_segment(
                        possible_names,
                        "column alias position unknown",
                        &slot.structure,
                        slot.position,
                    )));
                    invalidated_forwarding = true;
                }
            }
            OutputElement::Unknown(mut segment) => {
                if segment.position_range.min >= aliases.len() {
                    renamed.push(OutputElement::Unknown(segment));
                    continue;
                }

                let possible_aliases = aliases
                    .iter()
                    .enumerate()
                    .filter(|(position, _)| {
                        *position >= segment.position_range.min
                            && segment
                                .position_range
                                .max
                                .is_none_or(|end| *position <= end)
                            && !occupied_positions.contains(position)
                    })
                    .map(|(_, alias)| alias.clone())
                    .collect::<Vec<_>>();
                if possible_aliases.is_empty() {
                    renamed.push(OutputElement::Unknown(segment));
                } else {
                    // A source the analysis does not model stays the reason
                    // for both the renamed and the remaining columns.
                    let unmodelled = match segment.provider {
                        UnknownProvider::Opaque(reason @ OpaqueReason::UnmodelledSource(_)) => {
                            Some(reason)
                        }
                        _ => None,
                    };
                    if unmodelled.is_none() {
                        segment.provider = UnknownProvider::Opaque(OpaqueReason::Unsupported(
                            "column alias position unknown",
                        ));
                    }
                    let alias_names = possible_aliases;
                    let alias_range = segment.position_range;
                    let mut alias_segment = opaque_alias_segment(
                        alias_names,
                        "column alias position unknown",
                        &segment.structure,
                        alias_range,
                    );
                    if let Some(reason) = unmodelled {
                        alias_segment.provider = UnknownProvider::Opaque(reason);
                    }
                    renamed.push(OutputElement::Unknown(segment));
                    renamed.push(OutputElement::Unknown(alias_segment));
                    invalidated_forwarding = true;
                }
            }
        }
    }
    *elements = renamed;
    AliasApplication::Applied {
        invalidated_forwarding,
    }
}

fn aliases_in_range(aliases: &[crate::Ident], range: PositionRange) -> Vec<crate::Ident> {
    aliases
        .iter()
        .enumerate()
        .filter(|(position, _)| {
            *position >= range.min && range.max.is_none_or(|end| *position <= end)
        })
        .map(|(_, alias)| alias.clone())
        .collect()
}

fn opaque_alias_segment(
    aliases: Vec<crate::Ident>,
    reason: &'static str,
    evidence: &Evidence,
    position_range: PositionRange,
) -> UnknownSegment {
    let mut segment = UnknownSegment::opaque(OpaqueReason::Unsupported(reason));
    segment.domain.allowed = Some(aliases);
    segment.position = crate::types::StarPosition::Unproven;
    segment.position_range = position_range;
    segment.schema_relations = evidence
        .relations()
        .map(|table| crate::Relation {
            catalog: table.catalog.clone(),
            schema: table.schema.clone(),
            name: table.table.clone(),
        })
        .collect();
    segment.structure.extend(evidence);
    segment
}

fn relation_unknown(
    table: &Rc<TableRef>,
    ranges: &Rc<[RangeRef]>,
    listed_names: &[crate::Ident],
    segment: CatalogUnknownSegment,
    interval_end: Option<usize>,
    canonical_relation: &crate::Relation,
) -> OutputElement {
    OutputElement::Unknown(UnknownSegment {
        id: None,
        schema_relations: vec![canonical_relation.clone()],
        position: segment.after_ordinal.map_or(
            crate::types::StarPosition::Unproven,
            crate::types::StarPosition::After,
        ),
        position_range: segment.after_ordinal.map_or(
            PositionRange { min: 0, max: None },
            |after| PositionRange {
                min: after.saturating_add(1),
                max: interval_end,
            },
        ),
        provider: UnknownProvider::Relation(table.clone()),
        ranges: ranges.clone(),
        domain: NameDomain {
            filters: Vec::new(),
            allowed: None,
            removed: listed_names.to_vec(),
        },
        structure: Evidence::relation(table),
    })
}

/// Whether two operands list the same names, each once, with no unknown
/// segment that could publish another: the condition under which a `BY NAME`
/// operation pairs every output with exactly one column of each operand.
fn same_listed_names(left: &OutputModel, right: &OutputModel, dialect: Dialect) -> bool {
    let names = |model: &OutputModel| {
        let mut keys = Vec::with_capacity(model.elements.len());
        for element in &model.elements {
            let OutputElement::Column(slot) = element else {
                return None;
            };
            keys.push(by_name_key(&slot.name, dialect));
        }
        let count = keys.len();
        keys.sort();
        keys.dedup();
        (keys.len() == count).then_some(keys)
    };
    matches!((names(left), names(right)), (Some(left), Some(right)) if left == right)
}

/// The key two names of a `BY NAME` operation are matched by. `DuckDB` folds
/// only ASCII letters, quoted or not.
fn by_name_key(name: &crate::Ident, dialect: Dialect) -> String {
    match dialect {
        Dialect::DuckDb => name.value.to_ascii_lowercase(),
        _ => name.column_key(dialect).into_owned(),
    }
}

fn build_set_operation_model(
    kind: crate::SetOperationKind,
    recursive: bool,
    by_name_aligned: bool,
    left: &OutputModel,
    right: Option<&OutputModel>,
) -> SetOperationModel {
    let mut slots = Vec::new();
    let mut segments = Vec::new();
    let left_exact_positions: Vec<_> = left
        .elements
        .iter()
        .filter_map(|element| match element {
            OutputElement::Column(slot) => slot.ordinal,
            OutputElement::Unknown(_) => None,
        })
        .collect();
    let right_exact_positions: Vec<_> = right
        .into_iter()
        .flat_map(|model| &model.elements)
        .filter_map(|element| match element {
            OutputElement::Column(slot) => slot.ordinal,
            OutputElement::Unknown(_) => None,
        })
        .collect();
    for element in &left.elements {
        match element {
            OutputElement::Column(left_slot) => {
                let mut matches = Vec::new();
                if by_name_aligned && let Some(right) = right {
                    let key = by_name_key(&left_slot.name, right.names.dialect);
                    matches.extend(right.elements.iter().filter_map(|element| match element {
                        OutputElement::Column(right_slot)
                            if by_name_key(&right_slot.name, right.names.dialect) == key =>
                        {
                            Some(SlotMatch {
                                operand: crate::SetOperand::Right,
                                output: OutputRef::Slot(right_slot.published()),
                                kind: SlotMatchKind::Proven,
                            })
                        }
                        _ => None,
                    }));
                }
                if let Some(right) = right
                    && kind.correspondence == SetCorrespondence::Positional
                    && !recursive
                {
                    let has_exact_match = left_slot.ordinal.is_some_and(|ordinal| {
                        right.elements.iter().any(|element| {
                            matches!(element, OutputElement::Column(slot)
                                if slot.ordinal == Some(ordinal))
                        })
                    });
                    for right_element in &right.elements {
                        match right_element {
                            OutputElement::Column(right_slot)
                                if left_slot.position.overlaps(right_slot.position) =>
                            {
                                if has_exact_match && right_slot.ordinal != left_slot.ordinal {
                                    continue;
                                }
                                if right_slot.ordinal.is_some_and(|ordinal| {
                                    left_slot.ordinal != Some(ordinal)
                                        && left_exact_positions.contains(&ordinal)
                                }) {
                                    continue;
                                }
                                matches.push(SlotMatch {
                                    operand: crate::SetOperand::Right,
                                    output: OutputRef::Slot(right_slot.published()),
                                    kind: if left_slot.ordinal.is_some()
                                        && left_slot.ordinal == right_slot.ordinal
                                    {
                                        SlotMatchKind::Proven
                                    } else {
                                        SlotMatchKind::Possible
                                    },
                                });
                            }
                            OutputElement::Unknown(right_segment)
                                if left_slot.position.overlaps(right_segment.position_range) =>
                            {
                                if left_slot
                                    .ordinal
                                    .is_some_and(|ordinal| right_exact_positions.contains(&ordinal))
                                {
                                    continue;
                                }
                                matches.push(SlotMatch {
                                    operand: crate::SetOperand::Right,
                                    output: OutputRef::Segment(right_segment.published()),
                                    kind: SlotMatchKind::Possible,
                                });
                            }
                            _ => {}
                        }
                    }
                }
                slots.push(AlignedSlot {
                    left: left_slot.published(),
                    matches,
                });
            }
            OutputElement::Unknown(left_segment) => {
                let mut matches = Vec::new();
                if let Some(right) = right
                    && kind.correspondence == SetCorrespondence::Positional
                    && !recursive
                {
                    for right_element in &right.elements {
                        let output = match right_element {
                            OutputElement::Column(slot)
                                if left_segment.position_range.overlaps(slot.position)
                                    && !left_exact_positions
                                        .contains(&slot.ordinal.unwrap_or(usize::MAX)) =>
                            {
                                Some(OutputRef::Slot(slot.published()))
                            }
                            OutputElement::Unknown(segment)
                                if left_segment.position_range.overlaps(segment.position_range) =>
                            {
                                Some(OutputRef::Segment(segment.published()))
                            }
                            _ => None,
                        };
                        if let Some(output) = output {
                            matches.push(SlotMatch {
                                operand: crate::SetOperand::Right,
                                output,
                                kind: SlotMatchKind::Possible,
                            });
                        }
                    }
                }
                segments.push(AlignedSegment {
                    left: left_segment.published(),
                    matches,
                });
            }
        }
    }
    SetOperationModel {
        kind,
        recursive,
        slots,
        segments,
    }
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
                    position: slot.position,
                    ranges: ranges.clone(),
                    candidate_ranges: ranges.clone(),
                    value: match merged_by {
                        Some(op) => ValueId::SetMerged { op, left: id },
                        None => ValueId::Forward(id),
                    },
                    origin: SlotOrigin::Forwarded(slot.name.clone()),
                    structure: slot.structure.clone(),
                })
            }
            OutputElement::Unknown(segment) => {
                let id = segment.published();
                OutputElement::Unknown(UnknownSegment {
                    id: None,
                    schema_relations: segment.schema_relations.clone(),
                    position: segment.position,
                    position_range: segment.position_range,
                    provider: match merged_by {
                        Some(op) => UnknownProvider::SetOperation { op, left: id },
                        None => UnknownProvider::Segment { scope, id },
                    },
                    ranges: ranges.clone(),
                    domain: segment.domain.clone(),
                    structure: segment.structure.clone(),
                })
            }
        })
        .collect()
}
