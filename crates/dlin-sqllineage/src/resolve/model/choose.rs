//! The one rule that decides what a name denotes among the relations that may
//! publish it.
//!
//! A name denotes a column only when exactly one provider can be shown to
//! publish it. Providers are the enumerated columns that carry the name and the
//! unknown segments whose name conditions still admit it; a relation whose
//! catalog columns lack the name, or a CTE or derived table that lists no such
//! column and has no unknown segment, is not a provider. Several providers make
//! the name ambiguous, whichever of them list the column and whichever may
//! merely hide it behind a star.
//!
//! The same function answers a reference in a FROM clause and a reference to a
//! column of a scope, because both read a model's elements: a scope that reads a
//! CTE or derived table keeps that model, and the elements the reader sees under
//! one range variable are weighed by the model they came from. A competition
//! inside a CTE therefore survives any number of `SELECT *` wrappers.
//!
//! The model is asked under the name it knows the element by, which is not
//! always the name the reader publishes it under. A listed column carries that
//! relation as its [`SlotOrigin`]: a column forwarded under another name (a
//! RENAME) asks for the old name, and a column the reader defines (a REPLACE)
//! does not ask at all, because the model's competition over the name concerns
//! a different value. Unknown segments are asked under the name being chosen,
//! since a RENAME only removes names from their domain.

use super::input::Invalidity;
use super::{ColumnSlot, OutputElement, OutputModel, SlotOrigin, UnknownSegment};
use crate::{Candidate, Ident};

/// The relations that may own a name that has no single owner.
#[derive(Debug, Clone)]
pub(in crate::resolve) struct Ambiguity {
    pub candidates: Vec<Candidate>,
    /// Whether a catalog may still narrow the candidates down: they are all
    /// physical relations of one join over duplicated range variables.
    pub refinable: bool,
    /// The enumerated columns among the providers, which decide the transform
    /// of a value that is read through the ambiguity.
    pub listed: Vec<usize>,
    /// Whether the name has no single owner whatever the columns that are not
    /// known turn out to be: a join over duplicated range variables, or a
    /// provider that is ambiguous about the name itself. Otherwise the
    /// providers compete, and only `present` are known to publish the name.
    pub certain: bool,
    /// The candidates known to publish the name.
    pub present: Vec<Candidate>,
    /// How many providers are known to publish the name. Two make the name
    /// ambiguous whatever the others carry.
    pub known: usize,
    /// Whether one of them is a column the model defines itself, which has
    /// no range variable to name it: the scope that is read stands for it.
    pub defined: bool,
}

