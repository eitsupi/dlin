//! Call resolution, argument binding and macro invocation.

use super::*;

impl<'a> Interp<'a> {
    // ----- calls -------------------------------------------------------

    /// The source text of an expression, for naming a callee.
    pub(super) fn text(&self, expr: &Expr) -> Option<String> {
        (expr.span.source == SourceId::Template)
            .then(|| {
                self.source
                    .get(expr.span.start as usize..expr.span.end as usize)
            })
            .flatten()
            .map(str::to_string)
    }

    pub(super) fn is_bound(&self, st: &State, name: &str) -> bool {
        st.lookup(name).is_some() || self.global(name, Span::default_for_template()).is_some()
    }

    pub(crate) fn eval_call(
        &mut self,
        st: &mut State,
        call: &Call,
        caller: Option<AValue>,
    ) -> Ev<AValue> {
        let span = call.span;
        // A call to a name that does not exist, or through a namespace that
        // is not a variable, hides whatever the callee does.
        match &call.callee.kind {
            ExprKind::Var(name) if !self.is_bound(st, name) => {
                let causes = Causes::one(DependencyReason::UnresolvedCall {
                    callee: Some(name.to_string()),
                    at: self.site(span),
                });
                return self.opaque_call(st, call, &causes);
            }
            ExprKind::GetAttr { expr, name } => {
                if let ExprKind::Var(namespace) = &expr.kind
                    && !self.is_bound(st, namespace)
                {
                    let causes = Causes::one(DependencyReason::UnsupportedMacroNamespace {
                        namespace: namespace.to_string(),
                        macro_name: name.to_string(),
                        at: self.site(span),
                    });
                    return self.opaque_call(st, call, &causes);
                }
                let receiver = self.eval(st, expr)?;
                match &receiver {
                    AValue::Context(_) | AValue::Builtin(_) => {
                        let callee = self.get_attr(st, &receiver, name, expr.span);
                        let (args, kwargs) = self.eval_args(st, &call.args, span)?;
                        return self.call_value(st, callee, args, kwargs, caller, call);
                    }
                    _ => {
                        let (args, kwargs) = self.eval_args(st, &call.args, span)?;
                        // A macro stored in a namespace or a map is called,
                        // not treated as a method.
                        if let AValue::Heap(_) = &receiver {
                            let attribute = self.get_attr(st, &receiver, name, span);
                            if matches!(attribute, AValue::Macro(_) | AValue::Builtin(_)) {
                                return self.call_value(st, attribute, args, kwargs, caller, call);
                            }
                        }
                        // A method of a value that may be a callable the
                        // analysis could not follow is an unknown callee.
                        // Unknown data (vars, target, query results) has only
                        // data methods.
                        if let AValue::Unknown(causes) = &receiver
                            && may_be_callable(causes)
                        {
                            self.record_used(st, causes);
                            st.clobber_reachable(
                                args.iter().chain(kwargs.iter().map(|(_, v)| v)),
                                causes,
                            );
                            return Ok(AValue::Unknown(causes.clone()));
                        }
                        return Ok(self.method(st, receiver, name, args, kwargs, span));
                    }
                }
            }
            _ => {}
        }
        let callee = self.eval(st, &call.callee)?;
        if matches!(callee, AValue::Builtin(Builtin::Statement)) && caller.is_some() {
            // dbt's statement only calls its body when executing, so the
            // body is not part of the parse-time dependencies.
            self.eval_args(st, &call.args, span)?;
            return Ok(AValue::Known(Value::from("")));
        }
        let (args, kwargs) = self.eval_args(st, &call.args, span)?;
        self.call_value(st, callee, args, kwargs, caller, call)
    }

