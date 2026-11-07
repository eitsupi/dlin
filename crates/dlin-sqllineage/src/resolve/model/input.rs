//! The model of a FROM clause: the relations of a scope with their joins
//! folded, and the lookup of an unqualified name in it.
//!
//! A star expands these elements and applies its own modifiers; a reference to
//! a name reads the same elements through [`InputModel::lookup`]. The join
//! rules that decide which columns exist live in the fold alone, and the one
//! function that weighs independent providers of a name against each other is
//! [`OutputModel::choose`](super::OutputModel::choose).

use std::collections::HashSet;
use std::rc::Rc;

use super::build::{CacheKey, OutputModels, forward_scope};
use super::join::fold_join_columns;
use super::{Choice, Group, OpaqueReason, OutputElement, OutputModel, ValueId};
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

/// Why a model cannot attribute some names to one relation.
#[derive(Debug, Clone)]
pub(in crate::resolve) enum Invalidity {
    /// A join of this model's own FROM clause runs over duplicated range
    /// variables.
    Own(Rc<InvalidRanges>),
    /// The output is that of another model, which may have such a join.
    Whole(Rc<OutputModel>),
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
    /// A CTE or derived table is the only provider of the name, or the only
    /// relation of the clause: it answers for it.
    Scope { scope: ScopeId, range: RangeRef },
    /// A lone physical relation whose columns the catalog does not list.
    Physical(Rc<TableRef>),
    /// Several relations may carry the name, or several columns do. `typed`
    /// is set when a catalog must not narrow the candidates down. `values`
    /// are the listed columns among the providers, which decide the
    /// transform of a value read through the ambiguity.
    Ambiguous {
        candidates: Vec<Candidate>,
        typed: bool,
        values: Vec<ValueId>,
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

fn candidates_of<'r>(ranges: impl IntoIterator<Item = &'r RangeRef>) -> Vec<Candidate> {
    ranges.into_iter().filter_map(RangeRef::candidate).collect()
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
    fn lookup(&self, name: &Ident) -> InputLookup {
        let elements = &self.model.elements;
        match self.model.choose(name) {
            Choice::Absent => InputLookup::Absent,
            Choice::Column(index) => match &elements[index] {
                OutputElement::Column(slot) => InputLookup::Column {
                    value: slot.value.clone(),
                    contributing: slot.candidate_ranges.clone(),
                },
                _ => unreachable!("a chosen column is listed"),
            },
            Choice::Possible(index) => match &elements[index] {
                OutputElement::Unknown(segment) => segment
                    .ranges
                    .first()
                    .map_or(InputLookup::Absent, InputLookup::single),
                _ => unreachable!("a possible provider is an unknown segment"),
            },
            Choice::Ambiguous(ambiguity) => InputLookup::Ambiguous {
                typed: !ambiguity.refinable,
                values: ambiguity
                    .listed
                    .iter()
                    .filter_map(|&index| match &elements[index] {
                        OutputElement::Column(slot) => Some(slot.value.clone()),
                        _ => None,
                    })
                    .collect(),
                candidates: ambiguity.candidates,
            },
        }
    }
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
            _ => self.input(scope).lookup(name),
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
        let mut reads = Vec::new();
        let mut groups = Vec::new();
        for range in &ranges {
            if let Some(model) = self.binding_model(&range.binding) {
                reads.push((range.index, model));
            }
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
            invalid.push(Invalidity::Own(Rc::new(InvalidRanges {
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
            })));
        }
        let elements = fold_join_columns(groups, merges, dialect);
        let mut anonymous = Vec::new();
        for &child in graph.scopes.anonymous_derived(scope) {
            let model = self.scope(child);
            anonymous.extend(forward_scope(child, &model, &Rc::from([]), false));
        }
        let mut model = OutputModel::new(elements, Vec::new(), dialect);
        model.invalid = invalid;
        model.reads = reads;
        InputModel { model, anonymous }
    }

    /// The model of the CTE or derived table a binding reads, if it is one.
    pub(super) fn binding_model(&mut self, binding: &Binding) -> Option<Rc<OutputModel>> {
        match binding {
            Binding::Cte(child) | Binding::DerivedTable(child) => Some(self.scope(*child)),
            Binding::Table(_) | Binding::VirtualSource(_) => None,
        }
    }
}
