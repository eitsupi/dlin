//! Abstract expression evaluation and value access.

use super::*;

impl<'a> Interp<'a> {
    // ----- values -------------------------------------------------------

    pub(super) fn truth(&self, st: &State, value: &AValue, span: Span) -> Truth {
        match value {
            AValue::Known(value) => Truth::Known(value.is_true()),
            AValue::Unknown(causes) => Truth::Unknown(causes.clone()),
            AValue::Heap(id) => match st.heap.get(id) {
                Some(HeapObj::List(items)) => Truth::Known(!items.is_empty()),
                Some(HeapObj::Map(entries)) => Truth::Known(!entries.is_empty()),
                Some(HeapObj::Namespace(_)) => Truth::Known(true),
                Some(HeapObj::Clobbered(causes)) => Truth::Unknown(causes.clone()),
                None => Truth::Known(false),
            },
            AValue::Macro(_)
            | AValue::Builtin(_)
            | AValue::Relation
            | AValue::MacroNamespace(_) => Truth::Known(true),
            AValue::Context(object) => {
                if object.path.as_ref() == object.root {
                    Truth::Known(true)
                } else {
                    Truth::Unknown(self.context_causes(object, span))
                }
            }
        }
    }

    /// Convert a value to a minijinja value, or explain why it is unknown.
    pub(super) fn freeze(&self, st: &State, value: &AValue, span: Span) -> Result<Value, Causes> {
        self.freeze_at(st, value, span, 0, false)
    }

    /// Like [`Self::freeze`], for a value handed to minijinja whose result
    /// is adopted back: its containers keep their heap identity, so that a
    /// container returned as it is can be told apart from a new one.
    pub(super) fn freeze_shared(
        &self,
        st: &State,
        value: &AValue,
        span: Span,
    ) -> Result<Value, Causes> {
        self.freeze_at(st, value, span, 0, true)
    }

