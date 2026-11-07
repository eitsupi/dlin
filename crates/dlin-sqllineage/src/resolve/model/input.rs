//! The model of a FROM clause: the relations of a scope with their joins
//! folded, and the lookup of an unqualified name in it.
//!
//! A star expands these elements and applies its own modifiers; a reference to
//! a name reads the same elements through [`InputModel::lookup`]. The join
//! rules that decide which columns exist live in the fold alone, and the one
//! function that weighs independent providers of a name against each other is
//! [`InputModel::choose`].

use std::collections::HashSet;
use std::rc::Rc;

use super::{
    CacheKey, ColumnSlot, Group, NameMatches, OpaqueReason, OutputElement, OutputModel,
    OutputModels, ValueId, fold_join_columns, forward_scope,
};
use crate::graph::scope::{Binding, MergedColumns, ScopeId};
use crate::keys::table_key;
use crate::types::TableRef;
use crate::{Candidate, Dialect, Ident, Relation};

use super::super::target::{effective_bindings, same_column_name};

/// One occurrence of a range variable in a FROM clause: the name it is
/// visible under and what it reads. Occurrences are kept as values of their
/// own, so two ranges that share a name stay two candidates instead of
/// collapsing into whichever binding a name lookup finds first.
#[derive(Debug, Clone)]
pub(in crate::resolve) struct RangeRef {
    /// Position among the bindings of the FROM clause. Occurrences with the
    /// same index are the same range, whatever columns they carry.
    pub index: usize,
    pub name: Ident,
    pub binding: Binding,
}

impl RangeRef {
    /// The typed candidate a range stands for in an ambiguity. A virtual
    /// source is not a relation and is never a candidate.
    pub(in crate::resolve) fn candidate(&self) -> Option<Candidate> {
        match &self.binding {
            Binding::Table(table) => Some(Candidate::Relation(Relation {
                catalog: table.catalog.clone(),
                schema: table.schema.clone(),
                name: table.table.clone(),
            })),
            Binding::Cte(_) => Some(Candidate::Cte(self.name.clone())),
            Binding::DerivedTable(_) => Some(Candidate::Derived(Some(self.name.clone()))),
            Binding::VirtualSource(_) => None,
        }
    }
}

/// Names that a join over duplicated range variables cannot attribute: the
/// join sides are identified by range names, which two ranges share.
#[derive(Debug)]
pub(in crate::resolve) struct InvalidRanges {
    using: Vec<Ident>,
    natural: bool,
    /// Every relation of the FROM clause, since any of them may own the name.
    pub candidates: Vec<Candidate>,
}

impl InvalidRanges {
    pub(in crate::resolve) fn covers(&self, name: &Ident, dialect: Dialect) -> bool {
        self.natural
            || self
                .using
                .iter()
                .any(|using| same_column_name(using, name, dialect))
    }

    /// Whether the candidates are named by something other than physical
    /// relations, which a catalog cannot narrow down.
    pub(in crate::resolve) fn typed(&self) -> bool {
        self.candidates
            .iter()
            .any(|candidate| !matches!(candidate, Candidate::Relation(_)))
    }
}

/// What the relations of one FROM clause publish together, before any
/// projection: every binding in order, with USING and NATURAL joins folded.
///
/// A star reads these elements and applies its own modifiers. A reference to
/// an unqualified name reads the same elements through [`InputModel::lookup`],
/// so the join rules that decide which columns exist live in the fold alone.
#[derive(Debug)]
pub(in crate::resolve) struct InputModel {
    /// The folded relations of the FROM clause.
    pub(super) model: OutputModel,
    /// Columns of derived tables without a range variable. A star publishes
    /// them after the relations; a name is never looked up in them.
    pub(super) anonymous: Vec<OutputElement>,
    pub(super) invalid: Vec<Rc<InvalidRanges>>,
}

