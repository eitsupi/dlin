//! Structural description of what a star or a scope outputs.
//!
//! A model is an ordered list of elements: columns whose names are known and
//! segments of columns the analysis cannot enumerate. USING/NATURAL folding
//! and the EXCLUDE/ILIKE/REPLACE/RENAME modifiers are applied once while a
//! star's model is built, so widths and names are read from one result instead
//! of being recomputed by every consumer. Values are lazy references and never
//! carry lineage: building a model does not resolve any column origin.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::rc::Rc;

mod build;
mod choose;
mod input;
mod join;
mod options;
mod slot;

pub(super) use build::{ModelStore, OutputModels};
pub(super) use choose::Choice;
use input::Invalidity;
pub(super) use input::{InputLookup, RangeRef};
pub(crate) use slot::OutputRef;
pub(super) use slot::{Forwarding, SegmentId, SlotId};

use super::target::{excluded_matches_name, ilike_matches, same_column_name};

use crate::graph::node::{NodeId, StarBase, StarColumnName};
use crate::graph::scope::{ScopeId, VirtualSourceId};
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
    /// A column that a CTE or derived table publishes, as its reader sees it.
    Forward(SlotId),
    /// A column a USING key takes from a CTE or derived table that lists no
    /// slot of that name, found the way a direct reference finds it.
    ScopeName { scope: ScopeId, column: Ident },
    /// A column of a set operation, which combines its operands positionally:
    /// the merge of `op` standing for the column of its left operand.
    SetMerged { op: ScopeId, left: SlotId },
    /// A column of a virtual source such as a lateral expansion.
    VirtualColumn {
        source: VirtualSourceId,
        index: usize,
    },
    /// A column of the right operand of a set operation, found by name among
    /// the mappings the operation merges in beyond its left operand.
    /// A column that is named but sits behind segments the analysis cannot
    /// enumerate.
    Named {
        providers: Vec<UnknownProvider>,
        column: Ident,
    },
    /// A column merged by USING/NATURAL whose value both sides contribute to.
    Merged(Box<ValueId>, Box<ValueId>),
    /// A proven value whose completeness depends on the listed schemas.
    SchemaDependent {
        value: Option<Box<ValueId>>,
        relations: Vec<crate::Relation>,
    },
    /// A column that exactly one of several unknown relations supplies, with
    /// no way to tell which. Naming one of them would invent a source.
    Uncertain {
        column: Ident,
        candidates: Rc<[RangeRef]>,
    },
}

