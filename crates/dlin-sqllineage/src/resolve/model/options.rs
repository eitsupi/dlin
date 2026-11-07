//! Star modifiers (EXCLUDE, ILIKE, REPLACE, RENAME) applied to the elements of
//! a star's model.

use std::rc::Rc;

use super::super::target::{excluded_matches_name, ilike_matches, same_column_name};
use super::{ColumnSlot, OutputElement, RangeRef, StarFilter, ValueId, unknown_publishers};
use crate::graph::node::{StarBase, StarOptions};
use crate::graph::scope::ScopeId;
use crate::{Dialect, Ident, OutputName};

/// Apply EXCLUDE/ILIKE, then REPLACE, then RENAME in list order to the
/// current names. Columns that are not listed can only be restricted: their
/// domain records the filters, and a REPLACE or RENAME that names one makes it
/// an explicit column of its own.
pub(super) fn apply_options(
    base: &StarBase,
    options: &StarOptions,
    elements: Vec<OutputElement>,
    dialect: Dialect,
) -> Vec<OutputElement> {
    let admits = |name: &Ident, ranges: &[RangeRef]| {
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
    let mut elements: Vec<OutputElement> = elements
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

    for replacement in &options.replace {
        // A REPLACE target names one column of the star, so no unknown segment
        // publishes another column under that name next to it.
        if let Some(slot) = first_slot_named(&mut elements, &replacement.column, dialect) {
            slot.value = ValueId::Expr(replacement.node_id);
            remove_from_segments(&mut elements, &replacement.column, dialect);
            continue;
        }
        let (providers, mut ranges) = unknown_publishers(&elements, &replacement.column, dialect);
        let tails = tail_publishers(&elements, &replacement.column, dialect);
        if providers.is_empty() && tails.is_empty() {
            continue;
        }
        // The replacement stands for the column of that name in the segments
        // and the tail it replaces.
        remove_from_segments(&mut elements, &replacement.column, dialect);
        retire_tail_name(&mut elements, &replacement.column, dialect);
        ranges.extend(tails.into_iter().flat_map(|(_, ranges)| ranges.to_vec()));
        let ranges: Rc<[RangeRef]> = ranges.into();
        elements.push(OutputElement::Column(ColumnSlot {
            name: replacement.column.clone(),
            output_name: OutputName::Explicit(replacement.column.clone()),
            ranges: ranges.clone(),
            candidate_ranges: ranges,
            value: ValueId::Expr(replacement.node_id),
        }));
    }

    for (old, new) in &options.rename {
        if let Some(slot) = first_slot_named(&mut elements, old, dialect) {
            slot.name.clone_from(new);
            slot.output_name = OutputName::Explicit(new.clone());
            continue;
        }
        let (providers, mut ranges) = unknown_publishers(&elements, old, dialect);
        let tails = tail_publishers(&elements, old, dialect);
        if providers.is_empty() && tails.is_empty() {
            continue;
        }
        remove_from_segments(&mut elements, old, dialect);
        retire_tail_name(&mut elements, old, dialect);
        // The renamed column carries what every publisher of the old name
        // contributes: the unknown segments and the set-operation tails.
        let mut values = Vec::new();
        if !providers.is_empty() {
            values.push(ValueId::Named {
                providers,
                column: old.clone(),
            });
        }
        for (scope, tail_ranges) in tails {
            ranges.extend(tail_ranges.iter().cloned());
            values.push(ValueId::Tail {
                scope,
                column: old.clone(),
            });
        }
        let value = values
            .into_iter()
            .reduce(|left, right| ValueId::Merged(Box::new(left), Box::new(right)))
            .expect("a renamed column has a publisher");
        let ranges: Rc<[RangeRef]> = ranges.into();
        elements.push(OutputElement::Column(ColumnSlot {
            name: new.clone(),
            output_name: OutputName::Explicit(new.clone()),
            ranges: ranges.clone(),
            candidate_ranges: ranges,
            value,
        }));
    }
    // A name a RENAME introduces does not collide with a column the star
    // already has, so no unknown segment publishes it next to the renamed
    // column. A name that another item renames away is the exception: the
    // segment's own column of that name is that item's.
    for (_, new) in &options.rename {
        if !options
            .rename
            .iter()
            .any(|(old, _)| same_column_name(old, new, dialect))
        {
            remove_from_segments(&mut elements, new, dialect);
        }
    }
    elements
}

/// Stop the unknown segments from publishing `name`: a column of the star
/// stands for it.
fn remove_from_segments(elements: &mut [OutputElement], name: &Ident, dialect: Dialect) {
    for element in elements {
        if let OutputElement::Unknown(segment) = element
            && segment.domain.admits(name, dialect)
        {
            segment.domain.removed.push(name.clone());
        }
    }
}

/// The set-operation tails that may publish `name`, with their range variables.
fn tail_publishers(
    elements: &[OutputElement],
    name: &Ident,
    dialect: Dialect,
) -> Vec<(ScopeId, Rc<[RangeRef]>)> {
    elements
        .iter()
        .filter_map(|element| match element {
            OutputElement::Tail(tail) if tail.domain.admits(name, dialect) => {
                Some((tail.scope, tail.ranges.clone()))
            }
            _ => None,
        })
        .collect()
}

/// A column that replaced or renamed a tail's column of the same name is the
/// only one left under that name.
fn retire_tail_name(elements: &mut [OutputElement], name: &Ident, dialect: Dialect) {
    for element in elements {
        if let OutputElement::Tail(tail) = element
            && tail.domain.admits(name, dialect)
        {
            tail.domain.removed.push(name.clone());
        }
    }
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
