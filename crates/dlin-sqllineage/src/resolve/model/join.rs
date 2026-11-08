//! Folding of USING and NATURAL joins over the output of the relations a join
//! combined.

use std::collections::HashSet;
use std::rc::Rc;

use super::super::target::same_column_name;
use super::{
    ColumnSlot, Group, OutputElement, RangeRef, SlotOrigin, UnknownProvider, ValueId,
    unknown_publishers,
};
use crate::graph::scope::{JoinMerge, JoinMergeKind, MergedColumns};
use crate::{Dialect, Ident, OutputName, Relation};

fn union_ranges(left: &[RangeRef], right: &[RangeRef]) -> Rc<[RangeRef]> {
    left.iter().chain(right).cloned().collect()
}

struct NaturalUncertainty {
    known_on_left: bool,
    name: Ident,
    relations: Vec<Relation>,
}

fn natural_uncertainties(
    left: &[OutputElement],
    right: &[OutputElement],
    dialect: Dialect,
) -> Vec<NaturalUncertainty> {
    let mut uncertain = Vec::new();
    for (known, possible, known_on_left) in [(left, right, true), (right, left, false)] {
        for element in known {
            let OutputElement::Column(slot) = element else {
                continue;
            };
            let listed_other = possible.iter().any(|other| {
                matches!(other, OutputElement::Column(other)
                    if same_column_name(&other.name, &slot.name, dialect))
            });
            if listed_other {
                continue;
            }
            let (publishers, _) = unknown_publishers(possible, &slot.name, dialect);
            if publishers.is_empty() {
                continue;
            }
            let relations = possible
                .iter()
                .filter_map(|element| match element {
                    OutputElement::Unknown(segment)
                        if segment.domain.admits(&slot.name, dialect) =>
                    {
                        Some(segment.schema_relations.iter().cloned())
                    }
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>();
            uncertain.push(NaturalUncertainty {
                known_on_left,
                name: slot.name.clone(),
                relations,
            });
        }
    }
    uncertain
}

fn apply_natural_uncertainties(
    uncertainties: Vec<NaturalUncertainty>,
    left: &mut [OutputElement],
    right: &mut [OutputElement],
    kind: JoinMergeKind,
    dialect: Dialect,
) {
    for uncertainty in uncertainties {
        let (known, possible) = if uncertainty.known_on_left {
            (&mut *left, &mut *right)
        } else {
            (&mut *right, &mut *left)
        };
        for element in possible.iter_mut() {
            if let OutputElement::Unknown(segment) = element
                && segment.domain.admits(&uncertainty.name, dialect)
            {
                segment.domain.removed.push(uncertainty.name.clone());
            }
        }
        for element in known.iter_mut() {
            let OutputElement::Column(slot) = element else {
                continue;
            };
            if same_column_name(&slot.name, &uncertainty.name, dialect) {
                let preserved = matches!(
                    (uncertainty.known_on_left, kind),
                    (true, JoinMergeKind::Left) | (false, JoinMergeKind::Right)
                );
                if !preserved {
                    let suppressed = matches!(
                        (uncertainty.known_on_left, kind),
                        (true, JoinMergeKind::Right) | (false, JoinMergeKind::Left)
                    );
                    slot.value = ValueId::SchemaDependent {
                        value: (!suppressed).then(|| Box::new(slot.value.clone())),
                        relations: uncertainty.relations.clone(),
                    };
                }
            }
        }
    }
}

/// Fold USING and NATURAL joins in the order they were written. A shape the
/// fold cannot make sense of (a missing or ambiguous key, relations that
/// share a name) leaves every relation's columns as they are.
pub(super) fn fold_join_columns(
    groups: Vec<Group>,
    merges: &[JoinMerge],
    dialect: Dialect,
) -> Vec<OutputElement> {
    if merges.is_empty() {
        return concat_groups(groups);
    }
    let original = groups.clone();
    fold_groups(groups, merges, dialect).map_or_else(|| concat_groups(original), concat_groups)
}

fn concat_groups(mut groups: Vec<Group>) -> Vec<OutputElement> {
    let mut prefix = Some(0usize);
    let mut output = Vec::new();
    for group in &mut groups {
        for element in &mut group.elements {
            match element {
                OutputElement::Column(slot) => {
                    slot.ordinal = slot
                        .ordinal
                        .and_then(|ordinal| prefix.and_then(|prefix| prefix.checked_add(ordinal)));
                }
                OutputElement::Unknown(segment) => {
                    segment.position = segment.position.shifted(prefix);
                }
                OutputElement::Tail(_) => {}
            }
        }
        let width = group
            .elements
            .iter()
            .all(|element| !matches!(element, OutputElement::Unknown(_)))
            .then(|| {
                group
                    .elements
                    .iter()
                    .filter(|element| matches!(element, OutputElement::Column(_)))
                    .count()
            });
        prefix = prefix.and_then(|prefix| width.and_then(|width| prefix.checked_add(width)));
        output.append(&mut group.elements);
    }
    output
}

/// Where a USING key sits on one side of a join.
#[derive(Clone, Copy)]
enum KeyAt {
    Slot(usize),
    /// No column is listed, but unknown segments may publish the key.
    Unlisted,
}

/// Locate the column a USING key names on one side. A key matching several
/// columns, or none that could exist, makes the join unfoldable.
fn find_key(elements: &[OutputElement], key: &Ident, dialect: Dialect) -> Option<KeyAt> {
    let slots: Vec<usize> = elements
        .iter()
        .enumerate()
        .filter_map(|(index, element)| {
            matches!(element, OutputElement::Column(slot) if same_column_name(&slot.name, key, dialect))
                .then_some(index)
        })
        .collect();
    match slots.as_slice() {
        [index] => Some(KeyAt::Slot(*index)),
        [] => (!unknown_publishers(elements, key, dialect).0.is_empty()).then_some(KeyAt::Unlisted),
        _ => None,
    }
}

/// What one side contributes to a merged key.
struct KeySide {
    value: ValueId,
    ranges: Rc<[RangeRef]>,
    candidate_ranges: Rc<[RangeRef]>,
    /// The name of the side's listed column, if it lists one.
    names: Option<(Ident, OutputName)>,
}

/// Take the key out of one side. Unknown segments that may publish the key
/// stop doing so: the merged column replaces it.
fn take_key(
    elements: &mut Vec<OutputElement>,
    at: KeyAt,
    key: &Ident,
    dialect: Dialect,
) -> KeySide {
    match at {
        KeyAt::Slot(index) => {
            let OutputElement::Column(slot) = elements.remove(index) else {
                unreachable!("a key slot is a column");
            };
            KeySide {
                value: slot.value,
                candidate_ranges: slot.candidate_ranges,
                ranges: slot.ranges,
                names: Some((slot.name, slot.output_name)),
            }
        }
        // The key is guaranteed to exist on every joined relation, so a
        // physical relation contributes the column itself and a CTE or
        // derived table contributes whatever a direct reference to the name
        // would find in it.
        KeyAt::Unlisted => {
            let mut values: Vec<ValueId> = Vec::new();
            let mut opaque = Vec::new();
            let mut ranges: Vec<RangeRef> = Vec::new();
            for element in elements.iter_mut() {
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
                    UnknownProvider::Segment { scope, .. }
                    | UnknownProvider::SetOperation { op: scope, .. } => {
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
            // Relations that were not joined by this key (a cross join, an ON
            // join, a NATURAL join that may share nothing) cannot all have
            // it: the key is one column of one of them, and which one is
            // unknown.
            let mut distinct = HashSet::new();
            let several = ranges.iter().any(|range| {
                distinct.insert(range.index);
                distinct.len() > 1
            });
            let value = if several {
                ValueId::Uncertain {
                    column: key.clone(),
                    candidates: ranges.clone().into(),
                }
            } else {
                values
                    .into_iter()
                    .reduce(|left, right| ValueId::Merged(Box::new(left), Box::new(right)))
                    .expect("an unlisted key has a publisher")
            };
            KeySide {
                value,
                candidate_ranges: ranges.clone().into(),
                ranges: ranges.into(),
                names: None,
            }
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
        let mut left_elements = take_elements(&mut groups, &left);
        let mut right_elements = take_elements(&mut groups, &right);
        let natural_unknowns = match merge.columns {
            MergedColumns::Natural => {
                natural_uncertainties(&left_elements, &right_elements, dialect)
            }
            MergedColumns::Using(_) => Vec::new(),
        };
        let natural_order_uncertain = matches!(&merge.columns, MergedColumns::Natural)
            && left_elements.iter().chain(&right_elements).any(|element| {
                matches!(element, OutputElement::Unknown(_))
                    || matches!(element, OutputElement::Column(slot) if slot.ordinal.is_none())
            });
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
            MergedColumns::Natural => left_elements
                .iter()
                .filter_map(|element| match element {
                    OutputElement::Column(slot)
                        if right_elements.iter().any(|other| {
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

        // Every key is a column of the merged output, whatever the sides
        // list: USING guarantees it exists. The keys lead, in the order they
        // were written.
        let mut output = Vec::new();
        let mut selected_keys = HashSet::new();
        for key in keys {
            let left_at = find_key(&left_elements, &key, dialect)?;
            let right_at = find_key(&right_elements, &key, dialect)?;
            if !selected_keys.insert(key.column_key(dialect).into_owned()) {
                return None;
            }
            let left_side = take_key(&mut left_elements, left_at, &key, dialect);
            let right_side = take_key(&mut right_elements, right_at, &key, dialect);
            let ranges = union_ranges(&left_side.ranges, &right_side.ranges);
            let candidate_ranges = match merge.kind {
                JoinMergeKind::Left => left_side.candidate_ranges,
                JoinMergeKind::Right => right_side.candidate_ranges,
                JoinMergeKind::Inner | JoinMergeKind::Full => {
                    union_ranges(&left_side.candidate_ranges, &right_side.candidate_ranges)
                }
            };
            // An outer join takes the value from the preserved side; an inner
            // or full join may take it from either.
            let value = match merge.kind {
                JoinMergeKind::Left => left_side.value,
                JoinMergeKind::Right => right_side.value,
                JoinMergeKind::Inner | JoinMergeKind::Full => {
                    if matches!(left_side.value, ValueId::Uncertain { .. })
                        || matches!(right_side.value, ValueId::Uncertain { .. })
                    {
                        // One side cannot say where the key comes from, so the
                        // merged column cannot either.
                        ValueId::Uncertain {
                            column: key.clone(),
                            candidates: candidate_ranges.clone(),
                        }
                    } else {
                        ValueId::Merged(Box::new(left_side.value), Box::new(right_side.value))
                    }
                }
            };
            // The names come from the left column, else from the right one,
            // else from the USING list.
            let (name, output_name) = left_side
                .names
                .or(right_side.names)
                .unwrap_or_else(|| (key.clone(), OutputName::Explicit(key.clone())));
            output.push(OutputElement::Column(ColumnSlot {
                id: None,
                ordinal: Some(output.len()),
                name,
                output_name,
                ranges,
                candidate_ranges,
                value,
                // Both sides carry the key under its name, as USING requires.
                origin: SlotOrigin::Forwarded(key.clone()),
            }));
        }
        apply_natural_uncertainties(
            natural_unknowns,
            &mut left_elements,
            &mut right_elements,
            merge.kind,
            dialect,
        );
        output.extend(left_elements);
        output.extend(right_elements);
        recompute_ordinals(&mut output);
        for element in &mut output {
            if let OutputElement::Unknown(segment) = element
                && !matches!(segment.position, crate::types::StarPosition::Inferred)
            {
                segment.position = crate::types::StarPosition::Unproven;
            }
        }
        if natural_order_uncertain {
            for element in &mut output {
                if let OutputElement::Column(slot) = element {
                    slot.ordinal = None;
                }
            }
        }
        groups.insert(
            first,
            Group {
                members,
                elements: output,
            },
        );
    }
    Some(groups)
}

fn recompute_ordinals(elements: &mut [OutputElement]) {
    let mut next = Some(0usize);
    for element in elements {
        match element {
            OutputElement::Column(slot) if slot.ordinal.is_some() => {
                slot.ordinal = next;
                next = next.and_then(|ordinal| ordinal.checked_add(1));
            }
            OutputElement::Column(slot) => {
                slot.ordinal = None;
                next = None;
            }
            OutputElement::Unknown(_) => next = None,
            OutputElement::Tail(_) => {}
        }
    }
}

/// Move the elements of the groups at `indexes` into one list.
fn take_elements(groups: &mut [Group], indexes: &[usize]) -> Vec<OutputElement> {
    let mut elements = Vec::new();
    for &index in indexes {
        elements.append(&mut groups[index].elements);
    }
    elements
}
