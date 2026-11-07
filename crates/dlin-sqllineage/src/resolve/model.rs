//! Structural description of what a star or a scope outputs.
//!
//! A model is an ordered list of elements: columns whose names are known and
//! segments of columns the analysis cannot enumerate. USING/NATURAL folding
//! and the EXCLUDE/ILIKE/REPLACE/RENAME modifiers are applied once while a
//! star's model is built, so widths and names are read from one result instead
//! of being recomputed by every consumer. Values are lazy references and never
//! carry lineage: building a model does not resolve any column origin.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::target::{
    StarTarget, effective_bindings, excluded_matches_name, ilike_matches, resolve_star_target,
    same_column_name,
};

use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode, StarBase, StarColumnName, StarOptions};
use crate::graph::scope::{
    Binding, JoinMerge, JoinMergeKind, MergedColumns, OutputPlan, ScopeId, VirtualSourceId,
};
use crate::types::TableRef;
use crate::{Dialect, Ident, OutputName};

/// The number of columns an output has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Width {
    Exact(usize),
    Unknown,
}

impl Width {
    pub(super) fn exact(self) -> Option<usize> {
        match self {
            Self::Exact(width) => Some(width),
            Self::Unknown => None,
        }
    }
}

/// A lazy reference to the value an output column takes. It is evaluated
/// only when lineage is needed, which keeps model construction free of
/// column resolution and of cycles between widths, mappings and names.
#[derive(Debug, Clone)]
pub(super) enum ValueId {
    /// A column the catalog lists for a physical relation.
    Physical { table: Rc<TableRef>, column: Ident },
    /// The expression of an output column or a REPLACE item.
    Expr(NodeId),
    /// A slot of a CTE or derived table.
    ScopeSlot { scope: ScopeId, index: usize },
    /// A named column of a CTE or derived table that has no slot of its own.
    ScopeName { scope: ScopeId, column: Ident },
    /// A slot of a set operation, which combines the operands positionally.
    SetOperation { scope: ScopeId, index: usize },
    /// A column of a virtual source such as a lateral expansion.
    VirtualColumn {
        source: VirtualSourceId,
        index: usize,
    },
    /// A column that is named but sits behind segments the analysis cannot
    /// enumerate.
    Named {
        providers: Vec<UnknownProvider>,
        column: Ident,
    },
    /// A column merged by USING/NATURAL whose value both sides contribute to.
    Merged(Box<ValueId>, Box<ValueId>),
}