/// What an unqualified name denotes among the relations of a FROM clause.
#[derive(Debug)]
pub(in crate::resolve) enum InputLookup {
    /// Nothing is known to publish the name.
    Absent,
    /// The FROM clause holds only a virtual source, which does not publish
    /// columns by name.
    NoSource,
    /// One column. `contributing` are the ranges its value comes from.
    Column {
        value: ValueId,
        contributing: Rc<[RangeRef]>,
    },
    /// A CTE or derived table decides the name: it is the only range that
    /// can carry it, or it carries every column of that name there is.
    Scope { scope: ScopeId, range: RangeRef },
    /// A lone physical relation whose columns the catalog does not list.
    Physical(Rc<TableRef>),
    /// Several relations may carry the name, or several columns do. `typed`
    /// is set when a catalog must not narrow the candidates down: they are
    /// not all physical relations, or a join guarantees the name on some of
    /// them.
    Ambiguous {
        candidates: Vec<Candidate>,
        typed: bool,
    },
}

impl InputLookup {
    /// What a name denotes when the FROM clause holds a single relation: that
    /// relation, whatever it lists.
    fn single(range: &RangeRef) -> Self {
        match &range.binding {
            Binding::Table(table) => Self::Physical(Rc::new(table.clone())),
            Binding::Cte(scope) | Binding::DerivedTable(scope) => Self::Scope {
                scope: *scope,
                range: range.clone(),
            },
            Binding::VirtualSource(_) => Self::NoSource,
        }
    }

    /// The CTE or derived table binding the lookup settles on, if it settles
    /// on a single relation.
    pub(in crate::resolve) fn binding(&self) -> Option<Binding> {
        match self {
            Self::Scope { range, .. } => Some(range.binding.clone()),
            Self::Column { contributing, .. } => match &**contributing {
                [range] => Some(range.binding.clone()),
                _ => None,
            },
            _ => None,
        }
    }
}

fn occurrences<'r>(ranges: impl IntoIterator<Item = &'r RangeRef>) -> Vec<&'r RangeRef> {
    let mut seen = HashSet::new();
    ranges
        .into_iter()
        .filter(|range| seen.insert(range.index))
        .collect()
}

fn candidates_of<'r>(ranges: impl IntoIterator<Item = &'r RangeRef>) -> Vec<Candidate> {
    ranges.into_iter().filter_map(RangeRef::candidate).collect()
}

fn typed(candidates: &[Candidate]) -> bool {
    candidates
        .iter()
        .any(|candidate| !matches!(candidate, Candidate::Relation(_)))
}

impl InputModel {
    /// The elements an unqualified star publishes.
    pub(super) fn star_elements(&self) -> Vec<OutputElement> {
        self.model
            .elements
            .iter()
            .chain(&self.anonymous)
            .cloned()
            .collect()
    }

    /// Decide what the unqualified `name` denotes among several relations.
    fn lookup(&self, name: &Ident, dialect: Dialect) -> InputLookup {
        if let Some(invalid) = self
            .invalid
            .iter()
            .find(|invalid| invalid.covers(name, dialect))
        {
            return InputLookup::Ambiguous {
                candidates: invalid.candidates.clone(),
                typed: invalid.typed(),
            };
        }
        self.choose(&self.model.matches(name))
    }