    fn freeze_at(
        &self,
        st: &State,
        value: &AValue,
        span: Span,
        depth: usize,
        shared: bool,
    ) -> Result<Value, Causes> {
        // Containers can hold themselves; such values are not modelled.
        if depth > MAX_DEPTH {
            return Err(self.unsupported("deeply nested or cyclic container", span));
        }
        let freeze = |item: &AValue| self.freeze_at(st, item, span, depth + 1, shared);
        match value {
            AValue::Known(value) => Ok(value.clone()),
            AValue::Unknown(causes) => Err(causes.clone()),
            AValue::Heap(id) => match st.heap.get(id) {
                Some(HeapObj::List(items)) => items
                    .iter()
                    .map(freeze)
                    .collect::<Result<Vec<_>, _>>()
                    .map(|items| {
                        if shared {
                            SharedList::value(*id, items)
                        } else {
                            Value::from(items)
                        }
                    }),
                Some(HeapObj::Map(entries)) => entries
                    .iter()
                    .map(|(key, item)| freeze(item).map(|v| (key.clone(), v)))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|entries| {
                        if shared {
                            SharedMap::value(*id, entries)
                        } else {
                            Value::from_pairs(entries)
                        }
                    }),
                Some(HeapObj::Namespace(attrs)) => attrs
                    .iter()
                    .map(|(key, item)| freeze(item).map(|v| (Value::from(key.as_ref()), v)))
                    .collect::<Result<Vec<_>, _>>()
                    .map(|entries| {
                        if shared {
                            SharedMap::value(*id, entries)
                        } else {
                            Value::from_pairs(entries)
                        }
                    }),
                Some(HeapObj::Clobbered(causes)) => Err(causes.clone()),
                None => Ok(Value::UNDEFINED),
            },
            AValue::Relation => Err(self.relation_causes(None, span)),
            AValue::Context(object) => Err(self.context_causes(object, span)),
            AValue::Macro(def) => {
                Err(self.unsupported(format!("macro value {}", def.def.name), span))
            }
            AValue::Builtin(_) => Err(self.unsupported("function value", span)),
            AValue::MacroNamespace(_) => Err(self.unsupported("macro namespace value", span)),
        }
    }

    /// The items of an iterable, or why they are unknown.
    pub(super) fn items(
        &self,
        st: &State,
        value: &AValue,
        span: Span,
    ) -> Result<Vec<AValue>, Causes> {
        match value {
            AValue::Heap(id) => match st.heap.get(id) {
                Some(HeapObj::List(items)) => Ok(items.clone()),
                Some(HeapObj::Map(entries)) => Ok(entries
                    .iter()
                    .map(|(key, _)| AValue::Known(key.clone()))
                    .collect()),
                Some(HeapObj::Namespace(_)) => Err(self.unsupported("iterating a namespace", span)),
                Some(HeapObj::Clobbered(causes)) => Err(causes.clone()),
                None => Ok(Vec::new()),
            },
            AValue::Known(value) => {
                if value.is_undefined() || value.is_none() {
                    return Ok(Vec::new());
                }
                match value.try_iter() {
                    Ok(iter) => Ok(iter.take(MAX_ITEMS + 1).map(AValue::Known).collect()),
                    Err(_) => Err(self.unsupported("iterating a non-iterable value", span)),
                }
            }
            other => self.freeze(st, other, span).map(|_| Vec::new()),
        }
    }

    // ----- expressions --------------------------------------------------

    pub(crate) fn eval(&mut self, st: &mut State, expr: &Expr) -> Ev<AValue> {
        self.tick(Some(expr.span))?;
        let span = expr.span;
        let value = match &expr.kind {
            ExprKind::Var(name) => self.lookup(st, name, span),
            ExprKind::Const(value) => AValue::Known(value.clone()),
            ExprKind::Slice {
                expr,
                start,
                stop,
                step,
            } => {
                let value = self.eval(st, expr)?;
                let mut parts = vec![value];
                for part in [start, stop, step] {
                    parts.push(match part {
                        Some(e) => self.eval(st, e)?,
                        None => AValue::none(),
                    });
                }
                match self.freeze_all_shared(st, &parts, span) {
                    Ok(values) => {
                        match known::slice(&values[0], &values[1], &values[2], &values[3]) {
                            Ok(value) => {
                                let value = computed_list(value);
                                self.adopt_result(st, value, span)
                            }
                            Err(_) => AValue::Unknown(self.unsupported("slice", span)),
                        }
                    }
                    Err(causes) => AValue::Unknown(causes),
                }
            }
            ExprKind::Unary { op, expr } => {
                let value = self.eval(st, expr)?;
                match op {
                    UnaryOp::Not => match self.truth(st, &value, span) {
                        Truth::Known(b) => AValue::Known(Value::from(!b)),
                        Truth::Unknown(causes) => AValue::Unknown(causes),
                    },
                    UnaryOp::Neg => match self.freeze(st, &value, span) {
                        Ok(value) => known::negate(&value)
                            .map(AValue::Known)
                            .unwrap_or_else(|_| AValue::Unknown(self.unsupported("-", span))),
                        Err(causes) => AValue::Unknown(causes),
                    },
                }
            }
            ExprKind::Binary { op, left, right } => match op {
                BinOp::And | BinOp::Or => {
                    let left_value = self.eval(st, left)?;
                    match self.truth(st, &left_value, left.span) {
                        Truth::Known(b) => {
                            if b == (*op == BinOp::And) {
                                self.eval(st, right)?
                            } else {
                                left_value
                            }
                        }
                        Truth::Unknown(causes) => {
                            // Either the right side is not evaluated and the
                            // result is the left value, or it is evaluated.
                            self.eval_alternatives(
                                st,
                                &causes,
                                Some(left_value),
                                None,
                                Some(right),
                            )?
                        }
                    }
                }
                _ => {
                    let l = self.eval(st, left)?;
                    let r = self.eval(st, right)?;
                    self.binary(st, *op, &l, &r, span)
                }
            },
            ExprKind::Compare { expr, ops } => {
                let mut left = self.eval(st, expr)?;
                let mut unknown: Option<Causes> = None;
                for (op, rhs) in ops {
                    let right = self.eval(st, rhs)?;
                    match self.binary(st, *op, &left, &right, span) {
                        AValue::Known(result) if !result.is_true() => {
                            return Ok(match unknown {
                                Some(causes) => AValue::Unknown(causes),
                                None => AValue::Known(Value::from(false)),
                            });
                        }
                        AValue::Known(_) => {}
                        other => {
                            let causes = other
                                .causes()
                                .cloned()
                                .unwrap_or_else(|| self.unsupported("comparison", span));
                            unknown = Some(match unknown {
                                Some(existing) => existing.union(&causes),
                                None => causes,
                            });
                        }
                    }
                    left = right;
                }
                match unknown {
                    Some(causes) => AValue::Unknown(causes),
                    None => AValue::Known(Value::from(true)),
                }
            }
            ExprKind::IfExpr {
                test,
                then,
                otherwise,
            } => {
                let condition = self.eval(st, test)?;
                match self.truth(st, &condition, test.span) {
                    Truth::Known(true) => self.eval(st, then)?,
                    Truth::Known(false) => match otherwise {
                        Some(e) => self.eval(st, e)?,
                        None => AValue::undefined(),
                    },
                    Truth::Unknown(causes) => {
                        let otherwise_value = otherwise.is_none().then(AValue::undefined);
                        self.eval_alternatives(
                            st,
                            &causes,
                            otherwise_value,
                            Some(then),
                            otherwise.as_deref(),
                        )?
                    }
                }
            }
            ExprKind::Filter { name, expr, args } => {
                let input = match expr {
                    Some(e) => self.eval(st, e)?,
                    None => AValue::undefined(),
                };
                self.apply_filter(st, name, input, args, span)?
            }
            ExprKind::Test { name, expr, args } => {
                let value = self.eval(st, expr)?;
                let mut values = vec![value];
                for arg in args {
                    values.push(self.eval(st, arg.expr())?);
                }
                self.test(st, name, &values, span)
            }
            ExprKind::GetAttr { expr, name } => {
                let object = self.eval_attribute_receiver(st, expr)?;
                self.get_attr(st, &object, name, span)
            }
            ExprKind::GetItem { expr, subscript } => {
                let object = self.eval(st, expr)?;
                let key = self.eval(st, subscript)?;
                self.get_item(st, &object, &key, span)
            }
            ExprKind::Call(call) => self.eval_call(st, call, None)?,
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval(st, item)?);
                }
                self.alloc(st, HeapObj::List(values))
            }
            ExprKind::Map(pairs) => {
                let mut entries = Vec::with_capacity(pairs.len());
                let mut unknown_key: Option<Causes> = None;
                for (key, value) in pairs {
                    let key_value = self.eval(st, key)?;
                    let item = self.eval(st, value)?;
                    match self.freeze(st, &key_value, key.span) {
                        // A repeated key keeps its first position and
                        // takes the last value, as in a dict.
                        Ok(key) => match entries.iter_mut().find(|(k, _)| *k == key) {
                            Some(entry) => entry.1 = item,
                            None => entries.push((key, item)),
                        },
                        Err(causes) => {
                            unknown_key = Some(match unknown_key {
                                Some(existing) => existing.union(&causes),
                                None => causes,
                            })
                        }
                    }
                }
                match unknown_key {
                    Some(causes) => self.alloc(st, HeapObj::Clobbered(causes)),
                    None => self.alloc(st, HeapObj::Map(entries)),
                }
            }
        };
        if !self.var_frames.is_empty()
            && let AValue::Unknown(causes) = &value
            && causes
                .iter()
                .any(|cause| matches!(cause, DependencyReason::UnsupportedJinjaConstruct { .. }))
        {
            self.record_used(st, causes);
        }
        Ok(value)
    }

    /// Evaluate two alternatives selected by an unknown condition, each on
    /// its own copy of the state, and merge the states and the values. An
    /// alternative is either an expression or an already known value.
    pub(super) fn eval_alternatives(
        &mut self,
        st: &mut State,
        causes: &Causes,
        mut value: Option<AValue>,
        first: Option<&Expr>,
        second: Option<&Expr>,
    ) -> Ev<AValue> {
        let original = st.path.clone();
        let mut results: Vec<(State, AValue)> = Vec::new();
        let mut exits: Vec<Signal> = Vec::new();
        for alternative in [first, second] {
            let mut state = st.clone();
            state.path = state.path.union(causes);
            let result = match alternative {
                Some(expr) => self.eval(&mut state, expr),
                None => Ok(value.take().unwrap_or_else(AValue::undefined)),
            };
            match result {
                Ok(v) => results.push((state, v)),
                Err(Signal::Budget) => return Err(Signal::Budget),
                Err(signal) => exits.push(signal),
            }
        }
        if !exits.is_empty() {
            // A `return` or compiler error inside a conditional expression
            // is not modelled precisely.
            let reason = self.unsupported(
                "return in a conditional expression",
                Span::default_for_template(),
            );
            self.record_used(st, &reason.union(causes));
            if results.is_empty() {
                return Err(exits.remove(0));
            }
        }
        let mut merged: Option<(State, AValue)> = None;
        for (state, v) in results {
            merged = Some(match merged {
                None => (state, v),
                Some((other, other_value)) => {
                    let value = merge_value(&other_value, &v, causes);
                    (merge_states(other, state, causes), value)
                }
            });
        }
        let (mut state, v) = merged.expect("at least one alternative");
        state.path = original;
        *st = state;
        Ok(v)
    }

    pub(super) fn freeze_all(
        &self,
        st: &State,
        values: &[AValue],
        span: Span,
    ) -> Result<Vec<Value>, Causes> {
        self.freeze_all_with(st, values, span, false)
    }

    /// [`Self::freeze_all`] with [`Self::freeze_shared`].
    pub(super) fn freeze_all_shared(
        &self,
        st: &State,
        values: &[AValue],
        span: Span,
    ) -> Result<Vec<Value>, Causes> {
        self.freeze_all_with(st, values, span, true)
    }

    fn freeze_all_with(
        &self,
        st: &State,
        values: &[AValue],
        span: Span,
        shared: bool,
    ) -> Result<Vec<Value>, Causes> {
        let mut out = Vec::with_capacity(values.len());
        let mut unknown: Option<Causes> = None;
        for value in values {
            match self.freeze_at(st, value, span, 0, shared) {
                Ok(v) => out.push(v),
                Err(causes) => {
                    unknown = Some(match unknown {
                        Some(existing) => existing.union(&causes),
                        None => causes,
                    })
                }
            }
        }
        match unknown {
            Some(causes) => Err(causes),
            None => Ok(out),
        }
    }

    pub(super) fn binary(
        &mut self,
        st: &mut State,
        op: BinOp,
        left: &AValue,
        right: &AValue,
        span: Span,
    ) -> AValue {
        if matches!(op, BinOp::In | BinOp::NotIn)
            && let Some(contains) = string_membership(st, left, right)
        {
            return AValue::Known(Value::from(contains == (op == BinOp::In)));
        }
        let inputs = [left.clone(), right.clone()];
        match self.freeze_all_shared(st, &inputs, span) {
            Ok(values) => match known::binary(op, &values[0], &values[1]) {
                Ok(value) => {
                    // Fusion concatenates two immutable sequences, such as
                    // ranges, into an immutable one.
                    let mutable_input = inputs.iter().any(|input| {
                        matches!(input, AValue::Heap(id) if matches!(st.heap.get(id), Some(HeapObj::List(_))))
                    });
                    let value = if mutable_input {
                        computed_list(value)
                    } else {
                        value
                    };
                    self.adopt_result(st, value, span)
                }
                Err(_) => {
                    AValue::Unknown(self.unsupported(format!("operator {}", op.symbol()), span))
                }
            },
            Err(causes) => AValue::Unknown(causes),
        }
    }

    pub(super) fn lookup(&self, st: &State, name: &str, span: Span) -> AValue {
        if let Some(value) = st.lookup(name) {
            return value.clone();
        }
        self.global(name, span).unwrap_or_else(AValue::undefined)
    }

    /// A name that is not bound in the template.
    pub(super) fn global(&self, name: &str, span: Span) -> Option<AValue> {
        if let Some(def) = self.library.get(name) {
            return Some(AValue::Macro(MacroRef {
                def: def.clone(),
                closure: Closure::Project,
            }));
        }
        let builtin = match name {
            "ref" => Builtin::Ref,
            "source" => Builtin::Source,
            "var" => Builtin::Var,
            "env_var" => Builtin::EnvVar,
            "config" => Builtin::Config,
            "is_incremental" => Builtin::IsIncremental,
            "return" => Builtin::Return,
            "log" | "print" => Builtin::Noop,
            "run_query" => Builtin::RunQuery,
            "load_result" => Builtin::LoadResult,
            "statement" => Builtin::Statement,
            "namespace" => Builtin::Namespace,
            "range" => Builtin::Minijinja("range"),
            "dict" => Builtin::Minijinja("dict"),
            "zip"
            | "tojson"
            | "fromjson"
            | "toyaml"
            | "fromyaml"
            | "local_md5"
            | "set"
            | "set_strict"
            | "as_bool"
            | "as_number"
            | "as_text"
            | "as_native"
            | "diff_of_two_dicts"
            | "try_or_compiler_error" => Builtin::Unmodelled(
                // Leak-free: the names above are static.
                match name {
                    "zip" => "zip",
                    "tojson" => "tojson",
                    "fromjson" => "fromjson",
                    "toyaml" => "toyaml",
                    "fromyaml" => "fromyaml",
                    "local_md5" => "local_md5",
                    "set" => "set",
                    "set_strict" => "set_strict",
                    "as_bool" => "as_bool",
                    "as_number" => "as_number",
                    "as_text" => "as_text",
                    "as_native" => "as_native",
                    "diff_of_two_dicts" => "diff_of_two_dicts",
                    _ => "try_or_compiler_error",
                },
            ),
            // dbt collects dependencies while parsing, when `execute` is false.
            "execute" => return Some(AValue::Known(Value::from(false))),
            "dbt_version" => {
                return Some(AValue::Unknown(self.runtime(
                    RuntimeValueSource::DbtVersion,
                    None,
                    span,
                )));
            }
            "invocation_id" => {
                return Some(AValue::Unknown(self.runtime(
                    RuntimeValueSource::InvocationId,
                    None,
                    span,
                )));
            }
            "run_started_at" => {
                return Some(AValue::Unknown(self.runtime(
                    RuntimeValueSource::RunStartedAt,
                    None,
                    span,
                )));
            }
            "target"
            | "adapter"
            | "this"
            | "model"
            | "graph"
            | "flags"
            | "api"
            | "modules"
            | "exceptions"
            | "builtins"
            | "context"
            | "database"
            | "schema"
            | "selected_resources"
            | "invocation_args_dict"
            | "project_name" => {
                let root: &'static str = match name {
                    "target" => "target",
                    "adapter" => "adapter",
                    "this" => "this",
                    "model" => "model",
                    "graph" => "graph",
                    "flags" => "flags",
                    "api" => "api",
                    "modules" => "modules",
                    "exceptions" => "exceptions",
                    "builtins" => "builtins",
                    "context" => "context",
                    "database" => "database",
                    "schema" => "schema",
                    "selected_resources" => "selected_resources",
                    "invocation_args_dict" => "invocation_args_dict",
                    _ => "project_name",
                };
                return Some(AValue::Context(ContextObject {
                    root,
                    path: Arc::from(root),
                }));
            }
            _ => return None,
        };
        Some(AValue::Builtin(builtin))
    }

    pub(super) fn eval_attribute_receiver(&mut self, st: &mut State, expr: &Expr) -> Ev<AValue> {
        if let ExprKind::Var(name) = &expr.kind {
            self.tick(Some(expr.span))?;
            return Ok(st
                .lookup(name)
                .cloned()
                .or_else(|| self.global(name, expr.span))
                .unwrap_or_else(|| AValue::MacroNamespace(name.clone())));
        }
        self.eval(st, expr)
    }

    pub(super) fn get_attr(&self, st: &State, object: &AValue, name: &str, span: Span) -> AValue {
        match object {
            AValue::Known(value) => match value.get_attr(name) {
                Ok(value) => AValue::Known(value),
                Err(_) => AValue::undefined(),
            },
            AValue::Unknown(causes) => AValue::Unknown(causes.clone()),
            AValue::Heap(id) => match st.heap.get(id) {
                Some(HeapObj::Namespace(attrs)) => {
                    attrs.get(name).cloned().unwrap_or_else(AValue::undefined)
                }
                Some(HeapObj::Map(entries)) => entries
                    .iter()
                    .find(|(key, _)| key.as_str() == Some(name))
                    .map(|(_, value)| value.clone())
                    .unwrap_or_else(AValue::undefined),
                Some(HeapObj::Clobbered(causes)) => AValue::Unknown(causes.clone()),
                _ => AValue::undefined(),
            },
            AValue::Relation => AValue::Unknown(self.relation_causes(Some(name), span)),
            AValue::Context(object) => {
                match (object.root, object.path.as_ref() == object.root, name) {
                    ("adapter", true, "dispatch") => AValue::Builtin(Builtin::Dispatch),
                    ("exceptions", true, "raise_compiler_error" | "raise_not_implemented") => {
                        AValue::Builtin(Builtin::RaiseCompilerError)
                    }
                    ("exceptions", true, "warn") => AValue::Builtin(Builtin::Noop),
                    ("builtins", true, "ref") => AValue::Builtin(Builtin::Ref),
                    ("builtins", true, "source") => AValue::Builtin(Builtin::Source),
                    ("builtins", true, "config") => AValue::Builtin(Builtin::Config),
                    // Objects whose methods are called keep their path; the
                    // attributes of the others are data, unknown where they are
                    // read.
                    (
                        "adapter" | "modules" | "api" | "exceptions" | "builtins" | "context",
                        _,
                        _,
                    ) => AValue::Context(ContextObject {
                        root: object.root,
                        path: Arc::from(format!("{}.{}", object.path, name)),
                    }),
                    _ => AValue::Unknown(self.context_causes(
                        &ContextObject {
                            root: object.root,
                            path: Arc::from(format!("{}.{}", object.path, name)),
                        },
                        span,
                    )),
                }
            }
            AValue::Builtin(Builtin::Config) if matches!(name, "get" | "require") => {
                AValue::Builtin(Builtin::ConfigGet)
            }
            AValue::MacroNamespace(namespace) => self
                .library
                .namespace_member(namespace, name)
                .unwrap_or_else(|| {
                    AValue::Unknown(Causes::one(DependencyReason::UnsupportedMacroNamespace {
                        namespace: namespace.to_string(),
                        macro_name: name.to_owned(),
                        at: self.site(span),
                    }))
                }),
            AValue::Macro(_) | AValue::Builtin(_) => AValue::undefined(),
        }
    }

    pub(super) fn get_item(&self, st: &State, object: &AValue, key: &AValue, span: Span) -> AValue {
        if let AValue::Heap(id) = object {
            match (st.heap.get(id), self.freeze(st, key, span)) {
                (Some(HeapObj::Clobbered(causes)), _) => return AValue::Unknown(causes.clone()),
                (_, Err(causes)) => return AValue::Unknown(causes),
                (Some(HeapObj::List(items)), Ok(key)) => {
                    let len = items.len() as i64;
                    return key
                        .as_i64()
                        .map(|i| if i < 0 { i + len } else { i })
                        .and_then(|i| usize::try_from(i).ok())
                        .and_then(|i| items.get(i).cloned())
                        .unwrap_or_else(AValue::undefined);
                }
                (Some(HeapObj::Map(entries)), Ok(key)) => {
                    return entries
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_else(AValue::undefined);
                }
                (Some(HeapObj::Namespace(attrs)), Ok(key)) => {
                    return key
                        .as_str()
                        .and_then(|k| attrs.get(k).cloned())
                        .unwrap_or_else(AValue::undefined);
                }
                (None, _) => return AValue::undefined(),
            }
        }
        if let AValue::Context(_) | AValue::Relation = object
            && let Ok(key) = self.freeze(st, key, span)
            && let Some(name) = key.as_str()
        {
            return self.get_attr(st, object, name, span);
        }
        match self.freeze_all(st, &[object.clone(), key.clone()], span) {
            Ok(values) => known::get_item(&values[0], &values[1])
                .map(AValue::Known)
                .unwrap_or_else(|_| AValue::undefined()),
            Err(causes) => AValue::Unknown(causes),
        }
    }

    pub(super) fn test(&self, st: &State, name: &str, values: &[AValue], span: Span) -> AValue {
        let subject = &values[0];
        let is_known = matches!(subject, AValue::Known(_));
        match name {
            "defined" | "undefined" | "none" if !is_known => {
                let answer = match subject {
                    AValue::Unknown(causes) => return AValue::Unknown(causes.clone()),
                    _ => name == "defined",
                };
                return AValue::Known(Value::from(answer));
            }
            "callable" if matches!(subject, AValue::Macro(_) | AValue::Builtin(_)) => {
                return AValue::Known(Value::from(true));
            }
            _ => {}
        }
        match self.freeze_all(st, values, span) {
            Ok(values) => match known::test(name, &values) {
                Some(Ok(result)) => AValue::Known(Value::from(result)),
                Some(Err(_)) | None => {
                    AValue::Unknown(self.unsupported(format!("test:{name}"), span))
                }
            },
            Err(causes) => AValue::Unknown(causes),
        }
    }

    pub(super) fn apply_filter_expr(
        &mut self,
        st: &mut State,
        filter: &Expr,
        input: AValue,
    ) -> Ev<AValue> {
        match &filter.kind {
            ExprKind::Filter { name, expr, args } => {
                let input = match expr {
                    Some(inner) => self.apply_filter_expr(st, inner, input)?,
                    None => input,
                };
                self.apply_filter(st, name, input, args, filter.span)
            }
            _ => Ok(AValue::Unknown(
                self.unsupported("filter block", filter.span),
            )),
        }
    }

    pub(super) fn apply_filter(
        &mut self,
        st: &mut State,
        name: &str,
        input: AValue,
        args: &[Arg],
        span: Span,
    ) -> Ev<AValue> {
        let mut values = vec![input];
        let mut kwargs = Vec::new();
        for arg in args {
            match arg {
                Arg::Pos(e) => values.push(self.eval(st, e)?),
                Arg::Kw(key, e) => kwargs.push((key.clone(), self.eval(st, e)?)),
                Arg::PosSplat(e) | Arg::KwSplat(e) => {
                    values.push(self.eval(st, e)?);
                    let causes = self.unsupported("splat argument", span);
                    return Ok(self.unmodelled_filter(st, &values, &kwargs, causes));
                }
            }
        }
        // minijinja's `chain` gives a view that reads its inputs when it is
        // used, so later changes to them show through it. Fusion has no such
        // filter, so it is not modelled, like a filter minijinja does not
        // know, whether or not its arguments are known.
        if name == "chain" || !known::has_filter(name) {
            let causes = self.unsupported(format!("filter:{name}"), span);
            return Ok(self.unmodelled_filter(st, &values, &kwargs, causes));
        }
        if name == "default" || name == "d" {
            // Jinja2 also accepts `default_value=` and `boolean=`; minijinja
            // takes them positionally.
            for (index, keyword) in [(1, "default_value"), (2, "boolean")] {
                if let Some(pos) = kwargs.iter().position(|(k, _)| k.as_ref() == keyword) {
                    let value = kwargs.remove(pos).1;
                    while values.len() < index {
                        values.push(AValue::Known(Value::from("")));
                    }
                    values.insert(index.min(values.len()), value);
                }
            }
        }
        let positional_count = values.len() - 1;
        if (name == "default" || name == "d")
            && !matches!(values[0], AValue::Known(_) | AValue::Unknown(_))
        {
            // A value that is not a plain value (a container, a relation) is
            // defined. With `boolean`, a falsy one is replaced. Known inputs
            // go to minijinja below.
            let fallback = values
                .get(1)
                .cloned()
                .unwrap_or_else(|| AValue::Known(Value::from("")));
            let boolean = match values.get(2) {
                None => Truth::Known(false),
                Some(flag) => self.truth(st, flag, span),
            };
            return Ok(match boolean {
                Truth::Known(false) => values[0].clone(),
                Truth::Known(true) => match self.truth(st, &values[0], span) {
                    Truth::Known(true) => values[0].clone(),
                    Truth::Known(false) => fallback,
                    Truth::Unknown(causes) => merge_value(&values[0], &fallback, &causes),
                },
                Truth::Unknown(causes) => merge_value(&values[0], &fallback, &causes),
            });
        }
        if (name == "default" || name == "d")
            && positional_count == 1
            && let AValue::Known(value) = &values[0]
            && value.is_undefined()
        {
            // Keep the default's own form (it may be a container).
            return Ok(values[1].clone());
        }
        let mut frozen = match self.freeze_all_shared(st, &values, span) {
            Ok(frozen) => frozen,
            Err(causes) => return Ok(AValue::Unknown(causes)),
        };
        if !kwargs.is_empty() {
            let mut pairs = Vec::new();
            for (key, value) in &kwargs {
                match self.freeze_shared(st, value, span) {
                    Ok(value) => pairs.push((Value::from(key.as_ref()), value)),
                    Err(causes) => return Ok(AValue::Unknown(causes)),
                }
            }
            frozen.push(Value::from(minijinja::value::Kwargs::from_iter(
                pairs
                    .into_iter()
                    .map(|(k, v)| (k.as_str().unwrap_or_default().to_string(), v)),
            )));
        }
        Ok(match known::filter(name, &frozen) {
            Some(Ok(value)) => self.adopt_result(st, value, span),
            Some(Err(_)) | None => {
                AValue::Unknown(self.unsupported(format!("filter:{name}"), span))
            }
        })
    }

    /// The unknown result of a filter that is not modelled. Like an unknown
    /// call, it may return or change any container of its arguments, so
    /// those become unknown too.
    fn unmodelled_filter(
        &self,
        st: &mut State,
        values: &[AValue],
        kwargs: &[(Arc<str>, AValue)],
        causes: Causes,
    ) -> AValue {
        let inputs: Vec<AValue> = values
            .iter()
            .cloned()
            .chain(kwargs.iter().map(|(_, value)| value.clone()))
            .collect();
        st.clobber_reachable(&inputs, &causes);
        AValue::Unknown(causes)
    }
}

