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
    /// Whether a model already found the name ambiguous inside the relation.
    ambiguous: bool,
}

impl OutputModel {
    /// Decide what `name` denotes.
    pub(in crate::resolve) fn choose(&self, name: &Ident) -> Choice<'_> {
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
            let (ranges, merged) = match &self.elements[index] {
                OutputElement::Column(slot) => (&slot.ranges, &slot.candidate_ranges),
                OutputElement::Unknown(segment) => (&segment.ranges, &segment.ranges),
                OutputElement::Tail(_) => continue, // never matched
            };
            let asked = asked_name(&self.elements[index], name);
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
                let own = || Unit {
                    elements: vec![index],
                    candidates: range.candidate().into_iter().collect(),
                    ambiguous: false,
                };
                let unit = match asked.map(|asked| child.choose(asked)) {
                    None => Some(own()),
                    Some(Choice::Absent) => None,
                    Some(Choice::Ambiguous(inner)) => Some(Unit {
                        elements: vec![index],
                        candidates: inner.candidates,
                        ambiguous: true,
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
            let mut candidates = Vec::new();
            let mut ambiguous = false;
            for range in merged.iter() {
                let inner = self
                    .reads
                    .iter()
                    .find(|(at, _)| *at == range.index)
                    .zip(asked)
                    .and_then(|((_, child), asked)| match child.choose(asked) {
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
                candidates,
                ambiguous,
            });
        }

        match units.as_slice() {
            [] => Choice::Absent,
            [unit] if !unit.ambiguous => {
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