    /// The one place that weighs independent providers of a name against each
    /// other. `matches` holds everything in the folded FROM clause that can
    /// carry the name.
    fn choose(&self, matches: &NameMatches) -> InputLookup {
        let elements = &self.model.elements;
        let slots: Vec<&ColumnSlot> = matches
            .listed
            .iter()
            .filter_map(|&index| match &elements[index] {
                OutputElement::Column(slot) => Some(slot),
                _ => None,
            })
            .collect();
        // Segments and tails that cannot be listed. Each relation counts
        // once, however many of its elements may carry the name.
        let providers = occurrences(matches.unknown.iter().chain(&matches.tails).flat_map(
            |&index| match &elements[index] {
                OutputElement::Unknown(segment) => segment.ranges.iter(),
                OutputElement::Tail(tail) => tail.ranges.iter(),
                OutputElement::Column(_) => [].iter(),
            },
        ));

        // A CTE or derived table that lists every column of the name decides
        // for itself, and one it merely may hide the name behind decides the
        // same way: its own columns come before its own unknown segments.
        if let Some(range) = sole_scope_range(&slots) {
            return InputLookup::Scope {
                scope: scope_of(&range.binding),
                range,
            };
        }
        match slots.as_slice() {
            [] => match providers.as_slice() {
                [] => InputLookup::Absent,
                [range] => InputLookup::single(range),
                several => {
                    let candidates = candidates_of(several.iter().copied());
                    InputLookup::Ambiguous {
                        typed: typed(&candidates),
                        candidates,
                    }
                }
            },
            [slot] => {
                // A column that a join guarantees on its sides still
                // competes with a relation outside that join: the join does
                // not say the other relation lacks the name.
                //
                // A CTE or derived table among the relations the column comes
                // from outranks the relations of unknown shape outside the
                // join: it is known to list the name.
                let backed = slot.candidate_ranges.iter().any(|range| {
                    matches!(range.binding, Binding::Cte(_) | Binding::DerivedTable(_))
                });
                if slot.join_key && !backed && !providers.is_empty() {
                    let mut candidates = candidates_of(slot.candidate_ranges.iter());
                    candidates.extend(candidates_of(providers.iter().copied()));
                    return InputLookup::Ambiguous {
                        candidates,
                        typed: true,
                    };
                }
                InputLookup::Column {
                    value: slot.value.clone(),
                    contributing: slot.candidate_ranges.clone(),
                }
            }
            several => {
                let candidates =
                    candidates_of(several.iter().flat_map(|slot| slot.candidate_ranges.iter()));
                InputLookup::Ambiguous {
                    typed: typed(&candidates) || several.iter().any(|slot| slot.join_key),
                    candidates,
                }
            }
        }
    }
}

fn scope_of(binding: &Binding) -> ScopeId {
    match binding {
        Binding::Cte(scope) | Binding::DerivedTable(scope) => *scope,
        _ => unreachable!("only CTEs and derived tables are scopes"),
    }
}

/// The CTE or derived table that lists all of `slots`, if there is exactly
/// one range and it is not a merge of several.
fn sole_scope_range(slots: &[&ColumnSlot]) -> Option<RangeRef> {
    let first = slots.first()?;
    let [range] = &*first.ranges else {
        return None;
    };
    if !matches!(range.binding, Binding::Cte(_) | Binding::DerivedTable(_)) {
        return None;
    }
    slots
        .iter()
        .all(|slot| {
            !slot.join_key && matches!(&*slot.ranges, [other] if other.index == range.index)
        })
        .then(|| range.clone())
}

