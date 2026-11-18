//! Statement execution, branching and loop control.

use super::*;

impl<'a> Interp<'a> {
    // ----- statements -----------------------------------------------------

    pub(crate) fn run_template(&mut self, body: &'a [Stmt]) -> Ev<()> {
        self.template = Some(body);
        let state = State::new(
            vec![Scope::new()],
            BTreeMap::new(),
            Output::Discard,
            Causes::default(),
        );
        match self.exec_block(state, body) {
            Ok(_) | Err(Signal::Abort) | Err(Signal::Return(_)) => Ok(()),
            Err(Signal::Budget) => Err(Signal::Budget),
        }
    }

    pub(super) fn exec_block(&mut self, state: State, body: &[Stmt]) -> Ev<Flow> {
        let mut flow = Flow::normal(state);
        let fallback = Causes::default();
        for stmt in body {
            let Some(current) = flow.normal.take() else {
                break;
            };
            let next = self.exec_stmt(current, stmt)?;
            flow.normal = next.normal;
            flow.ret = merge_ret(flow.ret, next.ret, &fallback);
            flow.brk = merge_opt(flow.brk, next.brk, &fallback);
            flow.cont = merge_opt(flow.cont, next.cont, &fallback);
        }
        Ok(flow)
    }

    pub(super) fn exec_stmt(&mut self, mut st: State, stmt: &Stmt) -> Ev<Flow> {
        self.tick(None)?;
        let result = self.exec_stmt_inner(&mut st, stmt);
        match result {
            Ok(Some(flow)) => Ok(flow),
            Ok(None) => Ok(Flow::normal(st)),
            Err(Signal::Return(value)) => Ok(Flow {
                ret: Some((st, value)),
                ..Flow::default()
            }),
            Err(Signal::Abort) => Ok(Flow::default()),
            Err(Signal::Budget) => Err(Signal::Budget),
        }
    }

    /// Execute one statement. `Ok(None)` continues with `st`.
    pub(super) fn exec_stmt_inner(&mut self, st: &mut State, stmt: &Stmt) -> Ev<Option<Flow>> {
        match stmt {
            Stmt::Emit(expr) => {
                let value = self.eval(st, expr)?;
                self.emit(st, &value, expr.span);
            }
            Stmt::Raw(text) => st.out.push_str(text),
            Stmt::If {
                cond,
                then,
                otherwise,
            } => {
                let value = self.eval(st, cond)?;
                let state = st.clone();
                return match self.truth(&state, &value, cond.span) {
                    Truth::Known(true) => {
                        self.scan_skipped(otherwise);
                        self.exec_block(state, then).map(Some)
                    }
                    Truth::Known(false) => {
                        self.scan_skipped(then);
                        self.exec_block(state, otherwise).map(Some)
                    }
                    Truth::Unknown(causes) => {
                        self.branch(state, &causes, then, otherwise).map(Some)
                    }
                };
            }
            Stmt::For {
                target,
                iter,
                filter,
                recursive,
                body,
                else_body,
                span,
            } => {
                let iterable = self.eval(st, iter)?;
                let state = st.clone();
                return self
                    .exec_for(
                        state,
                        target,
                        iterable,
                        filter.as_ref(),
                        *recursive,
                        body,
                        else_body,
                        *span,
                    )
                    .map(Some);
            }
            Stmt::With { assignments, body } => {
                let mut state = st.clone();
                state.scopes.push(Scope::new());
                for (target, expr) in assignments {
                    let value = self.eval(&mut state, expr)?;
                    self.assign(&mut state, target, value)?;
                }
                let mut flow = self.exec_block(state, body)?;
                pop_scope(&mut flow);
                return Ok(Some(flow));
            }
            Stmt::Set { target, expr } => {
                let value = self.eval(st, expr)?;
                self.assign(st, target, value)?;
            }
            Stmt::SetBlock {
                target,
                filter,
                body,
            } => {
                let (captured, mut flow) = self.capture(st, body)?;
                if let Some((mut state, value)) = captured {
                    let value = match filter {
                        Some(filter) => self.apply_filter_expr(&mut state, filter, value)?,
                        None => value,
                    };
                    self.assign(&mut state, target, value)?;
                    flow.normal = Some(state);
                }
                return Ok(Some(flow));
            }
            Stmt::FilterBlock { filter, body } => {
                let (captured, mut flow) = self.capture(st, body)?;
                if let Some((mut state, value)) = captured {
                    let value = self.apply_filter_expr(&mut state, filter, value)?;
                    self.emit(&mut state, &value, filter.span);
                    flow.normal = Some(state);
                }
                return Ok(Some(flow));
            }
            Stmt::AutoEscape { body } => {
                let state = st.clone();
                return self.exec_block(state, body).map(Some);
            }
            Stmt::Macro(def) => {
                let value = AValue::Macro(MacroRef {
                    def: def.clone(),
                    closure: self.closure_here(st, &def.names),
                });
                st.assign(def.name.clone(), value);
            }
            Stmt::CallBlock { call, caller } => {
                let caller = AValue::Macro(MacroRef {
                    def: caller.clone(),
                    closure: self.closure_here(st, &caller.names),
                });
                let value = self.eval_call(st, call, Some(caller))?;
                self.emit(st, &value, call.span);
            }
            Stmt::Do(call) => {
                self.eval_call(st, call, None)?;
            }
            Stmt::Break => {
                return Ok(Some(Flow {
                    brk: Some(st.clone()),
                    ..Flow::default()
                }));
            }
            Stmt::Continue => {
                return Ok(Some(Flow {
                    cont: Some(st.clone()),
                    ..Flow::default()
                }));
            }
            Stmt::Unsupported {
                construct,
                span,
                exprs,
                body,
            } => {
                let causes = self.unsupported(*construct, *span);
                self.record_used(st, &causes);
                let mut found = StaticCalls::default();
                for expr in exprs {
                    found.scan_expr(expr);
                }
                found.scan_stmts(body);
                // The calls are found without evaluating the construct, so
                // only the construct is located.
                let at = self.site(*span);
                for call in found.refs {
                    self.rec.add_ref(call, at.clone());
                }
                for call in found.sources {
                    self.rec.add_source(call, at.clone());
                }
            }
        }
        Ok(None)
    }