/// What a name denotes among the elements of one model.
///
/// The kind of the answer is the kind of the element it points at: `Column`
/// carries a listed column and `Possible` an unknown segment, whatever a model
/// the element was forwarded from found out about the name. Only
/// [`OutputModel::choose`] builds them, from the element itself.
#[derive(Debug, Clone)]
pub(in crate::resolve) enum Choice<'m> {
    /// Nothing that can be listed publishes the name.
    Absent,
    /// One enumerated column: the index of its element and the column.
    Column(usize, &'m ColumnSlot),
    /// One provider that cannot list its columns: its segment.
    Possible(&'m UnknownSegment),
    Ambiguous(Ambiguity),
}

/// A provider as the policy sees it: the elements of one relation, or one
/// element that several relations merged.
struct Unit {
    /// Matching elements, in output order.
    elements: Vec<usize>,
    candidates: Vec<Candidate>,
    /// The candidates among them known to publish the name.
    present: Vec<Candidate>,
    /// Whether the provider is known to publish the name.
    known: bool,
    /// Whether a model already found the name ambiguous inside the relation.
    ambiguous: bool,
    /// Whether a model found the name inside the relation left to columns
    /// that are not known.
    uncertain: bool,
}

impl Unit {
    /// One provider made of `index`, known to publish the name when the
    /// element is a listed column.
    fn own(index: usize, candidates: Vec<Candidate>, listed: bool) -> Self {
        Self {
            elements: vec![index],
            present: if listed {
                candidates.clone()
            } else {
                Vec::new()
            },
            candidates,
            known: listed,
            ambiguous: false,
            uncertain: false,
        }
    }
}

impl OutputModel {
    /// Decide what `name` denotes.
    pub(in crate::resolve) fn choose(&self, name: &Ident) -> Choice<'_> {
        if let Some(ambiguity) = self.invalid_ambiguity(name) {
            return Choice::Ambiguous(ambiguity);
        }
        let matches = self.matches(name);
        let mut indexes: Vec<usize> = matches
            .listed
            .iter()
            .chain(&matches.unknown)
            .copied()
            .collect();
        indexes.sort_unstable();

        let mut units: Vec<Unit> = Vec::new();
        let mut collapsed: Vec<(usize, usize)> = Vec::new();
        for index in indexes {
            let (ranges, merged) = match &self.elements[index] {
                OutputElement::Column(slot) => (&slot.ranges, &slot.candidate_ranges),
                OutputElement::Unknown(segment) => (&segment.ranges, &segment.ranges),
            };
            let asked = asked_name(&self.elements[index], name);
            let listed = matches!(self.elements[index], OutputElement::Column(_));
            // The elements of a CTE or derived table are weighed by the model
            // they were forwarded from, so one relation is one provider no
            // matter how many elements it contributes.
            if let [range] = &**ranges
                && let Some((_, child)) = self.reads.iter().find(|(at, _)| *at == range.index)
            {
                if let Some(&(_, at)) = collapsed.iter().find(|(owner, _)| *owner == range.index) {
                    units[at].elements.push(index);
                    continue;
                }
                // The model the elements came from only says whether the
                // range provides the name at all and whether it is ambiguous
                // inside; what the reader's own element is, a listed column or
                // a segment, is the reader's. An element the reader defines
                // asks nothing and is its own provider.
                let own = || Unit::own(index, range.candidate().into_iter().collect(), listed);
                let unit = match asked.map(|asked| child.choose(asked)) {
                    None => Some(own()),
                    Some(Choice::Absent) => None,
                    Some(Choice::Ambiguous(inner)) => Some(match child.unverified(&inner) {
                        // The relation leaves the name to columns it does not
                        // know, and so does its reader. A column it defines
                        // itself is read as the range's.
                        Some((mut present, possible)) => Unit {
                            elements: vec![index],
                            candidates: {
                                if inner.defined {
                                    present.extend(range.candidate());
                                }
                                present.iter().chain(&possible).cloned().collect()
                            },
                            known: !present.is_empty(),
                            present,
                            ambiguous: false,
                            uncertain: true,
                        },
                        None => Unit {
                            elements: vec![index],
                            candidates: inner.candidates,
                            present: Vec::new(),
                            known: false,
                            ambiguous: true,
                            uncertain: false,
                        },
                    }),
                    Some(Choice::Column(..) | Choice::Possible(..)) => Some(own()),
                };
                if let Some(unit) = unit {
                    collapsed.push((range.index, units.len()));
                    units.push(unit);
                    continue;
                }
            }
            // A column several ranges merged is one provider, unless one of
            // those ranges is itself ambiguous about the name.
            let mut unit = Unit::own(index, Vec::new(), listed);
            for range in merged.iter() {
                let inner = self
                    .reads
                    .iter()
                    .find(|(at, _)| *at == range.index)
                    .zip(asked)
                    .and_then(|((_, child), asked)| match child.choose(asked) {
                        Choice::Ambiguous(inner) => Some((child, inner)),
                        _ => None,
                    });
                let Some((child, inner)) = inner else {
                    let candidate = range.candidate();
                    if listed {
                        unit.present.extend(candidate.clone());
                    }
                    unit.candidates.extend(candidate);
                    continue;
                };
                if let Some((mut present, possible)) = child.unverified(&inner) {
                    if inner.defined {
                        present.extend(range.candidate());
                    }
                    unit.candidates
                        .extend(present.iter().chain(&possible).cloned());
                    unit.present.extend(present);
                    unit.uncertain = true;
                } else {
                    unit.candidates.extend(inner.candidates);
                    unit.ambiguous = true;
                }
            }
            if unit.uncertain {
                unit.known = !unit.present.is_empty();
            }
            units.push(unit);
        }

        match units.as_slice() {
            [] => Choice::Absent,
            [unit] if !unit.ambiguous && !unit.uncertain => {
                self.single(unit).unwrap_or_else(|| self.ambiguous(&units))
            }
            _ => self.ambiguous(&units),
        }
    }

    /// The answer when one provider is left, taken from its elements: a lone
    /// listed column, or segments that only may publish the name. `None` when
    /// the provider has several columns or mixes columns with segments, which
    /// is no single answer.
    fn single(&self, unit: &Unit) -> Option<Choice<'_>> {
        let (&first, rest) = unit.elements.split_first()?;
        match &self.elements[first] {
            OutputElement::Column(slot) if rest.is_empty() => Some(Choice::Column(first, slot)),
            OutputElement::Unknown(segment)
                if rest
                    .iter()
                    .all(|&index| matches!(self.elements[index], OutputElement::Unknown(_))) =>
            {
                Some(Choice::Possible(segment))
            }
            _ => None,
        }
    }

    fn ambiguous(&self, units: &[Unit]) -> Choice<'_> {
        let mut candidates = Vec::new();
        let mut listed = Vec::new();
        let mut present: Vec<Candidate> = Vec::new();
        let certain = units.iter().any(|unit| unit.ambiguous);
        let known = units.iter().filter(|unit| unit.known).count();
        let defined = units
            .iter()
            .any(|unit| unit.known && unit.candidates.is_empty());
        for unit in units {
            candidates.extend(unit.candidates.iter().cloned());
            for candidate in &unit.present {
                if !present.contains(candidate) {
                    present.push(candidate.clone());
                }
            }
            listed.extend(
                unit.elements
                    .iter()
                    .copied()
                    .filter(|&index| matches!(self.elements[index], OutputElement::Column(_))),
            );
        }
        Choice::Ambiguous(Ambiguity {
            candidates,
            refinable: false,
            listed,
            certain,
            present,
            known,
            defined,
        })
    }

    /// Whether a join over duplicated range variables, in this model or the
    /// one it delegates to, leaves `name` without an owner.
    fn invalid_ambiguity(&self, name: &Ident) -> Option<Ambiguity> {
        let dialect = self.names.dialect;
        for invalid in &self.invalid {
            match invalid {
                Invalidity::Own(ranges) if ranges.covers(name, dialect) => {
                    // Only the relations that can carry the name are
                    // candidates; the others are in the clause but cannot be
                    // what the name means.
                    let mut candidates = self.possible_providers(name);
                    if candidates.is_empty() {
                        candidates.clone_from(&ranges.candidates);
                    }
                    return Some(Ambiguity {
                        candidates,
                        refinable: !ranges.typed(),
                        listed: self.slots_named(name).to_vec(),
                        certain: true,
                        present: Vec::new(),
                        known: 0,
                        defined: false,
                    });
                }
                Invalidity::Whole(model) => {
                    if let Choice::Ambiguous(ambiguity) = model.choose(name) {
                        return Some(ambiguity);
                    }
                }
                Invalidity::Own(_) => {}
            }
        }
        None
    }
}