/// Why a segment of columns cannot be enumerated.
#[derive(Debug, Clone)]
pub(super) enum UnknownProvider {
    /// A physical relation without catalog columns.
    Relation(Rc<TableRef>),
    /// An unknown element of a CTE or derived table.
    Scope { scope: ScopeId, index: usize },
    /// A shape the analysis does not model.
    Opaque(OpaqueReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OpaqueReason {
    /// A star over a nested field of a relation, named by the star node.
    FieldPath(NodeId),
    /// A star over an expression, named by the star node.
    Expression(NodeId),
    /// A star whose qualifier names several relations.
    Ambiguous(NodeId),
    /// A source whose columns are not modeled, such as VALUES.
    UnknownWidth,
    /// A scope that reads itself.
    Recursive,
}

/// The star-wide part of a name condition: the modifiers that restrict which
/// names a segment may still publish.
#[derive(Debug)]
pub(super) struct StarFilter {
    base: StarBase,
    exclude: Vec<StarColumnName>,
    ilike: Option<String>,
}

/// Conditions a name has to meet to come out of an unknown segment. Each
/// filter is paired with the range variables the segment had where the filter
/// applied, because a qualified EXCLUDE is matched against them.
#[derive(Debug, Clone, Default)]
pub(super) struct NameDomain {
    filters: Vec<(Rc<StarFilter>, Rc<[Ident]>)>,
    /// Names the segment no longer publishes: merged by USING or renamed away.
    removed: Vec<Ident>,
}

impl NameDomain {
    pub(super) fn admits(&self, name: &Ident, dialect: Dialect) -> bool {
        !self
            .removed
            .iter()
            .any(|removed| same_column_name(removed, name, dialect))
            && self.filters.iter().all(|(filter, ranges)| {
                !filter.exclude.iter().any(|excluded| {
                    excluded_matches_name(excluded, name, Some(&filter.base), ranges, dialect)
                }) && filter
                    .ilike
                    .as_deref()
                    .is_none_or(|pattern| ilike_matches(pattern, &name.value))
            })
    }
}

/// A column with a known name.
#[derive(Debug, Clone)]
pub(super) struct ColumnSlot {
    pub name: Ident,
    pub output_name: OutputName,
    /// Range variables the column is visible under. A merged column keeps
    /// the range variables of both sides, whatever the join kind.
    pub ranges: Rc<[Ident]>,
    pub value: ValueId,
}

/// Columns the analysis cannot enumerate, with the names they may still
/// publish.
#[derive(Debug, Clone)]
pub(super) struct UnknownSegment {
    pub provider: UnknownProvider,
    pub ranges: Rc<[Ident]>,
    pub domain: NameDomain,
}

impl UnknownSegment {
    fn opaque(reason: OpaqueReason) -> Self {
        Self {
            provider: UnknownProvider::Opaque(reason),
            ranges: Rc::from([]),
            domain: NameDomain::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum OutputElement {
    Column(ColumnSlot),
    Unknown(UnknownSegment),
    /// Columns a set operation merges in from its right operand beyond the
    /// ones its left operand lists. They are not listed, so they do not count
    /// towards the width, but a star over the set operation still publishes
    /// them.
    Tail(BranchTail),
}

/// The right-operand columns of a set operation, subject to the same name
/// conditions as an unknown segment.
#[derive(Debug, Clone)]
pub(super) struct BranchTail {
    /// The set operation that merges them in.
    pub scope: ScopeId,
    pub ranges: Rc<[Ident]>,
    pub domain: NameDomain,
}

/// A name that can be referenced but is not listed as an output column: the
/// key of a USING join whose sides are both unknown.
#[derive(Debug, Clone)]
pub(super) struct SymbolicColumn {
    pub name: Ident,
    pub ranges: Rc<[Ident]>,
    pub value: ValueId,
}

/// All candidates for a name, built on first use. Names are compared by the
/// dialect's column key and duplicates are kept.
#[derive(Debug)]
pub(super) struct NameIndex {
    dialect: Dialect,
    slots: OnceCell<HashMap<String, Vec<usize>>>,
    pub symbolic: Vec<SymbolicColumn>,
}

#[derive(Debug)]
pub(super) struct OutputModel {
    pub elements: Vec<OutputElement>,
    pub names: NameIndex,
    pub width: Width,
    /// Why the width of a scope is unknown, following only the scopes that
    /// produce its output.
    #[allow(dead_code)]
    pub unknown_reasons: Vec<&'static str>,
}

impl OutputModel {
    fn new(part: Part, unknown_reasons: Vec<&'static str>, dialect: Dialect) -> Self {
        let listed = part
            .elements
            .iter()
            .filter(|element| !matches!(element, OutputElement::Tail(_)))
            .count();
        let width = if part
            .elements
            .iter()
            .any(|element| matches!(element, OutputElement::Unknown(_)))
        {
            Width::Unknown
        } else {
            Width::Exact(listed)
        };
        Self {
            elements: part.elements,
            names: NameIndex {
                dialect,
                slots: OnceCell::new(),
                symbolic: part.symbolic,
            },
            width,
            unknown_reasons,
        }
    }

    fn opaque(reason: OpaqueReason, dialect: Dialect) -> Self {
        Self::new(
            Part {
                elements: vec![OutputElement::Unknown(UnknownSegment::opaque(reason))],
                symbolic: Vec::new(),
            },
            Vec::new(),
            dialect,
        )
    }

    /// The number of elements that stand for a column or a segment of the
    /// output, as opposed to merged-in tails.
    pub(super) fn listed_len(&self) -> usize {
        self.elements
            .iter()
            .filter(|element| !matches!(element, OutputElement::Tail(_)))
            .count()
    }

    pub(super) fn has_unknown_segment(&self) -> bool {
        self.width == Width::Unknown
    }

    /// Indexes of the enumerated columns named `name`, in output order.
    #[allow(dead_code)]
    pub(super) fn slots_named(&self, name: &Ident) -> &[usize] {
        let dialect = self.names.dialect;
        let index = self.names.slots.get_or_init(|| {
            let mut index: HashMap<String, Vec<usize>> = HashMap::new();
            for (position, element) in self.elements.iter().enumerate() {
                if let OutputElement::Column(slot) = element {
                    index
                        .entry(slot.name.column_key(dialect).into_owned())
                        .or_default()
                        .push(position);
                }
            }
            index
        });
        index
            .get(name.column_key(dialect).as_ref())
            .map_or(&[], Vec::as_slice)
    }
}

/// Elements together with the names referenced without being listed.
#[derive(Debug, Clone, Default)]
struct Part {
    elements: Vec<OutputElement>,
    symbolic: Vec<SymbolicColumn>,
}

/// The output of the relations a join combined, which can be merged further.
#[derive(Debug, Clone)]
struct Group {
    members: Vec<Ident>,
    part: Part,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CacheKey {
    Scope(ScopeId),
    Star(NodeId),
}

/// The models built so far for one statement, each built at most once.
///
/// A model that was built while a cycle was cut (a recursive CTE reading
/// itself) is not kept, because it depends on where the walk started.
#[derive(Default)]
pub(super) struct ModelStore {
    scopes: HashMap<ScopeId, Rc<OutputModel>>,
    stars: HashMap<NodeId, Rc<OutputModel>>,
    /// Models under construction, each flagged when a cycle was cut beneath it.
    active: Vec<(CacheKey, bool)>,
}

/// Builds models for a statement into a [`ModelStore`].
pub(super) struct OutputModels<'a, 'c, 's> {
    graph: &'a RawGraph,
    catalog: Option<&'a CatalogContext<'c>>,
    store: &'s mut ModelStore,
}

impl<'a, 'c, 's> OutputModels<'a, 'c, 's> {
    pub(super) fn new(
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

    pub(super) fn graph(&self) -> &'a RawGraph {
        self.graph
    }

    pub(super) fn scope(&mut self, scope: ScopeId) -> Rc<OutputModel> {
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

    pub(super) fn star(&mut self, node_id: NodeId) -> Rc<OutputModel> {
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
            RawNode::Star {
                base,
                options,
                scope,
            } => self.build_star(node_id, base, options, *scope),
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
    fn enter(&mut self, key: CacheKey) -> bool {
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
    fn leave(&mut self) -> bool {
        self.store.active.pop().is_some_and(|(_, tainted)| !tainted)
    }

    fn build_scope(&mut self, scope: ScopeId) -> OutputModel {
        let graph = self.graph;
        let mut part = match graph.scopes.output_plan(scope) {
            OutputPlan::Projection => {
                let mut part = Part::default();
                for column in graph.scopes.output_columns(scope) {
                    if matches!(graph.nodes[column.node_id], RawNode::Star { .. }) {
                        let star = self.star(column.node_id);
                        part.elements.extend(star.elements.iter().cloned());
                        part.symbolic.extend(star.names.symbolic.iter().cloned());
                    } else {
                        part.elements.push(OutputElement::Column(ColumnSlot {
                            name: column.name.clone(),
                            output_name: graph
                                .output_names
                                .get(&column.node_id)
                                .cloned()
                                .unwrap_or_else(|| OutputName::Explicit(column.name.clone())),
                            ranges: Rc::from([]),
                            value: ValueId::Expr(column.node_id),
                        }));
                    }
                }
                part
            }
            OutputPlan::Delegate(child) => {
                let model = self.scope(*child);
                forward_scope(*child, &model, &Rc::from([]), false)
            }
            // The left operand defines the names of a set operation.
            OutputPlan::SetOperation {
                left, recursive, ..
            } => {
                let model = self.scope(*left);
                let mut part = forward_scope(*left, &model, &Rc::from([]), true);
                if !recursive {
                    // This operation's tail covers whatever the left operand
                    // already carried.
                    part.elements
                        .retain(|element| !matches!(element, OutputElement::Tail(_)));
                    part.elements.push(OutputElement::Tail(BranchTail {
                        scope,
                        ranges: Rc::from([]),
                        domain: NameDomain::default(),
                    }));
                }
                part
            }
        };
        // Only the output plan is followed, so a flagged operand shows up here
        // while an unrelated nested query does not.
        if graph.scopes.has_unknown_output_width(scope)
            && !part
                .elements
                .iter()
                .any(|element| matches!(element, OutputElement::Unknown(_)))
        {
            part.elements
                .push(OutputElement::Unknown(UnknownSegment::opaque(
                    OpaqueReason::UnknownWidth,
                )));
        }
        OutputModel::new(
            part,
            graph.scopes.unknown_output_reasons(scope),
            graph.dialect,
        )
    }

    fn build_star(
        &mut self,
        node_id: NodeId,
        base: &StarBase,
        options: &StarOptions,
        scope: ScopeId,
    ) -> OutputModel {
        let graph = self.graph;
        let opaque = |reason| Part {
            elements: vec![OutputElement::Unknown(UnknownSegment::opaque(reason))],
            symbolic: Vec::new(),
        };
        let part = match resolve_star_target(base, scope, graph) {
            StarTarget::All => self.all_bindings(scope),
            StarTarget::Binding { name, binding } => self.binding_part(&binding, &Rc::from([name])),
            StarTarget::Unknown(table) => self.relation_part(&Rc::new(table), &Rc::from([])),
            StarTarget::FieldPath { .. } => opaque(OpaqueReason::FieldPath(node_id)),
            StarTarget::Expr => opaque(OpaqueReason::Expression(node_id)),
            StarTarget::Ambiguous(_) => opaque(OpaqueReason::Ambiguous(node_id)),
        };
        OutputModel::new(
            apply_options(base, options, part, graph.dialect),
            Vec::new(),
            graph.dialect,
        )
    }

    /// An unqualified star: every relation of the FROM clause with USING and
    /// NATURAL joins folded, followed by the anonymous derived tables.
    fn all_bindings(&mut self, scope: ScopeId) -> Part {
        let graph = self.graph;
        let mut groups = Vec::new();
        for (name, binding) in effective_bindings(scope, graph) {
            let part = self.binding_part(&binding, &Rc::from([name.clone()]));
            groups.push(Group {
                members: vec![name],
                part,
            });
        }
        let mut part = fold_join_columns(groups, graph.scopes.join_merges(scope), graph.dialect);
        for &child in graph.scopes.anonymous_derived(scope) {
            let model = self.scope(child);
            let child_part = forward_scope(child, &model, &Rc::from([]), false);
            part.elements.extend(child_part.elements);
            part.symbolic.extend(child_part.symbolic);
        }
        part
    }

    fn binding_part(&mut self, binding: &Binding, ranges: &Rc<[Ident]>) -> Part {
        match binding {
            Binding::Table(table) => self.relation_part(&Rc::new(table.clone()), ranges),
            Binding::Cte(child) | Binding::DerivedTable(child) => {
                let model = self.scope(*child);
                forward_scope(*child, &model, ranges, false)
            }
            Binding::VirtualSource(source) => Part {
                elements: self
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
                            value: ValueId::VirtualColumn {
                                source: *source,
                                index,
                            },
                        })
                    })
                    .collect(),
                symbolic: Vec::new(),
            },
        }
    }

    fn relation_part(&self, table: &Rc<TableRef>, ranges: &Rc<[Ident]>) -> Part {
        let elements = match self.catalog.and_then(|catalog| catalog.list_columns(table)) {
            Some(columns) => columns
                .into_iter()
                .map(|column| {
                    OutputElement::Column(ColumnSlot {
                        name: column.clone(),
                        output_name: OutputName::Explicit(column.clone()),
                        ranges: ranges.clone(),
                        value: ValueId::Physical {
                            table: table.clone(),
                            column,
                        },
                    })
                })
                .collect(),
            None => vec![OutputElement::Unknown(UnknownSegment {
                provider: UnknownProvider::Relation(table.clone()),
                ranges: ranges.clone(),
                domain: NameDomain::default(),
            })],
        };
        Part {
            elements,
            symbolic: Vec::new(),
        }
    }
}

/// Re-express the output of a scope under the range variables of the star
/// that reads it. `set_operation` marks slots that combine several operands.
fn forward_scope(
    scope: ScopeId,
    model: &OutputModel,
    ranges: &Rc<[Ident]>,
    set_operation: bool,
) -> Part {
    let elements = model
        .elements
        .iter()
        .enumerate()
        .map(|(index, element)| match element {
            OutputElement::Column(slot) => OutputElement::Column(ColumnSlot {
                name: slot.name.clone(),
                output_name: slot.output_name.clone(),
                ranges: ranges.clone(),
                value: if set_operation {
                    ValueId::SetOperation { scope, index }
                } else {
                    ValueId::ScopeSlot { scope, index }
                },
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
        .collect();
    let symbolic = model
        .names
        .symbolic
        .iter()
        .map(|symbolic| SymbolicColumn {
            name: symbolic.name.clone(),
            ranges: ranges.clone(),
            value: ValueId::ScopeName {
                scope,
                column: symbolic.name.clone(),
            },
        })
        .collect();
    Part { elements, symbolic }
}

fn union_ranges(left: &[Ident], right: &[Ident]) -> Rc<[Ident]> {
    left.iter().chain(right).cloned().collect()
}

/// Fold USING and NATURAL joins in the order they were written. A shape the
/// fold cannot make sense of (a missing or ambiguous key, relations that
/// share a name) leaves every relation's columns as they are.
fn fold_join_columns(groups: Vec<Group>, merges: &[JoinMerge], dialect: Dialect) -> Part {
    if merges.is_empty() {
        return concat_groups(groups);
    }
    let original = groups.clone();
    fold_groups(groups, merges, dialect).map_or_else(|| concat_groups(original), concat_groups)
}

fn concat_groups(groups: Vec<Group>) -> Part {
    let mut result = Part::default();
    for group in groups {
        result.elements.extend(group.part.elements);
        result.symbolic.extend(group.part.symbolic);
    }
    result
}

/// Where a USING key sits on one side of a join.
#[derive(Clone, Copy)]
enum KeyAt {
    Slot(usize),
    Symbolic(usize),
    /// No column is listed, but unknown segments may publish the key.
    Unlisted,
}

/// Locate the column a USING key names on one side. A key matching several
/// columns, or none that could exist, makes the join unfoldable.
fn find_key(part: &Part, key: &Ident, dialect: Dialect) -> Option<KeyAt> {
    let slots: Vec<usize> = part
        .elements
        .iter()
        .enumerate()
        .filter_map(|(index, element)| {
            matches!(element, OutputElement::Column(slot) if same_column_name(&slot.name, key, dialect))
                .then_some(index)
        })
        .collect();
    let symbolic: Vec<usize> = part
        .symbolic
        .iter()
        .enumerate()
        .filter_map(|(index, symbolic)| {
            same_column_name(&symbolic.name, key, dialect).then_some(index)
        })
        .collect();
    match (slots.as_slice(), symbolic.as_slice()) {
        ([index], []) => Some(KeyAt::Slot(*index)),
        ([], [index]) => Some(KeyAt::Symbolic(*index)),
        ([], []) => (!unknown_publishers(&part.elements, key, dialect)
            .0
            .is_empty())
        .then_some(KeyAt::Unlisted),
        _ => None,
    }
}

/// The value and range variables a side contributes to a merged key. Unknown
/// segments that may publish the key stop doing so: the merged column
/// replaces it.
fn key_contribution(
    part: &mut Part,
    at: KeyAt,
    key: &Ident,
    dialect: Dialect,
) -> (ValueId, Rc<[Ident]>) {
    match at {
        KeyAt::Slot(index) => match &part.elements[index] {
            OutputElement::Column(slot) => (slot.value.clone(), slot.ranges.clone()),
            _ => unreachable!("a key slot is a column"),
        },
        KeyAt::Symbolic(index) => {
            let symbolic = &part.symbolic[index];
            (symbolic.value.clone(), symbolic.ranges.clone())
        }
        // The key is guaranteed to exist on every joined relation, so a
        // physical relation contributes the column itself and a CTE or
        // derived table contributes whatever a direct reference to the name
        // would find in it.
        KeyAt::Unlisted => {
            let mut values: Vec<ValueId> = Vec::new();
            let mut opaque = Vec::new();
            let mut ranges: Vec<Ident> = Vec::new();
            for element in &mut part.elements {
                let OutputElement::Unknown(segment) = element else {
                    continue;
                };
                if !segment.domain.admits(key, dialect) {
                    continue;
                }
                segment.domain.removed.push(key.clone());
                ranges.extend(segment.ranges.iter().cloned());
                match &segment.provider {
                    UnknownProvider::Relation(table) => values.push(ValueId::Physical {
                        table: table.clone(),
                        column: key.clone(),
                    }),
                    UnknownProvider::Scope { scope, .. } => {
                        if !values.iter().any(
                            |value| matches!(value, ValueId::ScopeName { scope: seen, .. } if seen == scope),
                        ) {
                            values.push(ValueId::ScopeName {
                                scope: *scope,
                                column: key.clone(),
                            });
                        }
                    }
                    provider @ UnknownProvider::Opaque(_) => opaque.push(provider.clone()),
                }
            }
            if !opaque.is_empty() {
                values.push(ValueId::Named {
                    providers: opaque,
                    column: key.clone(),
                });
            }
            let value = values
                .into_iter()
                .reduce(|left, right| ValueId::Merged(Box::new(left), Box::new(right)))
                .expect("an unlisted key has a publisher");
            (value, ranges.into())
        }
    }
}

fn fold_groups(
    mut groups: Vec<Group>,
    merges: &[JoinMerge],
    dialect: Dialect,
) -> Option<Vec<Group>> {
    let mut seen = HashSet::new();
    if groups
        .iter()
        .flat_map(|group| &group.members)
        .any(|member| !seen.insert(member.column_key(dialect).into_owned()))
    {
        return None;
    }
    for merge in merges {
        let side = |members: &[Ident]| {
            groups
                .iter()
                .enumerate()
                .filter_map(|(index, group)| {
                    group
                        .members
                        .iter()
                        .any(|name| {
                            members
                                .iter()
                                .any(|member| same_column_name(member, name, dialect))
                        })
                        .then_some(index)
                })
                .collect::<Vec<_>>()
        };
        let left = side(&merge.left);
        let right = side(&merge.right);
        if left.is_empty() || right.is_empty() || left.iter().any(|index| right.contains(index)) {
            return None;
        }
        let members = left
            .iter()
            .chain(&right)
            .flat_map(|&index| groups[index].members.clone())
            .collect();
        let first = *left.iter().chain(&right).min()?;
        let mut left_part = take_parts(&mut groups, &left);
        let mut right_part = take_parts(&mut groups, &right);
        let mut position = 0;
        groups.retain(|_| {
            let keep = !left.contains(&position) && !right.contains(&position);
            position += 1;
            keep
        });

        let keys: Vec<Ident> = match &merge.columns {
            MergedColumns::Using(names) => names.clone(),
            // Only listed columns establish a shared name. Unknown segments
            // may publish more, but guessing which would invent columns.
            MergedColumns::Natural => left_part
                .elements
                .iter()
                .filter_map(|element| match element {
                    OutputElement::Column(slot)
                        if right_part.elements.iter().any(|other| {
                            matches!(other, OutputElement::Column(other)
                                if same_column_name(&other.name, &slot.name, dialect))
                        }) =>
                    {
                        Some(slot.name.clone())
                    }
                    _ => None,
                })
                .collect(),
        };

        let mut output = Vec::new();
        let mut selected_keys = HashSet::new();
        let mut symbolic = Vec::new();
        for key in keys {
            let left_at = find_key(&left_part, &key, dialect)?;
            let right_at = find_key(&right_part, &key, dialect)?;
            if !selected_keys.insert(key.column_key(dialect).into_owned()) {
                return None;
            }
            let (left_value, left_ranges) =
                key_contribution(&mut left_part, left_at, &key, dialect);
            let (right_value, right_ranges) =
                key_contribution(&mut right_part, right_at, &key, dialect);
            let ranges = union_ranges(&left_ranges, &right_ranges);
            // An outer join takes the value from the preserved side; an inner
            // or full join may take it from either.
            let value = match merge.kind {
                JoinMergeKind::Left => left_value,
                JoinMergeKind::Right => right_value,
                JoinMergeKind::Inner | JoinMergeKind::Full => {
                    ValueId::Merged(Box::new(left_value), Box::new(right_value))
                }
            };
            match (left_at, right_at) {
                (KeyAt::Slot(left_index), KeyAt::Slot(right_index)) => {
                    // The merged column leads the output and takes the left
                    // column's names.
                    let OutputElement::Column(left_slot) = left_part.elements.remove(left_index)
                    else {
                        unreachable!("a key slot is a column");
                    };
                    right_part.elements.remove(right_index);
                    output.push(OutputElement::Column(ColumnSlot {
                        name: left_slot.name,
                        output_name: left_slot.output_name,
                        ranges,
                        value,
                    }));
                }
                // Only one side lists the column, so it stays where it is and
                // only gains the other side's range variables and value.
                (KeyAt::Slot(index), other) => {
                    retarget_slot(&mut left_part, index, ranges, value);
                    drop_symbolic(&mut right_part, other);
                }
                (other, KeyAt::Slot(index)) => {
                    retarget_slot(&mut right_part, index, ranges, value);
                    drop_symbolic(&mut left_part, other);
                }
                (left_other, right_other) => {
                    drop_symbolic(&mut left_part, left_other);
                    drop_symbolic(&mut right_part, right_other);
                    symbolic.push(SymbolicColumn {
                        name: key,
                        ranges,
                        value,
                    });
                }
            }
        }
        output.extend(left_part.elements);
        output.extend(right_part.elements);
        symbolic.extend(left_part.symbolic);
        symbolic.extend(right_part.symbolic);
        groups.insert(
            first,
            Group {
                members,
                part: Part {
                    elements: output,
                    symbolic,
                },
            },
        );
    }
    Some(groups)
}

/// Move the contents of the groups at `indexes` into one part.
fn take_parts(groups: &mut [Group], indexes: &[usize]) -> Part {
    let mut part = Part::default();
    for &index in indexes {
        let group = &mut groups[index];
        part.elements.append(&mut group.part.elements);
        part.symbolic.append(&mut group.part.symbolic);
    }
    part
}

fn retarget_slot(part: &mut Part, index: usize, ranges: Rc<[Ident]>, value: ValueId) {
    if let OutputElement::Column(slot) = &mut part.elements[index] {
        slot.ranges = ranges;
        slot.value = value;
    }
}

fn drop_symbolic(part: &mut Part, at: KeyAt) {
    if let KeyAt::Symbolic(index) = at {
        part.symbolic.remove(index);
    }
}

/// The providers and range variables of the unknown segments that may publish
/// `name`.
fn unknown_publishers(
    elements: &[OutputElement],
    name: &Ident,
    dialect: Dialect,
) -> (Vec<UnknownProvider>, Vec<Ident>) {
    let mut providers = Vec::new();
    let mut ranges = Vec::new();
    for element in elements {
        if let OutputElement::Unknown(segment) = element
            && segment.domain.admits(name, dialect)
        {
            providers.push(segment.provider.clone());
            ranges.extend(segment.ranges.iter().cloned());
        }
    }
    (providers, ranges)
}

/// Apply EXCLUDE/ILIKE, then REPLACE, then RENAME in list order to the
/// current names. Columns that are not listed can only be restricted: their
/// domain records the filters, and a REPLACE or RENAME that names one makes it
/// an explicit column of its own.
fn apply_options(base: &StarBase, options: &StarOptions, part: Part, dialect: Dialect) -> Part {
    let admits = |name: &Ident, ranges: &[Ident]| {
        !options
            .exclude
            .iter()
            .any(|excluded| excluded_matches_name(excluded, name, Some(base), ranges, dialect))
            && options
                .ilike
                .as_deref()
                .is_none_or(|pattern| ilike_matches(pattern, &name.value))
    };
    let filter = (!options.exclude.is_empty() || options.ilike.is_some()).then(|| {
        Rc::new(StarFilter {
            base: base.clone(),
            exclude: options.exclude.clone(),
            ilike: options.ilike.clone(),
        })
    });
    let mut elements: Vec<OutputElement> = part
        .elements
        .into_iter()
        .filter_map(|element| match element {
            OutputElement::Column(slot) => {
                admits(&slot.name, &slot.ranges).then_some(OutputElement::Column(slot))
            }
            OutputElement::Unknown(mut segment) => {
                if let Some(filter) = &filter {
                    segment
                        .domain
                        .filters
                        .push((filter.clone(), segment.ranges.clone()));
                }
                Some(OutputElement::Unknown(segment))
            }
            OutputElement::Tail(mut tail) => {
                if let Some(filter) = &filter {
                    tail.domain
                        .filters
                        .push((filter.clone(), tail.ranges.clone()));
                }
                Some(OutputElement::Tail(tail))
            }
        })
        .collect();
    let mut symbolic: Vec<SymbolicColumn> = part
        .symbolic
        .into_iter()
        .filter(|symbolic| admits(&symbolic.name, &symbolic.ranges))
        .collect();

    for replacement in &options.replace {
        if let Some(slot) = first_slot_named(&mut elements, &replacement.column, dialect) {
            slot.value = ValueId::Expr(replacement.node_id);
            continue;
        }
        let ranges = if let Some(position) = symbolic
            .iter()
            .position(|symbolic| same_column_name(&symbolic.name, &replacement.column, dialect))
        {
            symbolic.remove(position).ranges
        } else {
            let (providers, ranges) = unknown_publishers(&elements, &replacement.column, dialect);
            if providers.is_empty() {
                continue;
            }
            ranges.into()
        };
        elements.push(OutputElement::Column(ColumnSlot {
            name: replacement.column.clone(),
            output_name: OutputName::Explicit(replacement.column.clone()),
            ranges,
            value: ValueId::Expr(replacement.node_id),
        }));
    }

    for (old, new) in &options.rename {
        if let Some(slot) = first_slot_named(&mut elements, old, dialect) {
            slot.name.clone_from(new);
            slot.output_name = OutputName::Explicit(new.clone());
            continue;
        }
        let (ranges, value) = if let Some(position) = symbolic
            .iter()
            .position(|symbolic| same_column_name(&symbolic.name, old, dialect))
        {
            let symbolic = symbolic.remove(position);
            (symbolic.ranges, symbolic.value)
        } else {
            let (providers, ranges) = unknown_publishers(&elements, old, dialect);
            if providers.is_empty() {
                continue;
            }
            for element in &mut elements {
                if let OutputElement::Unknown(segment) = element
                    && segment.domain.admits(old, dialect)
                {
                    segment.domain.removed.push(old.clone());
                }
            }
            (
                ranges.into(),
                ValueId::Named {
                    providers,
                    column: old.clone(),
                },
            )
        };
        elements.push(OutputElement::Column(ColumnSlot {
            name: new.clone(),
            output_name: OutputName::Explicit(new.clone()),
            ranges,
            value,
        }));
    }
    Part { elements, symbolic }
}

fn first_slot_named<'e>(
    elements: &'e mut [OutputElement],
    name: &Ident,
    dialect: Dialect,
) -> Option<&'e mut ColumnSlot> {
    elements.iter_mut().find_map(|element| match element {
        OutputElement::Column(slot) if same_column_name(&slot.name, name, dialect) => Some(slot),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::{
        ModelStore, OutputElement, OutputModel, OutputModels, UnknownProvider, ValueId, Width,
    };
    use crate::catalog::CatalogContext;
    use crate::graph::RawGraph;
    use crate::graph::node::RawNode;
    use crate::graph::scope::ScopeTree;
    use crate::{Dialect, Ident};

    struct Tables(Vec<(&'static str, Vec<&'static str>)>);

    impl crate::Catalog for Tables {
        fn lookup(&self, relation: &crate::Relation) -> Option<crate::CatalogEntry<'_>> {
            let (_, columns) = self
                .0
                .iter()
                .find(|(name, _)| *name == relation.name.value)?;
            Some(crate::CatalogEntry {
                relation: relation.clone(),
                columns: columns
                    .iter()
                    .map(|name| crate::CatalogColumn {
                        name: Ident::unquoted(*name),
                        data_type: None,
                        fields: None,
                    })
                    .collect::<Vec<_>>()
                    .into(),
            })
        }
    }

    fn graph(sql: &str) -> RawGraph {
        let dialect = Dialect::Generic;
        let statements = sqlparser::parser::Parser::new(&*dialect.to_sqlparser_dialect())
            .try_with_sql(sql)
            .and_then(|mut parser| parser.parse_statements())
            .expect("test SQL parses");
        let (graph, _, _) = crate::build::LineageBuilder::new(dialect)
            .build(&statements[0])
            .unwrap_or_else(|_| panic!("test SQL builds"));
        graph
    }

    /// The model of the first star of `sql` under the given catalog tables.
    fn first_star(sql: &str, tables: &[(&'static str, Vec<&'static str>)]) -> Rc<OutputModel> {
        let graph = graph(sql);
        let catalog = Tables(tables.to_vec());
        let context = CatalogContext::new(&catalog, Dialect::Generic);
        let mut store = ModelStore::default();
        let mut models = OutputModels::new(&graph, Some(&context), &mut store);
        let node = graph
            .nodes
            .iter()
            .position(|node| matches!(node, RawNode::Star { .. }))
            .expect("test SQL has a star");
        models.star(node)
    }

    /// Column names in output order; `?` stands for an unknown segment.
    fn layout(model: &OutputModel) -> Vec<String> {
        model
            .elements
            .iter()
            .map(|element| match element {
                OutputElement::Column(slot) => slot.name.value.clone(),
                OutputElement::Unknown(_) => "?".to_owned(),
                OutputElement::Tail(_) => "~".to_owned(),
            })
            .collect()
    }

    fn ranges(model: &OutputModel, name: &str) -> Vec<String> {
        let [index] = model.slots_named(&Ident::unquoted(name)) else {
            panic!("expected exactly one column named {name}");
        };
        let OutputElement::Column(slot) = &model.elements[*index] else {
            unreachable!();
        };
        slot.ranges
            .iter()
            .map(|range| range.value.clone())
            .collect()
    }

    #[test]
    fn using_folds_the_key_once_and_keeps_both_range_variables() {
        let model = first_star(
            "SELECT * FROM a JOIN b USING (id)",
            &[("a", vec!["x", "id"]), ("b", vec!["id", "y"])],
        );
        assert_eq!(layout(&model), ["id", "x", "y"]);
        assert_eq!(model.width, Width::Exact(3));
        assert_eq!(ranges(&model, "id"), ["a", "b"]);
        assert_eq!(ranges(&model, "x"), ["a"]);
    }

    #[test]
    fn natural_join_folds_the_shared_names_of_listed_columns() {
        let model = first_star(
            "SELECT * FROM a NATURAL JOIN b",
            &[("a", vec!["id", "x"]), ("b", vec!["y", "id"])],
        );
        assert_eq!(layout(&model), ["id", "x", "y"]);
    }

    #[test]
    fn qualified_exclude_matches_either_side_of_a_merged_column() {
        for qualifier in ["a", "b"] {
            let model = first_star(
                &format!("SELECT * EXCLUDE ({qualifier}.id) FROM a JOIN b USING (id)"),
                &[("a", vec!["id", "x"]), ("b", vec!["id", "y"])],
            );
            assert_eq!(layout(&model), ["x", "y"]);
        }
    }

    #[test]
    fn rename_applies_in_list_order_to_the_current_names() {
        let model = first_star(
            "SELECT * RENAME (x AS y, y AS z) FROM t",
            &[("t", vec!["x", "y"])],
        );
        assert_eq!(layout(&model), ["z", "y"]);
    }

    #[test]
    fn replace_keeps_position_and_name_but_not_excluded_columns() {
        let model = first_star(
            "SELECT * EXCLUDE (y) REPLACE (1 AS x, 2 AS y) FROM t",
            &[("t", vec!["x", "y", "z"])],
        );
        assert_eq!(layout(&model), ["x", "z"]);
        let OutputElement::Column(slot) = &model.elements[0] else {
            unreachable!();
        };
        assert!(matches!(slot.value, ValueId::Expr(_)));
    }

    #[test]
    fn a_relation_without_columns_is_an_unknown_segment() {
        let model = first_star("SELECT * FROM t", &[]);
        assert_eq!(layout(&model), ["?"]);
        assert_eq!(model.width, Width::Unknown);
        assert!(model.has_unknown_segment());
        let [OutputElement::Unknown(segment)] = model.elements.as_slice() else {
            unreachable!();
        };
        assert!(matches!(segment.provider, UnknownProvider::Relation(_)));
    }

    #[test]
    fn exclude_restricts_the_names_an_unknown_segment_may_publish() {
        let model = first_star("SELECT * EXCLUDE (t.id) FROM t", &[]);
        let [OutputElement::Unknown(segment)] = model.elements.as_slice() else {
            unreachable!();
        };
        assert!(
            !segment
                .domain
                .admits(&Ident::unquoted("id"), Dialect::Generic)
        );
        assert!(
            segment
                .domain
                .admits(&Ident::unquoted("other"), Dialect::Generic)
        );
        let other = first_star("SELECT * EXCLUDE (u.id) FROM t JOIN u ON TRUE", &[]);
        assert!(other.has_unknown_segment());
    }

    #[test]
    fn using_with_one_known_side_binds_the_key_to_the_listed_column() {
        let model = first_star(
            "SELECT * FROM a JOIN b USING (id)",
            &[("b", vec!["id", "y"])],
        );
        // The listed column keeps its position. An inner join takes the key
        // from both sides, and the unknown relation supplies it as a column.
        assert_eq!(layout(&model), ["?", "id", "y"]);
        assert_eq!(ranges(&model, "id"), ["a", "b"]);
        let OutputElement::Column(slot) = &model.elements[1] else {
            unreachable!();
        };
        assert!(matches!(slot.value, ValueId::Merged(..)));
        let OutputElement::Unknown(segment) = &model.elements[0] else {
            unreachable!();
        };
        assert!(
            !segment
                .domain
                .admits(&Ident::unquoted("id"), Dialect::Generic)
        );
        assert!(
            segment
                .domain
                .admits(&Ident::unquoted("name"), Dialect::Generic)
        );
        assert!(model.names.symbolic.is_empty());
    }

    #[test]
    fn using_value_follows_the_join_kind_when_one_side_is_unknown() {
        // The unknown `a` supplies `id` as a column of its own; `b` lists it.
        for (join, expected) in [
            ("JOIN", "merged"),
            ("LEFT JOIN", "a"),
            ("RIGHT JOIN", "b"),
            ("FULL JOIN", "merged"),
        ] {
            let model = first_star(
                &format!("SELECT * FROM a {join} b USING (id)"),
                &[("b", vec!["id", "y"])],
            );
            assert_eq!(layout(&model), ["?", "id", "y"], "{join}");
            let OutputElement::Column(slot) = &model.elements[1] else {
                unreachable!();
            };
            match (&slot.value, expected) {
                (ValueId::Merged(..), "merged") => {}
                (ValueId::Physical { table, .. }, owner) => {
                    assert_eq!(table.table.value, owner, "{join}");
                }
                (value, _) => panic!("{join}: unexpected {value:?}"),
            }
        }
    }

    #[test]
    fn set_operation_keeps_the_columns_its_right_operand_adds_as_a_tail() {
        let graph = graph("SELECT * FROM t1 UNION ALL SELECT a, b FROM t2");
        let mut store = ModelStore::default();
        let mut models = OutputModels::new(&graph, None, &mut store);
        let model = models.scope(ScopeTree::root());
        assert_eq!(layout(&model), ["?", "~"]);
        assert_eq!(model.listed_len(), 1);
        assert_eq!(model.width, Width::Unknown);
    }

    #[test]
    fn using_with_two_unknown_sides_names_the_key_without_listing_it() {
        let model = first_star("SELECT * FROM a JOIN b USING (id)", &[]);
        assert_eq!(layout(&model), ["?", "?"]);
        let [symbolic] = model.names.symbolic.as_slice() else {
            panic!("expected one symbolic column");
        };
        assert_eq!(symbolic.name, Ident::unquoted("id"));
        assert_eq!(symbolic.ranges.len(), 2);
        assert!(model.slots_named(&Ident::unquoted("id")).is_empty());

        let excluded = first_star("SELECT * EXCLUDE (b.id) FROM a JOIN b USING (id)", &[]);
        assert!(excluded.names.symbolic.is_empty());
    }

    #[test]
    fn replace_and_rename_of_unlisted_columns_become_explicit_columns() {
        let model = first_star("SELECT * REPLACE (1 AS a) RENAME (b AS c) FROM t", &[]);
        assert_eq!(layout(&model), ["?", "a", "c"]);
        let OutputElement::Column(renamed) = &model.elements[2] else {
            unreachable!();
        };
        assert!(matches!(renamed.value, ValueId::Named { .. }));
        let OutputElement::Unknown(segment) = &model.elements[0] else {
            unreachable!();
        };
        assert!(
            !segment
                .domain
                .admits(&Ident::unquoted("b"), Dialect::Generic)
        );
    }

    #[test]
    fn replace_of_an_excluded_unlisted_column_adds_nothing() {
        let model = first_star("SELECT * EXCLUDE (a) REPLACE (1 AS a) FROM t", &[]);
        assert_eq!(layout(&model), ["?"]);
    }

    #[test]
    fn scope_models_are_built_once_and_forward_child_slots() {
        let graph = graph("WITH c AS (SELECT x, y FROM t) SELECT * FROM c");
        let mut store = ModelStore::default();
        let mut models = OutputModels::new(&graph, None, &mut store);
        let root = models.scope(ScopeTree::root());
        assert!(Rc::ptr_eq(&root, &models.scope(ScopeTree::root())));
        assert_eq!(layout(&root), ["x", "y"]);
        let OutputElement::Column(slot) = &root.elements[0] else {
            unreachable!();
        };
        assert!(matches!(slot.value, ValueId::ScopeSlot { index: 0, .. }));
    }

    #[test]
    fn a_cycle_is_cut_to_an_unknown_segment() {
        let graph =
            graph("WITH RECURSIVE t AS (SELECT * FROM t UNION ALL SELECT 1) SELECT * FROM t");
        let mut store = ModelStore::default();
        let mut models = OutputModels::new(&graph, None, &mut store);
        assert!(models.scope(ScopeTree::root()).has_unknown_segment());
    }
}
