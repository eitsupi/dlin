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
        // they fall through to the unmodelled-method case below, except for
        // the keywords of `dict.update`.
        let arity_ok = match name {
            "update" => args.len() <= 1,
            _ => {
                kwargs.is_empty()
                    && match name {
                        "append" | "extend" => args.len() == 1,
                        "insert" => args.len() == 2,
                        "keys" | "values" | "items" | "clear" => args.is_empty(),
                        "pop" => args.len() <= 2,
                        "get" => matches!(args.len(), 1 | 2),
                        _ => true,
                    }
            }
        };
        let method = if arity_ok { name } else { "" };
        // The methods that change a container in place return it, as
        // Fusion's minijinja does, except `append`, `pop` and `clear`.
        let receiver = AValue::Heap(id);
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
                        Some(receiver)
                    }
                    Some(Err(causes)) => Some(self.clobber(st, id, causes)),
                    None => Some(receiver),
                }
            }
            (HeapObj::List(mut items), "insert") => {
                let index = args
                    .first()
                    .and_then(|value| self.freeze(st, value, span).ok())
                    .and_then(|value| value.as_i64());
                match (index, args.get(1)) {
                    (Some(index), Some(value)) => {
                        // A negative index counts from the end, as in Python.
                        let len = items.len() as i64;
                        let index = if index < 0 { index + len } else { index };
                        let index = usize::try_from(index.max(0)).unwrap_or(0).min(items.len());
                        items.insert(index, value.clone());
                        st.heap.insert(id, HeapObj::List(items));
                        Some(receiver)
                    }
                    _ => {
                        let causes = self.unsupported("method:insert", span);
                        Some(self.clobber(st, id, causes))
                    }
                }
            }
            (HeapObj::List(mut items), "pop") if args.len() <= 1 => {
                let index = match args.first() {
                    None => Some(items.len() as i64 - 1),
                    Some(index) => match self.freeze(st, index, span) {
                        Ok(index) => index.as_i64(),
                        Err(causes) => return Some(self.clobber(st, id, causes)),
                    },
                };
                let Some(index) = index else {
                    let causes = self.unsupported("method:pop", span);
                    return Some(self.clobber(st, id, causes));
                };
                let len = items.len() as i64;
                let index = if index < 0 { index + len } else { index };
                // An index out of range gives undefined, as in Fusion.
                let value = usize::try_from(index)
                    .ok()
                    .filter(|&index| index < items.len())
                    .map(|index| items.remove(index))
                    .unwrap_or_else(AValue::undefined);
                st.heap.insert(id, HeapObj::List(items));
                Some(value)
            }
            (HeapObj::List(_), "clear") => {
                st.heap.insert(id, HeapObj::List(Vec::new()));
                Some(AValue::none())
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
            (HeapObj::Map(mut entries), "pop") if !args.is_empty() => {
                let key = match self.freeze(st, &args[0], span) {
                    Ok(key) => key,
                    Err(causes) => return Some(self.clobber(st, id, causes)),
                };
                let value = match entries.iter().position(|(k, _)| *k == key) {
                    Some(index) => entries.remove(index).1,
                    None => args.get(1).cloned().unwrap_or_else(AValue::undefined),
                };
                st.heap.insert(id, HeapObj::Map(entries));
                Some(value)
            }
            (HeapObj::Map(_), "clear") => {
                st.heap.insert(id, HeapObj::Map(Vec::new()));
                Some(AValue::none())
            }
            (HeapObj::Map(mut entries), "update") => {
                let mut updates = match args.first() {
                    None => Vec::new(),
                    Some(AValue::Heap(other_id)) => match st.heap.get(other_id) {
                        Some(HeapObj::Map(other)) => other.clone(),
                        _ => {
                            let causes = self.unsupported("method:update", span);
                            return Some(self.clobber(st, id, causes));
                        }
                    },
                    Some(AValue::Unknown(causes)) => {
                        return Some(self.clobber(st, id, causes.clone()));
                    }
                    Some(_) => {
                        let causes = self.unsupported("method:update", span);
                        return Some(self.clobber(st, id, causes));
                    }
                };
                for (key, value) in kwargs {
                    if key.as_ref() == "**" {
                        let causes = self.unsupported("method:update", span);
                        return Some(self.clobber(st, id, causes));
                    }
                    updates.push((Value::from(key.as_ref()), value.clone()));
                }
                for (key, value) in updates {
                    match entries.iter_mut().find(|(k, _)| *k == key) {
                        Some(entry) => entry.1 = value,
                        None => entries.push((key, value)),
                    }
                }
                st.heap.insert(id, HeapObj::Map(entries));
                Some(receiver)
            }
            _ => {
                // An unmodelled method may change the container.
                let causes = self.unsupported(format!("method:{name}"), span);
                Some(self.clobber(st, id, causes))
            }
        }
    }

    /// Make a container unknown after a change that is not modelled, and
    /// give the unknown result of that change.
    fn clobber(&self, st: &mut State, id: HeapId, causes: Causes) -> AValue {
        st.heap.insert(id, HeapObj::Clobbered(causes.clone()));
        AValue::Unknown(causes)
    }

    /// A method of a known value, evaluated by minijinja with its
    /// Python-compatible methods. `None` when minijinja cannot evaluate it,
    /// which makes the result unknown rather than wrong.
    pub(super) fn known_method(
        &mut self,
        st: &mut State,
        value: &Value,
        name: &str,
        args: &[AValue],
        kwargs: &[(Arc<str>, AValue)],
        span: Span,
    ) -> Option<AValue> {
        let mut values = match self.freeze_all_shared(st, args, span) {
            Ok(values) => values,
            Err(causes) => return Some(AValue::Unknown(causes)),
        };
        if !kwargs.is_empty() {
            let mut pairs = Vec::new();
            for (key, item) in kwargs {
                if key.as_ref() == "**" {
                    return None;
                }
                match self.freeze_shared(st, item, span) {
                    Ok(item) => pairs.push((key.to_string(), item)),
                    Err(causes) => return Some(AValue::Unknown(causes)),
                }
            }
            values.push(Value::from(minijinja::value::Kwargs::from_iter(pairs)));
        }
        let result = known::call_method(value, name, &values)?;
        Some(self.adopt_result(st, result, span))
    }
}