impl OutputModels<'_, '_, '_> {
    /// The relations of a scope's FROM clause with their joins folded. This is
    /// what an unqualified star expands and what an unqualified name is read
    /// from; it is not the scope's output, which is projected from it.
    pub(in crate::resolve) fn input(&mut self, scope: ScopeId) -> Rc<InputModel> {
        if let Some(model) = self.store.inputs.get(&scope) {
            return model.clone();
        }
        if self.enter(CacheKey::Input(scope)) {
            return Rc::new(InputModel {
                model: OutputModel::opaque(OpaqueReason::Recursive, self.graph.dialect),
                anonymous: Vec::new(),
                invalid: Vec::new(),
            });
        }
        let model = Rc::new(self.build_input(scope));
        if self.leave() {
            self.store.inputs.insert(scope, model.clone());
        }
        model
    }

    /// Decide what the unqualified `name` denotes in the FROM clause of
    /// `scope`. A clause with a single relation needs no model: the name is
    /// that relation's.
    pub(in crate::resolve) fn lookup_input(&mut self, scope: ScopeId, name: &Ident) -> InputLookup {
        let bindings = effective_bindings(scope, self.graph);
        match bindings.as_slice() {
            [] => InputLookup::Absent,
            [(range_name, binding)] => InputLookup::single(&RangeRef {
                index: 0,
                name: range_name.clone(),
                binding: binding.clone(),
            }),
            _ => self.input(scope).lookup(name, self.graph.dialect),
        }
    }

    /// The position of a binding among the relations of a FROM clause. A
    /// binding of an enclosing query, which the clause does not contain, has
    /// none.
    pub(super) fn range_index(&self, scope: ScopeId, name: &Ident, binding: &Binding) -> usize {
        let dialect = self.graph.dialect;
        effective_bindings(scope, self.graph)
            .iter()
            .position(|(other_name, other)| {
                same_column_name(other_name, name, dialect)
                    && match (other, binding) {
                        (Binding::Table(left), Binding::Table(right)) => {
                            table_key(left, dialect) == table_key(right, dialect)
                        }
                        (
                            Binding::Cte(left) | Binding::DerivedTable(left),
                            Binding::Cte(right) | Binding::DerivedTable(right),
                        ) => left == right,
                        (Binding::VirtualSource(left), Binding::VirtualSource(right)) => {
                            left == right
                        }
                        _ => false,
                    }
            })
            .unwrap_or(usize::MAX)
    }

    /// Build the model of a scope's FROM clause: every binding, then the
    /// joins that merge columns folded in the order they were written.
    fn build_input(&mut self, scope: ScopeId) -> InputModel {
        let graph = self.graph;
        let dialect = graph.dialect;
        let ranges: Vec<RangeRef> = effective_bindings(scope, graph)
            .into_iter()
            .enumerate()
            .map(|(index, (name, binding))| RangeRef {
                index,
                name,
                binding,
            })
            .collect();
        let mut invalid = Vec::new();
        let mut groups = Vec::new();
        for range in &ranges {
            invalid.extend(self.binding_invalid(&range.binding));
            groups.push(Group {
                members: vec![range.name.clone()],
                elements: self.binding_part(range),
            });
        }
        let merges = graph.scopes.join_merges(scope);
        let mut seen = HashSet::new();
        if !merges.is_empty()
            && ranges
                .iter()
                .any(|range| !seen.insert(range.name.column_key(dialect).into_owned()))
        {
            // Join sides are named by range variable, so two ranges with one
            // name leave every merged name without an owner.
            invalid.push(Rc::new(InvalidRanges {
                using: merges
                    .iter()
                    .filter_map(|merge| match &merge.columns {
                        MergedColumns::Using(names) => Some(names.iter().cloned()),
                        MergedColumns::Natural => None,
                    })
                    .flatten()
                    .collect(),
                natural: merges
                    .iter()
                    .any(|merge| matches!(merge.columns, MergedColumns::Natural)),
                candidates: candidates_of(&ranges),
            }));
        }
        let elements = fold_join_columns(groups, merges, dialect);
        let mut anonymous = Vec::new();
        for &child in graph.scopes.anonymous_derived(scope) {
            let model = self.scope(child);
            anonymous.extend(forward_scope(child, &model, &Rc::from([]), false));
        }
        InputModel {
            model: OutputModel::new(elements, Vec::new(), dialect),
            anonymous,
            invalid,
        }
    }

    /// The name conditions a CTE or derived table carries from joins over
    /// duplicated range variables inside it.
    pub(super) fn binding_invalid(&mut self, binding: &Binding) -> Vec<Rc<InvalidRanges>> {
        match binding {
            Binding::Cte(child) | Binding::DerivedTable(child) => {
                self.scope(*child).invalid.clone()
            }
            Binding::Table(_) | Binding::VirtualSource(_) => Vec::new(),
        }
    }
}