/// Check a string against a heap list or string-keyed map without building
/// MiniJinja copies. Containment uses value equality for a list and key
/// lookup for a map. Still check the entire container: an unfreezable value
/// makes the original operation unknown even when the string was found.
fn string_membership(st: &State, needle: &AValue, container: &AValue) -> Option<bool> {
    let AValue::Known(needle) = needle else {
        return None;
    };
    if needle.kind() != ValueKind::String {
        return None;
    }
    let text = needle.as_str()?;
    let AValue::Heap(id) = container else {
        return None;
    };
    match st.heap.get(id)? {
        HeapObj::List(items) => items.iter().try_fold(false, |found, item| match item {
            AValue::Known(value) => Some(found || value == needle),
            _ => None,
        }),
        HeapObj::Map(entries) => entries.iter().try_fold(false, |found, (key, value)| {
            if key.kind() != ValueKind::String || !can_freeze(st, value, 1) {
                return None;
            }
            Some((key.as_str()? == text) || found)
        }),
        _ => None,
    }
}

/// Prove that freezing this value would succeed without materializing it.
/// Leave unknown values and depth failures to the regular diagnostic path.
fn can_freeze(st: &State, value: &AValue, depth: usize) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    match value {
        AValue::Known(_) => true,
        AValue::Heap(id) => {
            let nested = |value| can_freeze(st, value, depth + 1);
            match st.heap.get(id) {
                Some(HeapObj::List(items)) => items.iter().all(nested),
                Some(HeapObj::Map(entries)) => entries.iter().all(|(_, value)| nested(value)),
                Some(HeapObj::Namespace(attrs)) => attrs.values().all(nested),
                Some(HeapObj::Clobbered(_)) | None => false,
            }
        }
        _ => false,
    }
}

/// A slice or a concatenation of sequences is a new list in Fusion, which
/// minijinja computes lazily.
pub(super) fn computed_list(value: Value) -> Value {
    if value.kind() != ValueKind::Iterable {
        return value;
    }
    match value.try_iter() {
        Ok(items) => Value::from(items.take(MAX_ITEMS + 1).collect::<Vec<_>>()),
        Err(_) => value,
    }
}
