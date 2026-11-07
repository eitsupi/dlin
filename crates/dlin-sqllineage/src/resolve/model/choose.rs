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

use super::input::Invalidity;
use super::{OutputElement, OutputModel};
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
}

/// What a name denotes among the elements of one model.
#[derive(Debug, Clone)]
pub(in crate::resolve) enum Choice {
    /// Nothing that can be listed publishes the name.
    Absent,
    /// One enumerated column: the index of its element.
    Column(usize),
    /// One provider that cannot list its columns: the index of its element.
    Possible(usize),
    Ambiguous(Ambiguity),
}

/// A provider as the policy sees it: the elements of one relation, or one
/// element that several relations merged.
struct Unit {
    /// Matching elements, in output order.
    elements: Vec<usize>,
    /// Whether the provider lists the column, as opposed to possibly hiding it.
    listed: bool,
    candidates: Vec<Candidate>,
    /// Whether a model already found the name ambiguous inside the relation.
    ambiguous: bool,
}

impl OutputModel {
    /// Decide what `name` denotes.
    pub(in crate::resolve) fn choose(&self, name: &Ident) -> Choice {
        if let Some(ambiguity) = self.invalid_ambiguity(name) {
            return Choice::Ambiguous(ambiguity);
        }
        let matches = self.matches(name);
        // Set-operation tails are not weighed: a tail holds the right
        // operand's columns beyond the left operand's, so it competes with
        // nothing the left operand lists. Readers consult it when no provider
        // is found here.
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
            let (listed, ranges, merged) = match &self.elements[index] {
                OutputElement::Column(slot) => (true, &slot.ranges, &slot.candidate_ranges),
                OutputElement::Unknown(segment) => (false, &segment.ranges, &segment.ranges),
                OutputElement::Tail(_) => continue, // never matched
            };
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
                let candidates: Vec<Candidate> = range.candidate().into_iter().collect();
                let unit = match child.choose(name) {
                    Choice::Absent => None,
                    Choice::Ambiguous(inner) => Some(Unit {
                        elements: vec![index],
                        listed,
                        candidates: inner.candidates,
                        ambiguous: true,
                    }),
                    Choice::Column(_) => Some(Unit {
                        elements: vec![index],
                        listed: true,
                        candidates,
                        ambiguous: false,
                    }),
                    Choice::Possible(_) => Some(Unit {
                        elements: vec![index],
                        listed: false,
                        candidates,
                        ambiguous: false,
                    }),
                };
                if let Some(unit) = unit {
                    collapsed.push((range.index, units.len()));
                    units.push(unit);
                    continue;
                }
            }
            // A column several ranges merged is one provider, unless one of
            // those ranges is itself ambiguous about the name.
            let mut candidates = Vec::new();
            let mut ambiguous = false;
            for range in merged.iter() {
                let inner = self
                    .reads
                    .iter()
                    .find(|(at, _)| *at == range.index)
                    .and_then(|(_, child)| match child.choose(name) {
                        Choice::Ambiguous(inner) => Some(inner),
                        _ => None,
                    });
                match inner {
                    Some(inner) => {
                        candidates.extend(inner.candidates);
                        ambiguous = true;
                    }
                    None => candidates.extend(range.candidate()),
                }
            }
            units.push(Unit {
                elements: vec![index],
                listed,
                candidates,
                ambiguous,
            });
        }

        match units.as_slice() {
            [] => Choice::Absent,
            [unit] if !unit.ambiguous => {
                if unit.listed && unit.elements.len() == 1 {
                    Choice::Column(unit.elements[0])
                } else if unit.listed {
                    self.ambiguous(&units)
                } else {
                    Choice::Possible(unit.elements[0])
                }
            }
            _ => self.ambiguous(&units),
        }
    }

    fn ambiguous(&self, units: &[Unit]) -> Choice {
        let mut candidates = Vec::new();
        let mut listed = Vec::new();
        for unit in units {
            candidates.extend(unit.candidates.iter().cloned());
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
        })
    }

    /// Whether a join over duplicated range variables, in this model or the
    /// one it delegates to, leaves `name` without an owner.
    fn invalid_ambiguity(&self, name: &Ident) -> Option<Ambiguity> {
        let dialect = self.names.dialect;
        for invalid in &self.invalid {
            match invalid {
                Invalidity::Own(ranges) if ranges.covers(name, dialect) => {
                    return Some(Ambiguity {
                        candidates: ranges.candidates.clone(),
                        refinable: !ranges.typed(),
                        listed: self.slots_named(name).to_vec(),
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