    /// The closure of a macro or caller defined in the current frame.
    pub(super) fn closure_here(&self, st: &State, names: &[Arc<str>]) -> Closure {
        if self.depth == 0 && st.scopes.len() == 1 {
            Closure::TemplateRoot
        } else {
            Closure::Captured(capture(st, names))
        }
    }

    pub(super) fn branch(
        &mut self,
        state: State,
        causes: &Causes,
        a: &[Stmt],
        b: &[Stmt],
    ) -> Ev<Flow> {
        let original = state.path.clone();
        let mut left = state.clone();
        left.path = left.path.union(causes);
        let mut right = state;
        right.path = right.path.union(causes);
        let fa = self.exec_block(left, a)?;
        let fb = self.exec_block(right, b)?;
        // An exit reached on both sides no longer depends on the condition;
        // an exit reached on one side only still does.
        let both = [
            fa.normal.is_some() && fb.normal.is_some(),
            fa.brk.is_some() && fb.brk.is_some(),
            fa.cont.is_some() && fb.cont.is_some(),
            fa.ret.is_some() && fb.ret.is_some(),
        ];
        let mut flow = merge_flows(fa, fb, causes);
        for (state, joined) in [
            (flow.normal.as_mut(), both[0]),
            (flow.brk.as_mut(), both[1]),
            (flow.cont.as_mut(), both[2]),
            (flow.ret.as_mut().map(|(state, _)| state), both[3]),
        ] {
            if let (Some(state), true) = (state, joined) {
                state.path = original.clone();
            }
        }
        Ok(flow)
    }

