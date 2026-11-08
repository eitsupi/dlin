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
use crate::catalog::ColumnPresence;
use crate::graph::scope::{Binding, BindingEnv, MergedColumns, ScopeId};
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
#[derive(Debug, Clone)]
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
    fn standing(&self, name: &Ident) -> Standing {
        let elements = &self.model.elements;
        match self.model.choose(name) {
            Choice::Absent => Standing::Absent,
            Choice::Column(_, slot) => Standing::Present(InputLookup::Column {
                value: slot.value.clone(),
                contributing: slot.candidate_ranges.clone(),
            }),
            Choice::Possible(segment) => {
                segment
                    .ranges
                    .first()
                    .map_or(Standing::Absent, |first| Standing::Possible {
                        answer: InputLookup::single(first),
                        candidates: candidates_of(segment.ranges.iter()),
                    })
            }
            Choice::Ambiguous(ambiguity) => Standing::Present(InputLookup::Ambiguous {
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
            }),
        }
    }
}

/// What one FROM clause says about an unqualified name.
enum Standing {
    /// No relation of the clause can be shown to publish the name.
    Absent,
    /// The clause settles the name, to a column or to an ambiguity inside it.
    Present(InputLookup),
    /// Exactly one relation of the clause may publish the name, but its
    /// columns are not listed, so nothing proves it does. `answer` is what
    /// the name denotes if the clause is all there is; `candidates` are the
    /// relations that may carry it.
    Possible {
        answer: InputLookup,
        candidates: Vec<Candidate>,
    },
}

/// What an unqualified name denotes after the clauses of the scopes around the
/// reference have been weighed, nearest first.
enum Walk {
    Absent,
    Present(InputLookup),
    Possible {
        answer: InputLookup,
        candidates: Vec<Candidate>,
    },
    /// Relations of different clauses may each carry the name, and the
    /// nearer ones cannot be shown to hide the farther ones.
    Several {
        answer: InputLookup,
        candidates: Vec<Candidate>,
        values: Vec<ValueId>,
    },
}

impl Walk {
    /// Add the nearer clause's possible providers in front of what the
    /// farther clauses answered.
    fn behind(answer: InputLookup, mut candidates: Vec<Candidate>, farther: Self) -> Self {
        let mut values = Vec::new();
        match farther {
            Self::Absent => return Self::Possible { answer, candidates },
            Self::Present(lookup) => {
                let (more, listed) = providers_of(&lookup);
                candidates.extend(more);
                values.extend(listed);
            }
            Self::Possible {
                candidates: more, ..
            } => candidates.extend(more),
            Self::Several {
                candidates: more,
                values: listed,
                ..
            } => {
                candidates.extend(more);
                values.extend(listed);
            }
        }
        Self::Several {
            answer,
            candidates,
            values,
        }
    }

    fn into_lookup(self) -> Option<InputLookup> {
        match self {
            Self::Absent => None,
            Self::Present(lookup) | Self::Possible { answer: lookup, .. } => Some(lookup),
            Self::Several {
                answer,
                mut candidates,
                values,
            } => {
                let mut seen = Vec::new();
                candidates.retain(|candidate| {
                    let new = !seen.contains(candidate);
                    if new {
                        seen.push(candidate.clone());
                    }
                    new
                });
                // The same physical relation at both levels supplies the
                // same column either way.
                if let [Candidate::Relation(_)] = candidates.as_slice() {
                    return Some(answer);
                }
                Some(InputLookup::Ambiguous {
                    candidates,
                    // The clauses were weighed against the catalog already,
                    // which must not narrow the candidates down again.
                    typed: true,
                    values,
                })
            }
        }
    }
}

fn table_candidate(table: &TableRef) -> Candidate {
    Candidate::Relation(Relation {
        catalog: table.catalog.clone(),
        schema: table.schema.clone(),
        name: table.table.clone(),
    })
}

/// The relations a lookup answer reads from, and the listed columns among
/// them.
fn providers_of(lookup: &InputLookup) -> (Vec<Candidate>, Vec<ValueId>) {
    match lookup {
        InputLookup::Absent | InputLookup::NoSource => (Vec::new(), Vec::new()),
        InputLookup::Column {
            value,
            contributing,
        } => (candidates_of(contributing.iter()), vec![value.clone()]),
        InputLookup::Scope { range, .. } => (candidates_of([range]), Vec::new()),
        InputLookup::Physical(table) => (vec![table_candidate(table)], Vec::new()),
        InputLookup::Ambiguous {
            candidates, values, ..
        } => (candidates.clone(), values.clone()),
    }
}