impl OutputModel {
    /// The candidates that are known to publish a name and those that only
    /// may, when `ambiguity` leaves the name to columns that are not known:
    /// at most one listed column publishes it and some provider may.
    ///
    /// Two providers known to publish it make it ambiguous whatever the
    /// others carry, even one relation read twice, and a column that a join
    /// merges is one provider. So does a structure that has no single owner for the
    /// name: a join over duplicated range variables, a provider ambiguous
    /// about it, or two ranges that share a name, which are alternative
    /// readings of one FROM item.
    pub(in crate::resolve) fn unverified(
        &self,
        ambiguity: &Ambiguity,
    ) -> Option<(Vec<Candidate>, Vec<Candidate>)> {
        if ambiguity.certain || ambiguity.known > 1 || self.has_alternative_ranges() {
            return None;
        }
        let present = ambiguity.present.clone();
        let mut possible: Vec<Candidate> = Vec::new();
        for candidate in &ambiguity.candidates {
            if !present.contains(candidate) && !possible.contains(candidate) {
                possible.push(candidate.clone());
            }
        }
        (!possible.is_empty()).then_some((present, possible))
    }

    /// Whether two ranges share a name.
    fn has_alternative_ranges(&self) -> bool {
        let dialect = self.names.dialect;
        let mut seen: Vec<(usize, String)> = Vec::new();
        for element in &self.elements {
            let ranges = match element {
                OutputElement::Column(slot) => &slot.ranges,
                OutputElement::Unknown(segment) => &segment.ranges,
            };
            for range in ranges.iter().filter(|range| !range.anonymous) {
                let key = range.name.column_key(dialect).into_owned();
                if seen
                    .iter()
                    .any(|(index, name)| *index != range.index && *name == key)
                {
                    return true;
                }
                seen.push((range.index, key));
            }
        }
        false
    }

    /// The relations of the FROM clause that may carry `name`: the ranges of
    /// the columns that list it and of the unknown segments that can still
    /// publish it.
    fn possible_providers(&self, name: &Ident) -> Vec<Candidate> {
        let matches = self.matches(name);
        let mut seen = Vec::new();
        let mut candidates = Vec::new();
        for &index in matches.listed.iter().chain(&matches.unknown) {
            let ranges = match &self.elements[index] {
                OutputElement::Column(slot) => &slot.ranges,
                OutputElement::Unknown(segment) => &segment.ranges,
            };
            for range in ranges.iter() {
                if !seen.contains(&range.index) {
                    seen.push(range.index);
                    candidates.extend(range.candidate());
                }
            }
        }
        candidates
    }
}

/// The name to ask the model an element was forwarded from about, when
/// `name` is what the reader looks for. `None` for a column the reader
/// defines: the model's view of the name does not apply to it.
fn asked_name<'n>(element: &'n OutputElement, name: &'n Ident) -> Option<&'n Ident> {
    match element {
        OutputElement::Column(slot) => match &slot.origin {
            SlotOrigin::Forwarded(forwarded) => Some(forwarded),
            SlotOrigin::Defined => None,
        },
        _ => Some(name),
    }
}