    /// Evaluate the arguments of a call whose callee is unknown, record the
    /// reason, and return an unknown value.
    pub(super) fn opaque_call(
        &mut self,
        st: &mut State,
        call: &Call,
        causes: &Causes,
    ) -> Ev<AValue> {
        let (args, kwargs) = self.eval_args(st, &call.args, call.span)?;
        self.record_used(st, causes);
        st.clobber_reachable(args.iter().chain(kwargs.iter().map(|(_, v)| v)), causes);
        Ok(AValue::Unknown(causes.clone()))
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn eval_args(
        &mut self,
        st: &mut State,
        args: &[Arg],
        span: Span,
    ) -> Ev<(Vec<AValue>, Vec<(Arc<str>, AValue)>)> {
        let mut positional = Vec::new();
        let mut keyword = Vec::new();
        for arg in args {
            match arg {
                Arg::Pos(e) => positional.push(self.eval(st, e)?),
                Arg::Kw(name, e) => keyword.push((name.clone(), self.eval(st, e)?)),
                Arg::PosSplat(e) => {
                    let value = self.eval(st, e)?;
                    match self.items(st, &value, span) {
                        Ok(items) => positional.extend(items),
                        Err(causes) => positional.push(AValue::Unknown(causes)),
                    }
                }
                Arg::KwSplat(e) => {
                    let value = self.eval(st, e)?;
                    match &value {
                        AValue::Heap(id) => match st.heap.get(id) {
                            Some(HeapObj::Map(entries)) => {
                                for (key, item) in entries.clone() {
                                    if let Some(key) = key.as_str() {
                                        keyword.push((Arc::from(key), item));
                                    }
                                }
                            }
                            Some(HeapObj::Clobbered(causes)) => {
                                keyword.push((Arc::from("**"), AValue::Unknown(causes.clone())))
                            }
                            _ => keyword.push((
                                Arc::from("**"),
                                AValue::Unknown(self.unsupported("**kwargs", span)),
                            )),
                        },
                        AValue::Unknown(causes) => {
                            keyword.push((Arc::from("**"), AValue::Unknown(causes.clone())))
                        }
                        AValue::Known(map) if map.kind() == ValueKind::Map => {
                            if let Ok(keys) = map.try_iter() {
                                for key in keys {
                                    if let (Some(name), Ok(item)) =
                                        (key.as_str(), map.get_item(&key))
                                    {
                                        keyword.push((Arc::from(name), AValue::Known(item)));
                                    }
                                }
                            }
                        }
                        _ => keyword.push((
                            Arc::from("**"),
                            AValue::Unknown(self.unsupported("**kwargs", span)),
                        )),
                    }
                }
            }
        }
        Ok((positional, keyword))
    }

    pub(super) fn call_value(
        &mut self,
        st: &mut State,
        callee: AValue,
        args: Vec<AValue>,
        kwargs: Vec<(Arc<str>, AValue)>,
        caller: Option<AValue>,
        call: &Call,
    ) -> Ev<AValue> {
        let span = call.span;
        match callee {
            AValue::Macro(def) => self.invoke(st, def, args, kwargs, caller, span),
            AValue::Builtin(builtin) => self.builtin(st, builtin, args, kwargs, call),
            AValue::Unknown(causes) => {
                // The callee is chosen by an unknown value.
                let hides_call = causes.iter().all(|reason| {
                    matches!(
                        reason,
                        DependencyReason::UnsupportedDispatch { .. }
                            | DependencyReason::UnsupportedMacroNamespace { .. }
                            | DependencyReason::UnresolvedCall { .. }
                    )
                });
                let causes = if hides_call {
                    causes
                } else {
                    causes.union(&Causes::one(DependencyReason::UnresolvedCall {
                        callee: self.text(&call.callee),
                        at: self.site(span),
                    }))
                };
                self.record_used(st, &causes);
                st.clobber_reachable(args.iter().chain(kwargs.iter().map(|(_, v)| v)), &causes);
                Ok(AValue::Unknown(causes))
            }
            AValue::Context(object) => {
                // `context` and `builtins` give access to macros and other
                // functions, so calling through them is an unknown call. The
                // other context objects (adapter, modules, api, ...) only
                // compute or query data.
                if matches!(object.root, "context" | "builtins") {
                    let causes = Causes::one(DependencyReason::UnresolvedCall {
                        callee: Some(object.path.to_string()),
                        at: self.site(span),
                    });
                    self.record_used(st, &causes);
                    st.clobber_reachable(args.iter().chain(kwargs.iter().map(|(_, v)| v)), &causes);
                    return Ok(AValue::Unknown(causes));
                }
                Ok(AValue::Unknown(self.context_causes(&object, span)))
            }
            AValue::Known(_) | AValue::Heap(_) | AValue::Relation => Ok(AValue::Unknown(
                self.unsupported("calling a non-callable value", span),
            )),
        }
    }

    pub(super) fn invoke(
        &mut self,
        st: &mut State,
        target: MacroRef,
        args: Vec<AValue>,
        kwargs: Vec<(Arc<str>, AValue)>,
        caller: Option<AValue>,
        span: Span,
    ) -> Ev<AValue> {
        if self.depth >= MAX_DEPTH {
            let causes = Causes::one(DependencyReason::AnalysisLimitReached {
                limit: AnalysisLimit::RecursionDepth,
                at: self.site(span),
            });
            self.record_used(st, &causes);
            st.clobber_reachable(args.iter().chain(kwargs.iter().map(|(_, v)| v)), &causes);
            return Ok(AValue::Unknown(causes));
        }
        let entering = self.root_call.is_none();
        if entering {
            self.root_call = Some(span);
            // The template's variables cannot change until this call
            // returns, so its macros read them from here.
            self.root_scopes = st.scopes.clone();
        }
        self.depth += 1;
        let result = self.invoke_inner(st, &target, args, kwargs, caller);
        self.depth -= 1;
        if entering {
            self.root_call = None;
        }
        result
    }

    pub(super) fn invoke_inner(
        &mut self,
        st: &mut State,
        target: &MacroRef,
        args: Vec<AValue>,
        mut kwargs: Vec<(Arc<str>, AValue)>,
        caller: Option<AValue>,
    ) -> Ev<AValue> {
        let def = &target.def;
        let base = match &target.closure {
            Closure::Project => Scope::new(),
            Closure::TemplateRoot => def
                .names
                .iter()
                .filter_map(|name| {
                    self.root_scopes
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(name))
                        .map(|value| (name.clone(), value.clone()))
                })
                .collect(),
            Closure::Captured(scope) => scope.as_ref().clone(),
        };
        let mut params = Scope::new();
        if !matches!(target.closure, Closure::Project) {
            params.insert(def.name.clone(), AValue::Macro(target.clone()));
        }
        let scopes = vec![base, params];
        let heap = std::mem::take(&mut st.heap);
        let mut frame = State::new(scopes, heap, Output::Known(String::new()), st.path.clone());
        let mut positional = args.into_iter();
        let mut bind_error = None;
        // An unknown `**kwargs` may supply any parameter that is not given
        // positionally.
        let unknown_kwargs = kwargs
            .iter()
            .find(|(name, _)| name.as_ref() == "**")
            .and_then(|(_, value)| value.causes().cloned());
        for (index, param) in def.params.iter().enumerate() {
            let value = if let Some(value) = positional.next() {
                value
            } else if let Some(causes) = &unknown_kwargs {
                AValue::Unknown(causes.clone())
            } else if let Some(pos) = kwargs.iter().position(|(name, _)| name == param) {
                kwargs.remove(pos).1
            } else if let Some(default) = def.default_for(index) {
                match self.eval(&mut frame, default) {
                    Ok(value) => value,
                    Err(signal) => {
                        bind_error = Some(signal);
                        break;
                    }
                }
            } else {
                AValue::undefined()
            };
            frame.assign(param.clone(), value);
        }
        if let Some(signal) = bind_error {
            st.heap = frame.heap;
            return Err(signal);
        }
        let rest: Vec<AValue> = positional.collect();
        let varargs = self.alloc(&mut frame, HeapObj::List(rest));
        frame.assign(Arc::from("varargs"), varargs);
        let entries = kwargs
            .into_iter()
            .map(|(name, value)| (Value::from(name.as_ref()), value))
            .collect();
        let kwargs_value = self.alloc(&mut frame, HeapObj::Map(entries));
        frame.assign(Arc::from("kwargs"), kwargs_value);
        if let Some(caller) = caller {
            frame.assign(Arc::from("caller"), caller);
        }
        let flow = self.exec_block(frame, &def.body)?;
        let fallback = Causes::default();
        let normal = merge_opt(
            flow.normal,
            merge_opt(flow.brk, flow.cont, &fallback),
            &fallback,
        )
        .map(|mut state| {
            let value = std::mem::replace(&mut state.out, Output::Discard).into_value();
            (state, value)
        });
        match merge_ret(normal, flow.ret, &fallback) {
            Some((state, value)) => {
                st.heap = state.heap;
                Ok(value)
            }
            None => Err(Signal::Abort),
        }
    }
}