/// Why a segment of columns cannot be enumerated.
#[derive(Debug, Clone)]
pub(super) enum UnknownProvider {
    /// A physical relation without catalog columns.
    Relation(Rc<TableRef>),
    /// A segment that a CTE or derived table publishes, as its reader sees
    /// it. `scope` is the publisher.
    Segment { scope: ScopeId, id: SegmentId },
    /// A segment of a set operation: the merge of `op` standing for the
    /// segment of its left operand.
    SetOperation { op: ScopeId, left: SegmentId },
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
    /// A shape whose correspondence is not implemented.
    Unsupported(&'static str),
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
    filters: Vec<(Rc<StarFilter>, Rc<[RangeRef]>)>,
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

/// How a column relates to the model of the CTE or derived table it is read
/// from. The name a column is published under can differ from the name that
/// model knows it by: a RENAME changes only the former.
#[derive(Debug, Clone)]
pub(super) enum SlotOrigin {
    /// Forwards the column the range's child model publishes under this name.
    Forwarded(Ident),
    /// Defined by this model (a projection expression, a REPLACE item); the
    /// child's view of the name does not apply.
    Defined,
}

/// A column with a known name.
#[derive(Debug, Clone)]
pub(super) struct ColumnSlot {
    /// The identity the owning scope gave the column when it published its
    /// model. `None` in star models and in the models of FROM clauses, which
    /// are views of other scopes' outputs and publish nothing themselves.
    pub id: Option<SlotId>,
    /// The name the column is published under.
    pub name: Ident,
    pub output_name: OutputName,
    /// Proven zero-based position in this model's output, when available.
    pub ordinal: Option<usize>,
    /// Possible positions established by syntax or schema evidence. Display
    /// order alone never narrows this range.
    pub position: PositionRange,
    /// Range variables the column is visible under. A merged column keeps
    /// the range variables of both sides, whatever the join kind.
    pub ranges: Rc<[RangeRef]>,
    /// Range variables whose values contribute to the column. Unlike
    /// `ranges`, this excludes sides suppressed by an outer USING join.
    pub candidate_ranges: Rc<[RangeRef]>,
    pub value: ValueId,
    pub origin: SlotOrigin,
}

/// Columns the analysis cannot enumerate, with the names they may still
/// publish.
#[derive(Debug, Clone)]
pub(super) struct UnknownSegment {
    /// The identity the owning scope gave the segment when it published its
    /// model; `None` wherever [`ColumnSlot::id`] is.
    pub id: Option<SegmentId>,
    /// Physical schemas responsible for this interval, retained across scopes.
    pub schema_relations: Vec<crate::Relation>,
    pub position: crate::types::StarPosition,
    /// Positions that this interval may occupy, independent of its names.
    pub position_range: PositionRange,
    pub provider: UnknownProvider,
    pub ranges: Rc<[RangeRef]>,
    pub domain: NameDomain,
}

impl ColumnSlot {
    /// The identity of a column of a published model.
    pub(super) fn published(&self) -> SlotId {
        self.id
            .expect("only the columns of published models are read")
    }

    /// The published column this one forwards, when it forwards one.
    fn forwards(&self) -> Option<OutputRef> {
        match self.value {
            ValueId::Forward(id) | ValueId::SetMerged { left: id, .. } => Some(OutputRef::Slot(id)),
            _ => None,
        }
    }
}

impl UnknownSegment {
    /// The identity of a segment of a published model.
    pub(super) fn published(&self) -> SegmentId {
        self.id
            .expect("only the segments of published models are read")
    }

    /// The published segment this one forwards, when it forwards one.
    fn forwards(&self) -> Option<OutputRef> {
        match self.provider {
            UnknownProvider::Segment { id, .. }
            | UnknownProvider::SetOperation { left: id, .. } => Some(OutputRef::Segment(id)),
            _ => None,
        }
    }

    fn opaque(reason: OpaqueReason) -> Self {
        Self {
            id: None,
            schema_relations: Vec::new(),
            position: crate::types::StarPosition::Inferred,
            position_range: PositionRange { min: 0, max: None },
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
}

/// A conservative interval of possible output positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PositionRange {
    /// Inclusive lower bound.
    pub min: usize,
    /// Inclusive upper bound, or no established upper bound.
    pub max: Option<usize>,
}

impl PositionRange {
    pub(super) fn exact(ordinal: usize) -> Self {
        Self {
            min: ordinal,
            max: Some(ordinal),
        }
    }

    pub(super) fn overlaps(self, other: Self) -> bool {
        self.max.is_none_or(|max| max >= other.min) && other.max.is_none_or(|max| max >= self.min)
    }

    pub(super) fn shifted(self, minimum_base: usize, exact_base: Option<usize>) -> Self {
        match exact_base {
            Some(base) => Self {
                min: base.saturating_add(self.min),
                max: self.max.and_then(|max| base.checked_add(max)),
            },
            None => Self {
                min: minimum_base.saturating_add(self.min),
                max: None,
            },
        }
    }
}

/// Correspondence and operator metadata for the outputs this model publishes.
/// Left identities define the namespace; right identities supply either proven
/// contributions or position-constrained candidates.
#[derive(Debug, Clone)]
pub(super) struct SetOperationModel {
    pub kind: crate::SetOperationKind,
    pub recursive: bool,
    pub slots: Vec<AlignedSlot>,
    pub segments: Vec<AlignedSegment>,
}

#[derive(Debug, Clone)]
pub(super) struct AlignedSlot {
    pub left: SlotId,
    pub matches: Vec<SlotMatch>,
}

#[derive(Debug, Clone)]
pub(super) struct AlignedSegment {
    pub left: SegmentId,
    pub matches: Vec<SlotMatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SlotMatchKind {
    /// Both slots carry the same proven ordinal.
    Proven,
    /// Possible position ranges intersect; no value contribution is proven.
    Possible,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SlotMatch {
    pub operand: crate::SetOperand,
    pub output: OutputRef,
    pub kind: SlotMatchKind,
}

/// All candidates for a name, built on first use. Names are compared by the
/// dialect's column key and duplicates are kept.
#[derive(Debug)]
pub(super) struct NameIndex {
    dialect: Dialect,
    slots: OnceCell<HashMap<String, Vec<usize>>>,
    /// Positions of the unknown segments, which may publish any name their
    /// conditions admit.
    unlisted: OnceCell<Vec<usize>>,
}

#[derive(Debug)]
pub(super) struct OutputModel {
    pub elements: Vec<OutputElement>,
    pub names: NameIndex,
    pub width: Width,
    /// Why the width of a scope is unknown, following only the scopes that
    /// produce its output.
    pub unknown_reasons: Vec<&'static str>,
    /// Joins that merge names over a FROM clause in which two ranges share a
    /// name, so which relation a merged name belongs to cannot be told.
    pub invalid: Vec<Invalidity>,
    /// The models of the CTEs and derived tables this model reads, by the
    /// index of the range that reads them. The elements a range contributes are
    /// weighed by the model they came from.
    pub reads: Vec<(usize, Rc<OutputModel>)>,
    /// For a scope that only re-expresses one other scope's output (a
    /// delegating scope, the merge of a set operation), which output of that
    /// scope each of its own outputs stands for. Empty for every other model.
    pub forwarded: Vec<Forwarding>,
    /// Which publication of its scope this model is. `None` for the models of
    /// stars and FROM clauses, which publish nothing.
    pub publication: Option<u32>,
    pub set_operation: Option<SetOperationModel>,
}

/// The elements of a model that may carry one name.
pub(super) struct NameMatches {
    /// Enumerated columns, in output order.
    pub listed: Vec<usize>,
    /// Unknown segments that can still publish the name. A segment the
    /// analysis does not model (VALUES, a nested field) is not one.
    pub unknown: Vec<usize>,
}

impl OutputModel {
    fn new(
        elements: Vec<OutputElement>,
        unknown_reasons: Vec<&'static str>,
        dialect: Dialect,
    ) -> Self {
        let listed = elements.len();
        let width = if elements
            .iter()
            .any(|element| matches!(element, OutputElement::Unknown(_)))
        {
            Width::Unknown
        } else {
            Width::Exact(listed)
        };
        Self {
            elements,
            names: NameIndex {
                dialect,
                slots: OnceCell::new(),
                unlisted: OnceCell::new(),
            },
            width,
            unknown_reasons,
            invalid: Vec::new(),
            reads: Vec::new(),
            forwarded: Vec::new(),
            publication: None,
            set_operation: None,
        }
    }

    fn opaque(reason: OpaqueReason, dialect: Dialect) -> Self {
        Self::new(
            vec![OutputElement::Unknown(UnknownSegment::opaque(reason))],
            Vec::new(),
            dialect,
        )
    }

    /// Whether `forwarded` pairs every output that `source` publishes with
    /// exactly one output of this model, which `owner` publishes. A model
    /// that fails this has lost track of which output is which.
    pub(super) fn check_forwarding(
        &self,
        arena: &slot::SlotArena,
        owner: ScopeId,
        source_scope: ScopeId,
        source: &OutputModel,
    ) -> Result<(), &'static str> {
        for (index, pair) in self.forwarded.iter().enumerate() {
            if arena.owner(pair.from) != source_scope {
                return Err("a forwarded output is not published by the forwarded scope");
            }
            if arena.owner(pair.to) != owner {
                return Err("a forwarding output is not published by the reader");
            }
            if self.forwarded[..index]
                .iter()
                .any(|seen| seen.from == pair.from || seen.to == pair.to)
            {
                return Err("an output is forwarded twice");
            }
        }
        let source: Vec<_> = source
            .elements
            .iter()
            .filter_map(OutputElement::output_ref)
            .collect();
        if source.len() != self.forwarded.len()
            || !source
                .iter()
                .all(|output| self.forwarded.iter().any(|pair| pair.from == *output))
        {
            return Err("an output of the forwarded scope has no correspondence");
        }
        Ok(())
    }

    pub(super) fn has_unknown_segment(&self) -> bool {
        self.width == Width::Unknown
    }

    /// Everything in the output that may carry `name`: the columns that list
    /// it and the unknown segments whose name conditions admit it.
    pub(super) fn matches(&self, name: &Ident) -> NameMatches {
        let dialect = self.names.dialect;
        let mut matches = NameMatches {
            listed: self.slots_named(name).to_vec(),
            unknown: Vec::new(),
        };
        let unlisted = self.names.unlisted.get_or_init(|| {
            self.elements
                .iter()
                .enumerate()
                .filter(|(_, element)| matches!(element, OutputElement::Unknown(_)))
                .map(|(index, _)| index)
                .collect()
        });
        for &index in unlisted {
            match &self.elements[index] {
                OutputElement::Unknown(segment)
                    if (!matches!(segment.provider, UnknownProvider::Opaque(_))
                        || matches!(
                            segment.provider,
                            UnknownProvider::Opaque(OpaqueReason::Unsupported(
                                "BY NAME set operation"
                            ))
                        ))
                        && segment.domain.admits(name, dialect) =>
                {
                    matches.unknown.push(index);
                }
                _ => {}
            }
        }
        matches
    }

    /// Indexes of the enumerated columns named `name`, in output order.
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

/// A lower bound on width, including individually established positions.
/// Counting only listed columns loses the gaps in a partial schema.
fn minimum_width(elements: &[OutputElement]) -> usize {
    let listed = elements
        .iter()
        .filter(|element| matches!(element, OutputElement::Column(_)))
        .count();
    elements.iter().fold(listed, |minimum, element| {
        minimum.max(match element {
            OutputElement::Column(slot) => slot.position.min.saturating_add(1),
            OutputElement::Unknown(segment) => segment.position_range.min,
        })
    })
}

/// The output of the relations a join combined, which can be merged further.
#[derive(Debug, Clone)]
struct Group {
    members: Vec<Ident>,
    elements: Vec<OutputElement>,
}

impl OutputElement {
    /// The published output this element forwards, when it forwards one.
    pub(super) fn forwards(&self) -> Option<OutputRef> {
        match self {
            Self::Column(slot) => slot.forwards(),
            Self::Unknown(segment) => segment.forwards(),
        }
    }

    /// This element's own identity, once its scope has published it.
    pub(super) fn output_ref(&self) -> Option<OutputRef> {
        match self {
            Self::Column(slot) => slot.id.map(OutputRef::Slot),
            Self::Unknown(segment) => segment.id.map(OutputRef::Segment),
        }
    }
}

/// The providers and range variables of the unknown segments that may publish
/// `name`.
fn unknown_publishers(
    elements: &[OutputElement],
    name: &Ident,
    dialect: Dialect,
) -> (Vec<UnknownProvider>, Vec<RangeRef>) {
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

#[cfg(test)]
mod tests;

impl UnknownProvider {
    /// Whether an unknown name depends on unimplemented BY NAME
    /// correspondence. Follow published segments without evaluating values,
    /// so ambiguity can retain this reason without claiming any candidate.
    pub(in crate::resolve) fn has_by_name_reason(
        &self,
        graph: &crate::graph::RawGraph,
        arena: &slot::SlotArena,
    ) -> bool {
        let mut provider = self.clone();
        let mut seen = std::collections::HashSet::new();
        loop {
            provider = match provider {
                Self::Relation(_) => return false,
                Self::Opaque(OpaqueReason::Unsupported("BY NAME set operation")) => return true,
                Self::Opaque(_) => return false,
                Self::Segment { id, .. } => {
                    if !seen.insert(id) {
                        return false;
                    }
                    arena.segment(id).provider.clone()
                }
                Self::SetOperation { op, left } => {
                    if graph
                        .scopes
                        .unknown_output_reasons(op)
                        .contains(&"BY NAME set operation")
                    {
                        return true;
                    }
                    if !seen.insert(left) {
                        return false;
                    }
                    arena.segment(left).provider.clone()
                }
            };
        }
    }
}