    /// Execute a block capturing its output as a value. Returns the state
    /// that continues normally with the captured value, and the other exits
    /// of the block (a `return` in the block returns from the enclosing
    /// macro), each with the enclosing output restored.
    pub(super) fn capture(
        &mut self,
        st: &State,
        body: &[Stmt],
    ) -> Ev<(Option<(State, AValue)>, Flow)> {
        let mut inner = st.clone();
        inner.out = Output::Known(String::new());
        let mut flow = self.exec_block(inner, body)?;
        let restore = |state: &mut State| state.out = st.out.clone();
        let captured = flow.normal.take().map(|mut state| {
            let value = std::mem::replace(&mut state.out, Output::Discard).into_value();
            restore(&mut state);
            (state, value)
        });
        for state in [
            flow.brk.as_mut(),
            flow.cont.as_mut(),
            flow.ret.as_mut().map(|(s, _)| s),
        ]
        .into_iter()
        .flatten()
        {
            restore(state);
        }
        Ok((captured, flow))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn exec_for(
        &mut self,
        st: State,
        target: &Target,
        iterable: AValue,
        filter: Option<&Expr>,
        recursive: bool,
        body: &[Stmt],
        else_body: &[Stmt],
        span: Span,
    ) -> Ev<Flow> {
        let items = if recursive {
            let causes = self.unsupported("recursive loop", span);
            self.record_used(&st, &causes);
            Err(causes)
        } else {
            self.items(&st, &iterable, span)
        };
        let items = match items {
            Ok(items) if items.len() > MAX_ITEMS => {
                let causes = Causes::one(DependencyReason::AnalysisLimitReached {
                    limit: AnalysisLimit::WorkBudget,
                    at: self.site(span),
                });
                self.record_used(&st, &causes);
                Err(causes)
            }
            other => other,
        };
        match items {
            Ok(items) => self.exec_known_loop(st, target, items, filter, body, else_body),
            Err(causes) => {
                self.exec_unknown_loop(st, target, &causes, filter, body, else_body, span)
            }
        }
    }

    pub(super) fn loop_object(index: usize, length: usize) -> AValue {
        AValue::Known(Value::from_pairs([
            ("index", Value::from(index + 1)),
            ("index0", Value::from(index)),
            ("first", Value::from(index == 0)),
            ("last", Value::from(index + 1 == length)),
            ("length", Value::from(length)),
            ("revindex", Value::from(length - index)),
            ("revindex0", Value::from(length - index - 1)),
        ]))
    }

    pub(super) fn exec_known_loop(
        &mut self,
        st: State,
        target: &Target,
        items: Vec<AValue>,
        filter: Option<&Expr>,
        body: &[Stmt],
        else_body: &[Stmt],
    ) -> Ev<Flow> {
        // Apply the loop filter first: `else` and `loop` describe the items
        // that pass it. An item whose filter result is unknown may or may
        // not be iterated.
        let mut passing: Vec<(AValue, Option<Causes>)> = Vec::new();
        for item in &items {
            let Some(filter) = filter else {
                passing.push((item.clone(), None));
                continue;
            };
            let mut probe = st.clone();
            probe.scopes.push(Scope::new());
            self.assign(&mut probe, target, item.clone())?;
            let value = self.eval(&mut probe, filter)?;
            probe.scopes.pop();
            if !probe.same(&st) {
                // A filter that changes state interleaves with the body in
                // ways the interpreter does not follow.
                let causes = self.unsupported("loop filter with side effects", filter.span);
                self.record_used(&st, &causes);
                return self.exec_unknown_loop(
                    st,
                    target,
                    &causes,
                    Some(filter),
                    body,
                    else_body,
                    filter.span,
                );
            }
            match self.truth(&probe, &value, filter.span) {
                Truth::Known(true) => passing.push((item.clone(), None)),
                Truth::Known(false) => {}
                Truth::Unknown(causes) => passing.push((item.clone(), Some(causes))),
            }
        }
        let uncertain: Option<Causes> = passing
            .iter()
            .filter_map(|(_, c)| c.clone())
            .reduce(|a, b| a.union(&b));
        if passing.is_empty() {
            return self.exec_block(st, else_body);
        }
        let fallback = Causes::default();
        let length = passing.len();
        let mut current = Some(st.clone());
        let mut exits = Flow::default();
        for (index, (item, maybe)) in passing.into_iter().enumerate() {
            let Some(mut state) = current.take() else {
                break;
            };
            state.scopes.push(Scope::new());
            self.assign(&mut state, target, item)?;
            let loop_info = match &uncertain {
                None => Self::loop_object(index, length),
                Some(causes) => AValue::Unknown(causes.clone()),
            };
            state.assign(Arc::from("loop"), loop_info);
            let mut flow = match maybe {
                None => self.exec_block(state, body)?,
                Some(causes) => self.branch(state, &causes, body, &[])?,
            };
            pop_scope(&mut flow);
            current = merge_opt(flow.normal, flow.cont, &fallback);
            exits.brk = merge_opt(exits.brk, flow.brk, &fallback);
            exits.ret = merge_ret(exits.ret, flow.ret, &fallback);
        }
        let mut normal = merge_opt(current, exits.brk.take(), &fallback);
        if let Some(causes) = &uncertain {
            // If no item passes the filter, `else` runs.
            let mut skipped = st;
            skipped.path = skipped.path.union(causes);
            let else_flow = self.exec_block(skipped, else_body)?;
            normal = merge_opt(normal, else_flow.normal, causes);
            exits.ret = merge_ret(exits.ret, else_flow.ret, causes);
        }
        exits.normal = normal;
        Ok(exits)
    }

    /// A loop whose iterations are unknown: iterate to a fixed point of the
    /// state, widening what keeps changing.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn exec_unknown_loop(
        &mut self,
        st: State,
        target: &Target,
        causes: &Causes,
        filter: Option<&Expr>,
        body: &[Stmt],
        else_body: &[Stmt],
        span: Span,
    ) -> Ev<Flow> {
        let entry = st;
        let mut acc = entry.clone();
        let mut exits = Flow::default();
        let mut stable = false;
        let mut filter_effects = Causes::default();
        for round in 0..=LOOP_ROUNDS {
            let mut state = acc.clone();
            state.path = state.path.union(causes);
            state.scopes.push(Scope::new());
            self.assign(&mut state, target, AValue::Unknown(causes.clone()))?;
            state.assign(Arc::from("loop"), AValue::Unknown(causes.clone()));
            if let Some(filter) = filter {
                // Whether an item passes is as unknown as the items; the body
                // runs on the path of both.
                let before_filter = state.clone();
                let value = self.eval(&mut state, filter)?;
                let truth = if state.same(&before_filter) {
                    self.truth(&state, &value, filter.span)
                } else {
                    // The filter may run zero or many times. Its first result
                    // does not describe later calls after it changes state.
                    let effects = self.unsupported("loop filter with side effects", filter.span);
                    self.record_used(&state, &effects);
                    filter_effects = filter_effects.union(&effects);
                    state = widen(&before_filter, state, &effects);
                    Truth::Unknown(effects)
                };
                match truth {
                    Truth::Unknown(filter_causes) => state.path = state.path.union(&filter_causes),
                    Truth::Known(true) => {}
                    Truth::Known(false) => {
                        // No item passes: only the `else` path remains.
                        self.scan_skipped(body);
                        if round == 0 {
                            return self.exec_block(entry, else_body);
                        }
                        stable = true;
                        break;
                    }
                }
            }
            let mut flow = self.exec_block(state, body)?;
            pop_scope(&mut flow);
            let after = merge_opt(flow.normal, flow.cont, causes);
            exits.brk = merge_opt(exits.brk, flow.brk, causes);
            exits.ret = merge_ret(exits.ret, flow.ret, causes);
            let next = match after {
                Some(after) => merge_states(acc.clone(), after, causes),
                None => acc.clone(),
            };
            if next.same(&acc) {
                stable = true;
                break;
            }
            acc = if round + 1 == LOOP_ROUNDS {
                widen(&entry, next, causes)
            } else {
                next
            };
        }
        if !stable {
            let reason = Causes::one(DependencyReason::AnalysisLimitReached {
                limit: AnalysisLimit::WorkBudget,
                at: self.site(span),
            });
            self.record_used(&acc, &reason);
        }
        // No item may pass even when the filter ran and changed state.
        // Do not restore the original contents in the `else` path.
        let mut skipped = if filter_effects.is_empty() {
            entry
        } else {
            widen(&entry, acc.clone(), &filter_effects)
        };
        skipped.path = skipped.path.union(causes);
        let else_flow = self.exec_block(skipped, else_body)?;
        let normal = merge_opt(Some(acc), else_flow.normal, causes);
        exits.normal = merge_opt(normal, exits.brk.take(), causes);
        exits.ret = merge_ret(exits.ret, else_flow.ret, causes);
        Ok(exits)
    }

