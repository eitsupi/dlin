//! Values computed by minijinja, taken back into the abstract state.
//!
//! A list or dict that a template can change must live in the heap, so that
//! every alias sees the change. Containers built by a template are allocated
//! there directly; containers computed by minijinja (`dict()`, `var()`, a
//! filter, `str.split()`) come back as plain values and are adopted here, so
//! that they behave the same whatever produced them.
//!
//! A result can hold, or be, a container that was handed to minijinja, as in
//! `[child] | first` or `items | selectattr("enabled")`. Containers handed to
//! minijinja are frozen as [`SharedList`] and [`SharedMap`] objects that
//! remember their heap object, so such a container is the same heap object
//! again, while a new container gets a new one. Telling them apart by value
//! would take a new container equal to an input for the input.

use std::fmt;

use minijinja::value::{Enumerator, Object, ObjectRepr};

use super::*;

/// A frozen heap list.
#[derive(Debug)]
pub(super) struct SharedList {
    id: HeapId,
    items: Vec<Value>,
}

impl SharedList {
    pub(super) fn value(id: HeapId, items: Vec<Value>) -> Value {
        Value::from_object(Self { id, items })
    }
}

impl Object for SharedList {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        self.items.get(key.as_usize()?).cloned()
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Seq(self.items.len())
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&Value::from(self.items.clone()), f)
    }
}

/// A frozen heap dict or namespace, with its entries in order.
#[derive(Debug)]
pub(super) struct SharedMap {
    id: HeapId,
    entries: Vec<(Value, Value)>,
}

impl SharedMap {
    pub(super) fn value(id: HeapId, entries: Vec<(Value, Value)>) -> Value {
        Value::from_object(Self { id, entries })
    }
}

impl Object for SharedMap {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Values(self.entries.iter().map(|(k, _)| k.clone()).collect())
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&Value::from_pairs(self.entries.clone()), f)
    }
}

/// The heap object a frozen container was made from, if it was frozen with
/// [`Interp::freeze_shared`].
fn shared_id(value: &Value) -> Option<HeapId> {
    value
        .downcast_object_ref::<SharedList>()
        .map(|list| list.id)
        .or_else(|| value.downcast_object_ref::<SharedMap>().map(|map| map.id))
}

impl<'a> Interp<'a> {
    /// Take a value computed by minijinja into the state. Its inputs must
    /// have been frozen with [`Interp::freeze_shared`].
    pub(super) fn adopt_result(&mut self, st: &mut State, value: Value, span: Span) -> AValue {
        if !is_container(&value) {
            return AValue::Known(value);
        }
        let mut budget = MAX_ITEMS;
        let mut allocated = Vec::new();
        match self.adopt_at(st, value, span, 0, &mut budget, &mut allocated) {
            Ok(value) => value,
            Err(causes) => {
                // Do not leave a partly adopted container behind.
                for id in allocated {
                    st.heap.remove(&id);
                }
                AValue::Unknown(causes)
            }
        }
    }

    fn adopt_at(
        &mut self,
        st: &mut State,
        value: Value,
        span: Span,
        depth: usize,
        budget: &mut usize,
        allocated: &mut Vec<HeapId>,
    ) -> Result<AValue, Causes> {
        if let Some(id) = shared_id(&value) {
            return Ok(AValue::Heap(id));
        }
        if !is_container(&value) {
            return Ok(AValue::Known(value));
        }
        if depth > MAX_DEPTH {
            return Err(self.limit(AnalysisLimit::RecursionDepth, span));
        }
        let iter = value
            .try_iter()
            .map_err(|_| self.unsupported("iterating a non-iterable value", span))?;
        let mut items = Vec::new();
        for item in iter {
            if *budget == 0 {
                return Err(self.limit(AnalysisLimit::WorkBudget, span));
            }
            *budget -= 1;
            items.push(item);
        }
        let obj = if value.kind() == ValueKind::Map {
            let mut entries = Vec::with_capacity(items.len());
            for key in items {
                let item = value.get_item(&key).unwrap_or(Value::UNDEFINED);
                entries.push((
                    key,
                    self.adopt_at(st, item, span, depth + 1, budget, allocated)?,
                ));
            }
            HeapObj::Map(entries)
        } else {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(self.adopt_at(st, item, span, depth + 1, budget, allocated)?);
            }
            HeapObj::List(values)
        };
        let adopted = self.alloc(st, obj);
        if let AValue::Heap(id) = adopted {
            allocated.push(id);
        }
        Ok(adopted)
    }

    fn limit(&self, limit: AnalysisLimit, span: Span) -> Causes {
        Causes::one(DependencyReason::AnalysisLimitReached {
            limit,
            at: self.site(span),
        })
    }
}

/// Whether a template can change `value` in place: a list or a dict. A
/// lazy sequence, such as a slice, is a list once dbt has computed it.
fn is_container(value: &Value) -> bool {
    matches!(
        value.kind(),
        ValueKind::Seq | ValueKind::Map | ValueKind::Iterable
    )
}
