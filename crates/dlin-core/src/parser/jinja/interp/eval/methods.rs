//! Methods on abstract containers and known values.

use super::*;

impl<'a> Interp<'a> {
    pub(super) fn method(
        &mut self,
        st: &mut State,
        receiver: AValue,
        name: &str,
        args: Vec<AValue>,
        kwargs: Vec<(Arc<str>, AValue)>,
        span: Span,
    ) -> AValue {
        match &receiver {
            AValue::Unknown(causes) => return AValue::Unknown(causes.clone()),
            AValue::Relation => return AValue::Unknown(self.relation_causes(Some(name), span)),
            AValue::Heap(id) => {
                let id = *id;
                if let Some(value) = self.heap_method(st, id, name, &args, &kwargs, span) {
                    return value;
                }
            }
            AValue::Known(value) => {
                if let Some(result) = self.known_method(st, value, name, &args, &kwargs, span) {
                    return result;
                }
            }
            _ => {}
        }
        AValue::Unknown(self.unsupported(format!("method:{name}"), span))
    }

    pub(super) fn heap_method(
        &mut self,
        st: &mut State,
        id: HeapId,
        name: &str,
        args: &[AValue],
        kwargs: &[(Arc<str>, AValue)],
        span: Span,
    ) -> Option<AValue> {
        let obj = st.heap.get(&id)?.clone();
        // Keyword arguments and unexpected argument counts are not modelled;
        // they fall through to the unmodelled-method case below.
        let arity_ok = kwargs.is_empty()
            && match name {
                "append" | "extend" | "update" => args.len() == 1,
                "insert" => args.len() == 2,
                "pop" | "keys" | "values" | "items" => args.is_empty(),
                "get" => matches!(args.len(), 1 | 2),
                _ => true,
            };
        let method = if arity_ok { name } else { "" };
        match (obj, method) {
            (HeapObj::Clobbered(causes), _) => Some(AValue::Unknown(causes)),
            (HeapObj::List(mut items), "append") => {
                items.push(args.first().cloned().unwrap_or_else(AValue::none));
                st.heap.insert(id, HeapObj::List(items));
                Some(AValue::none())
            }
            (HeapObj::List(mut items), "extend") => {
                match args.first().map(|value| self.items(st, value, span)) {
                    Some(Ok(more)) => {
                        items.extend(more);
                        st.heap.insert(id, HeapObj::List(items));
                    }
                    Some(Err(causes)) => {
                        st.heap.insert(id, HeapObj::Clobbered(causes));
                    }
                    None => {}
                }
                Some(AValue::none())
            }
            (HeapObj::List(mut items), "insert") => {
                let index = args
                    .first()
                    .and_then(|value| self.freeze(st, value, span).ok())
                    .and_then(|value| value.as_i64())
                    .and_then(|i| usize::try_from(i).ok());
                match (index, args.get(1)) {
                    (Some(index), Some(value)) => {
                        items.insert(index.min(items.len()), value.clone());
                        st.heap.insert(id, HeapObj::List(items));
                    }
                    _ => {
                        let causes = self.unsupported("method:insert", span);
                        st.heap.insert(id, HeapObj::Clobbered(causes));
                    }
                }
                Some(AValue::none())
            }
            (HeapObj::List(mut items), "pop") if args.is_empty() => {
                let value = items.pop().unwrap_or_else(AValue::undefined);
                st.heap.insert(id, HeapObj::List(items));
                Some(value)
            }
            (HeapObj::Map(entries), "get") => {
                let key = args.first()?;
                match self.freeze(st, key, span) {
                    Ok(key) => Some(
                        entries
                            .iter()
                            .find(|(k, _)| *k == key)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_else(|| args.get(1).cloned().unwrap_or_else(AValue::none)),
                    ),
                    Err(causes) => Some(AValue::Unknown(causes)),
                }
            }
            (HeapObj::Map(entries), "keys") => {
                let keys = entries
                    .iter()
                    .map(|(k, _)| AValue::Known(k.clone()))
                    .collect();
                Some(self.alloc(st, HeapObj::List(keys)))
            }
            (HeapObj::Map(entries), "values") => {
                let values = entries.into_iter().map(|(_, v)| v).collect();
                Some(self.alloc(st, HeapObj::List(values)))
            }
            (HeapObj::Map(entries), "items") => {
                let mut pairs = Vec::new();
                for (key, value) in entries {
                    let pair = self.alloc(st, HeapObj::List(vec![AValue::Known(key), value]));
                    pairs.push(pair);
                }
                Some(self.alloc(st, HeapObj::List(pairs)))
            }
            (HeapObj::Map(mut entries), "update") => {
                let other = args.first()?;
                let other_entries = match other {
                    AValue::Heap(other_id) => match st.heap.get(other_id) {
                        Some(HeapObj::Map(other)) => Some(other.clone()),
                        _ => None,
                    },
                    _ => None,
                };
                match other_entries {
                    Some(other) => {
                        for (key, value) in other {
                            match entries.iter_mut().find(|(k, _)| *k == key) {
                                Some(entry) => entry.1 = value,
                                None => entries.push((key, value)),
                            }
                        }
                        st.heap.insert(id, HeapObj::Map(entries));
                    }
                    None => {
                        let causes = self.unsupported("method:update", span);
                        st.heap.insert(id, HeapObj::Clobbered(causes));
                    }
                }
                Some(AValue::none())
            }
            _ => {
                // An unmodelled method may change the container.
                let causes = self.unsupported(format!("method:{name}"), span);
                st.heap.insert(id, HeapObj::Clobbered(causes.clone()));
                Some(AValue::Unknown(causes))
            }
        }
    }

    /// A method of a known value, evaluated by minijinja with its
    /// Python-compatible methods. `None` when minijinja cannot evaluate it,
    /// which makes the result unknown rather than wrong.
    pub(super) fn known_method(
        &self,
        st: &State,
        value: &Value,
        name: &str,
        args: &[AValue],
        kwargs: &[(Arc<str>, AValue)],
        span: Span,
    ) -> Option<AValue> {
        let mut values = match self.freeze_all(st, args, span) {
            Ok(values) => values,
            Err(causes) => return Some(AValue::Unknown(causes)),
        };
        if !kwargs.is_empty() {
            let mut pairs = Vec::new();
            for (key, item) in kwargs {
                if key.as_ref() == "**" {
                    return None;
                }
                match self.freeze(st, item, span) {
                    Ok(item) => pairs.push((key.to_string(), item)),
                    Err(causes) => return Some(AValue::Unknown(causes)),
                }
            }
            values.push(Value::from(minijinja::value::Kwargs::from_iter(pairs)));
        }
        known::call_method(value, name, &values).map(AValue::Known)
    }
}