    pub(super) fn assign(&mut self, st: &mut State, target: &Target, value: AValue) -> Ev<()> {
        match target {
            Target::Name(name) => st.assign(name.clone(), value),
            Target::Tuple(targets) => {
                let items = match self.items(st, &value, Span::default_for_template()) {
                    Ok(items) if items.len() == targets.len() => items,
                    Ok(_) => {
                        let causes = self.unsupported("unpacking", Span::default_for_template());
                        vec![AValue::Unknown(causes); targets.len()]
                    }
                    Err(causes) => vec![AValue::Unknown(causes); targets.len()],
                };
                for (target, item) in targets.iter().zip(items) {
                    self.assign(st, target, item)?;
                }
            }
            Target::Attr { expr, name } => {
                let object = self.eval(st, expr)?;
                if let AValue::Heap(id) = object
                    && let Some(HeapObj::Namespace(attrs)) = st.heap.get_mut(&id)
                {
                    attrs.insert(name.clone(), value);
                }
            }
            Target::Unsupported(_) => {}
        }
        Ok(())
    }

    pub(super) fn emit(&mut self, st: &mut State, value: &AValue, span: Span) {
        if matches!(st.out, Output::Discard) {
            return;
        }
        match self.freeze(st, value, span) {
            Ok(value) => st.out.push_str(&known::render(&value)),
            Err(causes) => st.out.push_unknown(&causes),
        }
    }

    /// Record the literal dependency calls of a branch that is not taken.
    pub(super) fn scan_skipped(&mut self, stmts: &[Stmt]) {
        let mut found = StaticCalls::default();
        found.scan_stmts(stmts);
        for call in found.refs {
            self.rec.decisions.push(Decision {
                function: DependencyFunction::Ref,
                literals: vec![call.name.clone()],
                reference: Some(call),
                source: None,
                line: 0,
                outcome: DecisionOutcome::SkippedKnownFalse,
            });
        }
        for call in found.sources {
            self.rec.decisions.push(Decision {
                function: DependencyFunction::Source,
                literals: vec![call.source_name.clone(), call.table_name.clone()],
                reference: None,
                source: Some(call),
                line: 0,
                outcome: DecisionOutcome::SkippedKnownFalse,
            });
        }
    }
}