impl OutputModels<'_, '_, '_> {
    /// The relations of a scope's FROM clause with their joins folded. This is
    /// what an unqualified star expands and what an unqualified name is read
    /// from; it is not the scope's output, which is projected from it.
    pub(in crate::resolve) fn input(&mut self, env: BindingEnv) -> Rc<InputModel> {
        // Environments that see the same range variables share one model.
        let env = self.graph.scopes.clause_env(env);
        if let Some(model) = self.store.inputs.get(&env) {
            return model.clone();
        }
        if self.enter(CacheKey::Input(env)) {
            return Rc::new(InputModel {
                model: OutputModel::opaque(OpaqueReason::Recursive, self.graph.dialect),
                anonymous: Vec::new(),
            });
        }
        let model = Rc::new(self.build_input(env));
        if self.leave() {
            self.store.inputs.insert(env, model.clone());
        }
        model
    }

    /// Decide what the unqualified `name` denotes at `env`.
    ///
    /// The FROM clauses around the reference are weighed one at a time, the
    /// innermost first, and a clause is never pooled with another. A clause
    /// that provides the name settles it, and so does an ambiguity inside it.
    /// A clause that cannot be shown to publish it leaves the name to the
    /// clauses around. A clause that may publish it, because it reads a
    /// relation whose columns are not listed, competes with whatever the
    /// clauses around provide. When no clause provides the name at all, it is
    /// the innermost clause's: a clause with a single relation answers for any
    /// name.
    pub(in crate::resolve) fn lookup_input(
        &mut self,
        env: BindingEnv,
        name: &Ident,
    ) -> InputLookup {
        let clauses = self.graph.scopes.clause_envs(env);
        let Some((&first, rest)) = clauses.split_first() else {
            return InputLookup::Absent;
        };
        let (standing, fallback) = self.clause_standing(first, name);
        let walk = self.settle(standing, rest, name);
        walk.into_lookup().unwrap_or(fallback)
    }

    fn settle(&mut self, standing: Standing, farther: &[BindingEnv], name: &Ident) -> Walk {
        match standing {
            Standing::Present(lookup) => Walk::Present(lookup),
            Standing::Absent => self.walk_outward(farther, name),
            Standing::Possible { answer, candidates } => {
                let outer = self.walk_outward(farther, name);
                Walk::behind(answer, candidates, outer)
            }
        }
    }

    fn walk_outward(&mut self, clauses: &[BindingEnv], name: &Ident) -> Walk {
        let Some((&next, rest)) = clauses.split_first() else {
            return Walk::Absent;
        };
        let (standing, _) = self.clause_standing(next, name);
        self.settle(standing, rest, name)
    }

    /// What the FROM clause `env` says about `name`, with the answer the name
    /// gets from that clause when nothing provides it anywhere. A clause with
    /// a single relation needs no model of its own: the name is that
    /// relation's.
    fn clause_standing(&mut self, env: BindingEnv, name: &Ident) -> (Standing, InputLookup) {
        let bindings = effective_bindings(env, self.graph);
        match bindings.as_slice() {
            [] => (Standing::Absent, InputLookup::Absent),
            [(range_name, binding)] => {
                let range = RangeRef {
                    index: 0,
                    name: range_name.clone(),
                    binding: binding.clone(),
                };
                let single = InputLookup::single(&range);
                let standing = match binding {
                    Binding::Table(table) => {
                        match self
                            .catalog
                            .map(|catalog| catalog.column_presence(table, name))
                        {
                            Some(ColumnPresence::Present(_)) => Standing::Present(single.clone()),
                            // The catalog lists the name twice: the relation
                            // has the column, but a reference cannot tell
                            // which one it means.
                            Some(ColumnPresence::Duplicate) => {
                                Standing::Present(InputLookup::Ambiguous {
                                    candidates: candidates_of([&range]),
                                    typed: true,
                                    values: Vec::new(),
                                })
                            }
                            Some(ColumnPresence::Absent) => Standing::Absent,
                            Some(ColumnPresence::Possible) | None => Standing::Possible {
                                answer: single.clone(),
                                candidates: candidates_of([&range]),
                            },
                        }
                    }
                    Binding::Cte(child) | Binding::DerivedTable(child) => {
                        let model = self.scope(*child);
                        match model.choose(name) {
                            Choice::Column(..) | Choice::Ambiguous(_) => {
                                Standing::Present(single.clone())
                            }
                            Choice::Possible(_) => Standing::Possible {
                                answer: single.clone(),
                                candidates: candidates_of([&range]),
                            },
                            // Columns the analysis cannot list may still
                            // carry the name.
                            Choice::Absent if model.has_unknown_segment() => Standing::Possible {
                                answer: single.clone(),
                                candidates: candidates_of([&range]),
                            },
                            Choice::Absent => Standing::Absent,
                        }
                    }
                    Binding::VirtualSource(_) => Standing::Absent,
                };
                (standing, single)
            }
            _ => (self.input(env).standing(name), InputLookup::Absent),
        }
    }

    /// The position of a binding among the relations of a FROM clause. A
    /// binding of an enclosing query, which the clause does not contain, has
    /// none.
    pub(super) fn range_index(&self, env: BindingEnv, name: &Ident, binding: &Binding) -> usize {
        let dialect = self.graph.dialect;
        effective_bindings(env, self.graph)
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
    fn build_input(&mut self, env: BindingEnv) -> InputModel {
        let graph = self.graph;
        let dialect = graph.dialect;
        let ranges: Vec<RangeRef> = effective_bindings(env, graph)
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
        let merges = graph.scopes.join_merges(env);
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
        for child in graph.scopes.anonymous_derived(env) {
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
