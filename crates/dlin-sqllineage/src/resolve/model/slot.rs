//! Identity of the columns and segments a scope publishes.
//!
//! A scope's model lists its outputs in order. Position, name and width are
//! views of an output that change from reader to reader: a star shifts
//! ordinals, a RENAME changes the name, a join folds range variables. The id
//! given to an output when its scope publishes it is what does not change, so
//! a reader can refer to one output even when another one shares its name or
//! its position.
//!
//! The arena keeps, per id, only what identifies the output and defines its
//! value. The view data (ordinal, ranges, published name) stays in the model
//! elements. Ids are valid within one [`ModelStore`](super::ModelStore) and
//! are never reused. A model that is rebuilt after a cycle was cut allocates
//! new ids and the discarded ones are no longer reachable.

use std::rc::Rc;

use super::{UnknownProvider, ValueId};
use crate::Ident;
use crate::graph::scope::ScopeId;
use crate::types::StarPosition;

/// A column published by a scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::resolve) struct SlotId(u32);

/// A segment of unenumerable columns published by a scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::resolve) struct SegmentId(u32);

/// The definition of a published column.
#[derive(Debug)]
#[allow(dead_code, reason = "ids are published before any reader follows them")]
pub(in crate::resolve) struct SlotDef {
    /// The scope whose model publishes the column.
    pub owner: ScopeId,
    /// The name the column was published under.
    pub name: Ident,
    /// How the column's value is found.
    pub value: ValueId,
    /// The column's element index in the owner's model, tails included.
    pub index: usize,
}

/// The definition of a published segment.
#[derive(Debug)]
#[allow(dead_code, reason = "ids are published before any reader follows them")]
pub(in crate::resolve) struct SegmentDef {
    /// The scope whose model publishes the segment.
    pub owner: ScopeId,
    pub provider: UnknownProvider,
    /// The position the owner's model gives the segment.
    pub position: StarPosition,
}

/// Every published column and segment of one statement. Append-only.
#[derive(Debug, Default)]
pub(in crate::resolve) struct SlotArena {
    slots: Vec<Rc<SlotDef>>,
    segments: Vec<Rc<SegmentDef>>,
}

impl SlotArena {
    pub(super) fn add_slot(&mut self, def: SlotDef) -> SlotId {
        let id = SlotId(
            u32::try_from(self.slots.len()).expect("a statement publishes fewer than 2^32 columns"),
        );
        self.slots.push(Rc::new(def));
        id
    }

    pub(super) fn add_segment(&mut self, def: SegmentDef) -> SegmentId {
        let id = SegmentId(
            u32::try_from(self.segments.len())
                .expect("a statement publishes fewer than 2^32 segments"),
        );
        self.segments.push(Rc::new(def));
        id
    }

    #[cfg(test)]
    pub(super) fn slot(&self, id: SlotId) -> &Rc<SlotDef> {
        &self.slots[id.0 as usize]
    }

    #[cfg(test)]
    pub(super) fn segment(&self, id: SegmentId) -> &Rc<SegmentDef> {
        &self.segments[id.0 as usize]
    }
}
