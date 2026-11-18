//! The abstract interpreter.
//!
//! It follows a template like a render would, but only what can select a
//! dependency is modelled precisely: `ref`/`source`/`var`, conditions,
//! loops, macro calls and their return values. Everything dbt provides at
//! run time is an unknown value with a reason, and the reason is reported
//! only when such a value selects a dependency. It is not a reimplementation
//! of dbt.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use minijinja::value::{Value, ValueKind};

use super::ir::{Arg, BinOp, Call, Expr, ExprKind, SourceId, Span, Stmt, Target, UnaryOp};
use super::known;
use super::library::MacroLibrary;
use super::value::{
    AValue, Builtin, Causes, ContextObject, HeapId, HeapObj, MacroRef, Output, Reason, Scope,
    State, merge_states, merge_value, path_difference,
};
use crate::graph::dependency::{
    AnalysisLimit, DependencyFunction, DependencyReason, ExtractionSite, RuntimeValueSource,
};
use crate::parser::sql::{RefCall, SourceCall, SqlConfig, normalize_version_str};

const MAX_DEPTH: usize = 32;
pub(crate) const MAX_STEPS: u64 = 1_000_000;
const MAX_ITEMS: usize = 10_000;
const LOOP_ROUNDS: usize = 3;

/// Non-local exits of an evaluation.
#[derive(Debug)]
pub(crate) enum Signal {
    Return(AValue),
    /// `raise_compiler_error`: dbt stops rendering on this path.
    Abort,
    /// The work budget is exhausted.
    Budget,
}

type Ev<T> = Result<T, Signal>;

/// What happened to a ref or source call, for comparing extractors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DecisionOutcome {
    /// Recorded as a dependency. `conditional` when under an unknown
    /// condition.
    Accepted { conditional: bool },
    /// Not recorded because an argument is unknown or invalid.
    Rejected,
    /// In a branch whose condition is known to be false.
    SkippedKnownFalse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Decision {
    pub(crate) function: DependencyFunction,
    pub(crate) reference: Option<RefCall>,
    pub(crate) source: Option<SourceCall>,
    /// String literals in the arguments, for matching a rejected call.
    pub(crate) literals: Vec<String>,
    pub(crate) line: u32,
    pub(crate) outcome: DecisionOutcome,
}

/// What an extraction recorded.
#[derive(Debug, Default)]
pub(crate) struct Recorder {
    pub(crate) refs: Vec<RefCall>,
    pub(crate) sources: Vec<SourceCall>,
    pub(crate) config: SqlConfig,
    pub(crate) reasons: BTreeSet<Reason>,
    pub(crate) decisions: Vec<Decision>,
    /// Names of the macros that were called.
    pub(crate) entered: BTreeSet<Arc<str>>,
}

impl Recorder {
    fn add_ref(&mut self, call: RefCall) {
        if !self.refs.contains(&call) {
            self.refs.push(call);
        }
    }

    fn add_source(&mut self, call: SourceCall) {
        if !self.sources.contains(&call) {
            self.sources.push(call);
        }
    }

    fn add_causes(&mut self, causes: &Causes) {
        self.reasons.extend(causes.iter().cloned());
    }
}

/// The exits of a block of statements.
#[derive(Debug, Default)]
struct Flow {
    normal: Option<State>,
    ret: Option<(State, AValue)>,
    brk: Option<State>,
    cont: Option<State>,
}

impl Flow {
    fn normal(state: State) -> Self {
        Flow {
            normal: Some(state),
            ..Flow::default()
        }
    }
}

fn merge_opt(a: Option<State>, b: Option<State>, fallback: &Causes) -> Option<State> {
    match (a, b) {
        (Some(x), Some(y)) => Some(merge_states(x, y, fallback)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn merge_ret(
    a: Option<(State, AValue)>,
    b: Option<(State, AValue)>,
    fallback: &Causes,
) -> Option<(State, AValue)> {
    match (a, b) {
        (Some((sa, va)), Some((sb, vb))) => {
            let causes = path_difference(&sa.path, &sb.path, fallback);
            let value = merge_value(&va, &vb, &causes);
            Some((merge_states(sa, sb, fallback), value))
        }
        (x, None) => x,
        (None, y) => y,
    }
}

fn merge_flows(a: Flow, b: Flow, fallback: &Causes) -> Flow {
    Flow {
        normal: merge_opt(a.normal, b.normal, fallback),
        ret: merge_ret(a.ret, b.ret, fallback),
        brk: merge_opt(a.brk, b.brk, fallback),
        cont: merge_opt(a.cont, b.cont, fallback),
    }
}

/// The truthiness of a value, or why it is unknown.
enum Truth {
    Known(bool),
    Unknown(Causes),
}

pub(crate) struct Interp<'a> {
    library: &'a MacroLibrary,
    source: &'a str,
    vars: &'a HashMap<String, serde_json::Value>,
    pub(crate) incremental: bool,
    pub(crate) steps: u64,
    depth: usize,
    /// The call in the template that entered the current macro chain.
    root_call: Option<Span>,
    /// The last expression evaluated, for reporting where the budget ran out.
    pub(crate) current: Option<Span>,
    next_heap: HeapId,
    pub(crate) rec: Recorder,
    /// Whether `config()` may still set values (only on the first pass).
    pub(crate) record_config: bool,
}

impl<'a> Interp<'a> {
    pub(crate) fn new(
        library: &'a MacroLibrary,
        source: &'a str,
        vars: &'a HashMap<String, serde_json::Value>,
    ) -> Self {
        Interp {
            library,
            source,
            vars,
            incremental: false,
            steps: 0,
            depth: 0,
            root_call: None,
            current: None,
            next_heap: 0,
            rec: Recorder::default(),
            record_config: true,
        }
    }

    // ----- sites and reasons -------------------------------------------

    /// The location to report for something at `span`: the span itself in
    /// the template body, or the call that entered the macro chain.
    pub(crate) fn site(&self, span: Span) -> ExtractionSite {
        let span = if self.depth == 0 {
            span
        } else {
            self.root_call.unwrap_or(span)
        };
        if span.source != SourceId::Template {
            return ExtractionSite::default();
        }
        let expr = self
            .source
            .get(span.start as usize..span.end as usize)
            .map(str::to_string);
        ExtractionSite {
            line: Some(span.line),
            expr,
        }
    }

    fn unsupported(&self, construct: impl Into<String>, span: Span) -> Causes {
        Causes::one(DependencyReason::UnsupportedJinjaConstruct {
            construct: construct.into(),
            at: self.site(span),
        })
    }

    fn runtime(&self, source: RuntimeValueSource, name: Option<String>, span: Span) -> Causes {
        Causes::one(DependencyReason::RuntimeValueUnknown {
            source,
            name,
            at: self.site(span),
        })
    }

    fn context_causes(&self, object: &ContextObject, span: Span) -> Causes {
        let source = match object.root {
            "target" => RuntimeValueSource::TargetAttribute,
            "adapter" => RuntimeValueSource::AdapterCall,
            _ => RuntimeValueSource::ContextObject,
        };
        self.runtime(source, Some(object.path.to_string()), span)
    }

    fn relation_causes(&self, name: Option<&str>, span: Span) -> Causes {
        self.runtime(
            RuntimeValueSource::RelationAttribute,
            name.map(str::to_string),
            span,
        )
    }

    /// Record reasons that select a dependency, together with the
    /// conditions of the current path.
    fn record_used(&mut self, st: &State, causes: &Causes) {
        self.rec.add_causes(causes);
        self.rec.add_causes(&st.path);
    }

    fn tick(&mut self, span: Option<Span>) -> Ev<()> {
        self.steps += 1;
        if span.is_some() {
            self.current = span;
        }
        if self.steps > MAX_STEPS {
            return Err(Signal::Budget);
        }
        Ok(())
    }

    fn alloc(&mut self, st: &mut State, obj: HeapObj) -> AValue {
        let id = self.next_heap;
        self.next_heap += 1;
        st.heap.insert(id, obj);
        AValue::Heap(id)
    }

    // ----- statements -----------------------------------------------------

    pub(crate) fn run_template(&mut self, body: &[Stmt]) -> Ev<()> {
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

    fn exec_block(&mut self, state: State, body: &[Stmt]) -> Ev<Flow> {
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

    fn exec_stmt(&mut self, mut st: State, stmt: &Stmt) -> Ev<Flow> {
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
    fn exec_stmt_inner(&mut self, st: &mut State, stmt: &Stmt) -> Ev<Option<Flow>> {
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
                    closure: Some(capture(st, &def.names)),
                });
                st.assign(def.name.clone(), value);
            }
            Stmt::CallBlock { call, caller } => {
                let caller = AValue::Macro(MacroRef {
                    def: caller.clone(),
                    closure: Some(capture(st, &caller.names)),
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
                for call in found.refs {
                    self.rec.add_ref(call);
                }
                for call in found.sources {
                    self.rec.add_source(call);
                }
            }
        }
        Ok(None)
    }

    fn branch(&mut self, state: State, causes: &Causes, a: &[Stmt], b: &[Stmt]) -> Ev<Flow> {
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
    fn capture(&mut self, st: &State, body: &[Stmt]) -> Ev<(Option<(State, AValue)>, Flow)> {
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
    fn exec_for(
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

    fn loop_object(index: usize, length: usize) -> AValue {
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

    fn exec_known_loop(
        &mut self,
        st: State,
        target: &Target,
        items: Vec<AValue>,
        filter: Option<&Expr>,
        body: &[Stmt],
        else_body: &[Stmt],
    ) -> Ev<Flow> {
        if items.is_empty() {
            return self.exec_block(st, else_body);
        }
        let fallback = Causes::default();
        let length = items.len();
        let mut current = Some(st);
        let mut exits = Flow::default();
        for (index, item) in items.into_iter().enumerate() {
            let Some(mut state) = current.take() else {
                break;
            };
            state.scopes.push(Scope::new());
            self.assign(&mut state, target, item)?;
            state.assign(Arc::from("loop"), Self::loop_object(index, length));
            let mut flow = match filter {
                None => self.exec_block(state, body)?,
                Some(filter) => {
                    let value = self.eval(&mut state, filter)?;
                    match self.truth(&state, &value, filter.span) {
                        Truth::Known(true) => self.exec_block(state, body)?,
                        Truth::Known(false) => Flow::normal(state),
                        Truth::Unknown(causes) => self.branch(state, &causes, body, &[])?,
                    }
                }
            };
            pop_scope(&mut flow);
            current = merge_opt(flow.normal, flow.cont, &fallback);
            exits.brk = merge_opt(exits.brk, flow.brk, &fallback);
            exits.ret = merge_ret(exits.ret, flow.ret, &fallback);
        }
        exits.normal = merge_opt(current, exits.brk.take(), &fallback);
        Ok(exits)
    }

    /// A loop whose iterations are unknown: iterate to a fixed point of the
    /// state, widening what keeps changing.
    #[allow(clippy::too_many_arguments)]
    fn exec_unknown_loop(
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
        for round in 0..=LOOP_ROUNDS {
            let mut state = acc.clone();
            state.path = state.path.union(causes);
            state.scopes.push(Scope::new());
            self.assign(&mut state, target, AValue::Unknown(causes.clone()))?;
            state.assign(Arc::from("loop"), AValue::Unknown(causes.clone()));
            if let Some(filter) = filter {
                // Whether an item passes the filter is as unknown as the
                // items; evaluate it for its own dependencies only.
                self.eval(&mut state, filter)?;
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
        // The loop may run zero times, which is when `else` runs.
        let mut skipped = entry;
        skipped.path = skipped.path.union(causes);
        let else_flow = self.exec_block(skipped, else_body)?;
        let normal = merge_opt(Some(acc), else_flow.normal, causes);
        exits.normal = merge_opt(normal, exits.brk.take(), causes);
        exits.ret = merge_ret(exits.ret, else_flow.ret, causes);
        Ok(exits)
    }

    fn assign(&mut self, st: &mut State, target: &Target, value: AValue) -> Ev<()> {
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

    fn emit(&mut self, st: &mut State, value: &AValue, span: Span) {
        if matches!(st.out, Output::Discard) {
            return;
        }
        match self.freeze(st, value, span) {
            Ok(value) => st.out.push_str(&known::render(&value)),
            Err(causes) => st.out.push_unknown(&causes),
        }
    }

    /// Record the literal dependency calls of a branch that is not taken.
    fn scan_skipped(&mut self, stmts: &[Stmt]) {
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

    // ----- values -------------------------------------------------------

    fn truth(&self, st: &State, value: &AValue, span: Span) -> Truth {
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
            AValue::Macro(_) | AValue::Builtin(_) | AValue::Relation => Truth::Known(true),
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
    fn freeze(&self, st: &State, value: &AValue, span: Span) -> Result<Value, Causes> {
        match value {
            AValue::Known(value) => Ok(value.clone()),
            AValue::Unknown(causes) => Err(causes.clone()),
            AValue::Heap(id) => match st.heap.get(id) {
                Some(HeapObj::List(items)) => items
                    .iter()
                    .map(|item| self.freeze(st, item, span))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::from),
                Some(HeapObj::Map(entries)) => entries
                    .iter()
                    .map(|(key, item)| self.freeze(st, item, span).map(|v| (key.clone(), v)))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::from_pairs),
                Some(HeapObj::Namespace(attrs)) => attrs
                    .iter()
                    .map(|(key, item)| {
                        self.freeze(st, item, span)
                            .map(|v| (Value::from(key.as_ref()), v))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::from_pairs),
                Some(HeapObj::Clobbered(causes)) => Err(causes.clone()),
                None => Ok(Value::UNDEFINED),
            },
            AValue::Relation => Err(self.relation_causes(None, span)),
            AValue::Context(object) => Err(self.context_causes(object, span)),
            AValue::Macro(def) => {
                Err(self.unsupported(format!("macro value {}", def.def.name), span))
            }
            AValue::Builtin(_) => Err(self.unsupported("function value", span)),
        }
    }

    /// The items of an iterable, or why they are unknown.
    fn items(&self, st: &State, value: &AValue, span: Span) -> Result<Vec<AValue>, Causes> {
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
        Ok(match &expr.kind {
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
                match self.freeze_all(st, &parts, span) {
                    Ok(values) => known::slice(&values[0], &values[1], &values[2], &values[3])
                        .map(AValue::Known)
                        .unwrap_or_else(|_| AValue::Unknown(self.unsupported("slice", span))),
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
                            let saved = st.path.clone();
                            st.path = st.path.union(&causes);
                            let right_value = self.eval(st, right);
                            st.path = saved;
                            merge_value(&left_value, &right_value?, &causes)
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
                        let saved = st.path.clone();
                        st.path = st.path.union(&causes);
                        let a = self.eval(st, then);
                        let b = match otherwise {
                            Some(e) => self.eval(st, e),
                            None => Ok(AValue::undefined()),
                        };
                        st.path = saved;
                        merge_value(&a?, &b?, &causes)
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
                let object = self.eval(st, expr)?;
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
                        Ok(key) => entries.push((key, item)),
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
        })
    }

    fn freeze_all(&self, st: &State, values: &[AValue], span: Span) -> Result<Vec<Value>, Causes> {
        let mut out = Vec::with_capacity(values.len());
        let mut unknown: Option<Causes> = None;
        for value in values {
            match self.freeze(st, value, span) {
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

    fn binary(&self, st: &State, op: BinOp, left: &AValue, right: &AValue, span: Span) -> AValue {
        match self.freeze_all(st, &[left.clone(), right.clone()], span) {
            Ok(values) => match known::binary(op, &values[0], &values[1]) {
                Ok(value) => AValue::Known(value),
                Err(_) => {
                    AValue::Unknown(self.unsupported(format!("operator {}", op.symbol()), span))
                }
            },
            Err(causes) => AValue::Unknown(causes),
        }
    }

    fn lookup(&self, st: &State, name: &str, span: Span) -> AValue {
        if let Some(value) = st.lookup(name) {
            return value.clone();
        }
        self.global(name, span).unwrap_or_else(AValue::undefined)
    }

    /// A name that is not bound in the template.
    fn global(&self, name: &str, span: Span) -> Option<AValue> {
        if let Some(def) = self.library.get(name) {
            return Some(AValue::Macro(MacroRef {
                def: def.clone(),
                closure: None,
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

    fn get_attr(&self, st: &State, object: &AValue, name: &str, span: Span) -> AValue {
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
            AValue::Macro(_) | AValue::Builtin(_) => AValue::undefined(),
        }
    }

    fn get_item(&self, st: &State, object: &AValue, key: &AValue, span: Span) -> AValue {
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

    fn test(&self, st: &State, name: &str, values: &[AValue], span: Span) -> AValue {
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

    fn apply_filter_expr(&mut self, st: &mut State, filter: &Expr, input: AValue) -> Ev<AValue> {
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

    fn apply_filter(
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
                    self.eval(st, e)?;
                    return Ok(AValue::Unknown(self.unsupported("splat argument", span)));
                }
            }
        }
        if name == "default" || name == "d" {
            // Keep a known default for an undefined value even when the
            // value has a non-known form.
            if let AValue::Known(value) = &values[0]
                && !value.is_undefined()
            {
                return Ok(values[0].clone());
            }
            if let AValue::Known(_) = &values[0] {
                return Ok(values
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| AValue::Known(Value::from(""))));
            }
        }
        let mut frozen = match self.freeze_all(st, &values, span) {
            Ok(frozen) => frozen,
            Err(causes) => return Ok(AValue::Unknown(causes)),
        };
        if !kwargs.is_empty() {
            let mut pairs = Vec::new();
            for (key, value) in &kwargs {
                match self.freeze(st, value, span) {
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
            Some(Ok(value)) => AValue::Known(value),
            Some(Err(_)) | None => {
                AValue::Unknown(self.unsupported(format!("filter:{name}"), span))
            }
        })
    }

    // ----- calls -------------------------------------------------------

    /// The source text of an expression, for naming a callee.
    fn text(&self, expr: &Expr) -> Option<String> {
        (expr.span.source == SourceId::Template)
            .then(|| {
                self.source
                    .get(expr.span.start as usize..expr.span.end as usize)
            })
            .flatten()
            .map(str::to_string)
    }

    fn is_bound(&self, st: &State, name: &str) -> bool {
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
    fn opaque_call(&mut self, st: &mut State, call: &Call, causes: &Causes) -> Ev<AValue> {
        let (args, kwargs) = self.eval_args(st, &call.args, call.span)?;
        self.record_used(st, causes);
        st.clobber_reachable(args.iter().chain(kwargs.iter().map(|(_, v)| v)), causes);
        Ok(AValue::Unknown(causes.clone()))
    }

    #[allow(clippy::type_complexity)]
    fn eval_args(
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
                            _ => keyword.push((
                                Arc::from("**"),
                                AValue::Unknown(self.unsupported("**kwargs", span)),
                            )),
                        },
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

    fn call_value(
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
            AValue::Context(object) => Ok(AValue::Unknown(self.context_causes(&object, span))),
            AValue::Known(_) | AValue::Heap(_) | AValue::Relation => Ok(AValue::Unknown(
                self.unsupported("calling a non-callable value", span),
            )),
        }
    }

    fn invoke(
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
        let entering = self.depth == 0;
        if entering {
            self.root_call = Some(span);
        }
        self.depth += 1;
        let result = self.invoke_inner(st, &target, args, kwargs, caller);
        self.depth -= 1;
        if entering {
            self.root_call = None;
        }
        result
    }

    fn invoke_inner(
        &mut self,
        st: &mut State,
        target: &MacroRef,
        args: Vec<AValue>,
        mut kwargs: Vec<(Arc<str>, AValue)>,
        caller: Option<AValue>,
    ) -> Ev<AValue> {
        let def = &target.def;
        self.rec.entered.insert(def.name.clone());
        let mut scopes: Vec<Scope> = match &target.closure {
            Some(closure) => closure.as_ref().clone(),
            None => Vec::new(),
        };
        let mut params = Scope::new();
        if target.closure.is_some() {
            params.insert(def.name.clone(), AValue::Macro(target.clone()));
        }
        scopes.push(params);
        let heap = std::mem::take(&mut st.heap);
        let mut frame = State::new(scopes, heap, Output::Known(String::new()), st.path.clone());
        let mut positional = args.into_iter();
        let mut bind_error = None;
        for (index, param) in def.params.iter().enumerate() {
            let value = if let Some(value) = positional.next() {
                value
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

    fn string_arg(&self, st: &State, value: &AValue, span: Span) -> Result<Option<String>, Causes> {
        match self.freeze(st, value, span)? {
            value if value.kind() == ValueKind::String => Ok(value.as_str().map(str::to_string)),
            _ => Ok(None),
        }
    }

    fn reject(&mut self, st: &State, function: DependencyFunction, causes: Causes, call: &Call) {
        self.record_used(st, &causes);
        let mut found = StaticCalls::default();
        for arg in &call.args {
            found.literals(arg.expr());
        }
        self.rec.decisions.push(Decision {
            function,
            reference: None,
            source: None,
            literals: found.strings,
            line: call.span.line,
            outcome: DecisionOutcome::Rejected,
        });
    }

    fn invalid(&self, function: DependencyFunction, span: Span) -> Causes {
        Causes::one(DependencyReason::InvalidDependencyCall {
            function,
            at: self.site(span),
        })
    }

    fn builtin(
        &mut self,
        st: &mut State,
        builtin: Builtin,
        args: Vec<AValue>,
        kwargs: Vec<(Arc<str>, AValue)>,
        call: &Call,
    ) -> Ev<AValue> {
        let span = call.span;
        let kwarg = |name: &str| {
            kwargs
                .iter()
                .find(|(k, _)| k.as_ref() == name)
                .map(|(_, v)| v)
        };
        match builtin {
            Builtin::Ref => {
                let function = DependencyFunction::Ref;
                if args.is_empty() || args.len() > 2 {
                    let causes = self.invalid(function, span);
                    self.reject(st, function, causes, call);
                    return Ok(AValue::Relation);
                }
                let mut parts = Vec::new();
                let mut unknown: Option<Causes> = None;
                let mut invalid = false;
                for value in &args {
                    match self.string_arg(st, value, span) {
                        Ok(Some(text)) => parts.push(text),
                        Ok(None) => invalid = true,
                        Err(causes) => {
                            unknown = Some(unknown.map_or(causes.clone(), |c| c.union(&causes)))
                        }
                    }
                }
                let version = match kwarg("version").or_else(|| kwarg("v")) {
                    None => None,
                    Some(value) => match self.freeze(st, value, span) {
                        Ok(v) if v.kind() == ValueKind::Number => Some(v.to_string()),
                        Ok(v) if v.kind() == ValueKind::String => {
                            v.as_str().map(normalize_version_str)
                        }
                        Ok(v) if v.is_none() => None,
                        Ok(_) => {
                            invalid = true;
                            None
                        }
                        Err(causes) => {
                            unknown = Some(unknown.map_or(causes.clone(), |c| c.union(&causes)));
                            None
                        }
                    },
                };
                if let Some(causes) = unknown {
                    self.reject(st, function, causes, call);
                } else if invalid {
                    let causes = self.invalid(function, span);
                    self.reject(st, function, causes, call);
                } else {
                    let (package, name) = match parts.len() {
                        1 => (None, parts.remove(0)),
                        _ => {
                            let name = parts.remove(1);
                            (Some(parts.remove(0)), name)
                        }
                    };
                    let reference = RefCall {
                        package,
                        name,
                        version,
                    };
                    self.accept(st, Some(reference), None, span);
                }
                Ok(AValue::Relation)
            }
            Builtin::Source => {
                let function = DependencyFunction::Source;
                if args.len() != 2 {
                    let causes = self.invalid(function, span);
                    self.reject(st, function, causes, call);
                    return Ok(AValue::Relation);
                }
                match (
                    self.string_arg(st, &args[0], span),
                    self.string_arg(st, &args[1], span),
                ) {
                    (Ok(Some(source_name)), Ok(Some(table_name))) => {
                        self.accept(
                            st,
                            None,
                            Some(SourceCall {
                                source_name,
                                table_name,
                            }),
                            span,
                        );
                    }
                    (Err(a), Err(b)) => self.reject(st, function, a.union(&b), call),
                    (Err(causes), _) | (_, Err(causes)) => self.reject(st, function, causes, call),
                    _ => {
                        let causes = self.invalid(function, span);
                        self.reject(st, function, causes, call);
                    }
                }
                Ok(AValue::Relation)
            }
            Builtin::Var => {
                let Some(name) = args.first() else {
                    return Ok(AValue::undefined());
                };
                let name = match self.string_arg(st, name, span) {
                    Ok(Some(name)) => name,
                    Ok(None) => return Ok(AValue::undefined()),
                    Err(causes) => return Ok(AValue::Unknown(causes)),
                };
                if let Some(value) = self.vars.get(&name) {
                    return Ok(AValue::Known(Value::from(minijinja::value::Serde(value))));
                }
                if let Some(default) = args.get(1).or_else(|| kwarg("default")) {
                    return Ok(default.clone());
                }
                Ok(AValue::Unknown(Causes::one(
                    DependencyReason::UnresolvedVar {
                        name: Some(name),
                        at: self.site(span),
                    },
                )))
            }
            Builtin::EnvVar => {
                let name = args
                    .first()
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                Ok(AValue::Unknown(self.runtime(
                    RuntimeValueSource::EnvVar,
                    name,
                    span,
                )))
            }
            Builtin::Config => {
                if self.record_config {
                    if let Some(value) = kwarg("materialized")
                        && let Ok(Some(materialized)) = self.string_arg(st, value, span)
                        && self.rec.config.materialized.is_none()
                    {
                        self.rec.config.materialized = Some(materialized);
                    }
                    if let Some(value) = kwarg("tags")
                        && self.rec.config.tags.is_empty()
                        && let Ok(tags) = self.freeze(st, value, span)
                    {
                        self.rec.config.tags = match tags.as_str() {
                            Some(tag) => vec![tag.to_string()],
                            None => tags
                                .try_iter()
                                .map(|iter| iter.map(|tag| tag.to_string()).collect())
                                .unwrap_or_default(),
                        };
                    }
                }
                Ok(AValue::Known(Value::from("")))
            }
            Builtin::ConfigGet => {
                let name = args
                    .first()
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                Ok(AValue::Unknown(self.runtime(
                    RuntimeValueSource::ConfigValue,
                    name,
                    span,
                )))
            }
            Builtin::IsIncremental => Ok(AValue::Known(Value::from(self.incremental))),
            Builtin::Return => Err(Signal::Return(
                args.into_iter().next().unwrap_or_else(AValue::none),
            )),
            Builtin::Noop | Builtin::Statement => Ok(AValue::Known(Value::from(""))),
            Builtin::RunQuery => Ok(AValue::Unknown(self.runtime(
                RuntimeValueSource::RunQuery,
                None,
                span,
            ))),
            Builtin::LoadResult => Ok(AValue::Unknown(self.runtime(
                RuntimeValueSource::Statement,
                None,
                span,
            ))),
            Builtin::Namespace => {
                let mut attrs = BTreeMap::new();
                if let Some(AValue::Heap(id)) = args.first()
                    && let Some(HeapObj::Map(entries)) = st.heap.get(id)
                {
                    for (key, value) in entries {
                        if let Some(key) = key.as_str() {
                            attrs.insert(Arc::from(key), value.clone());
                        }
                    }
                }
                for (key, value) in kwargs {
                    attrs.insert(key, value);
                }
                Ok(self.alloc(st, HeapObj::Namespace(attrs)))
            }
            Builtin::Minijinja(name) => {
                let mut values = args;
                values.extend(kwargs.into_iter().map(|(_, v)| v));
                match self.freeze_all(st, &values, span) {
                    Ok(values) => Ok(known::call_global(name, &values)
                        .map(AValue::Known)
                        .unwrap_or_else(|_| {
                            AValue::Unknown(self.unsupported(format!("function:{name}"), span))
                        })),
                    Err(causes) => Ok(AValue::Unknown(causes)),
                }
            }
            Builtin::Unmodelled(name) => Ok(AValue::Unknown(
                self.unsupported(format!("function:{name}"), span),
            )),
            Builtin::Dispatch => {
                let name = args
                    .first()
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                let namespace = kwarg("macro_namespace")
                    .or_else(|| args.get(1))
                    .and_then(|value| self.string_arg(st, value, span).ok().flatten());
                let causes = Causes::one(DependencyReason::UnsupportedDispatch {
                    name,
                    namespace,
                    at: self.site(span),
                });
                self.record_used(st, &causes);
                Ok(AValue::Unknown(causes))
            }
            Builtin::RaiseCompilerError => Err(Signal::Abort),
        }
    }

    fn accept(
        &mut self,
        st: &State,
        reference: Option<RefCall>,
        source: Option<SourceCall>,
        span: Span,
    ) {
        let conditional = !st.path.is_empty();
        self.rec.add_causes(&st.path);
        let (function, literals) = match (&reference, &source) {
            (Some(r), _) => (DependencyFunction::Ref, vec![r.name.clone()]),
            (_, Some(s)) => (
                DependencyFunction::Source,
                vec![s.source_name.clone(), s.table_name.clone()],
            ),
            _ => return,
        };
        if let Some(r) = &reference {
            self.rec.add_ref(r.clone());
        }
        if let Some(s) = &source {
            self.rec.add_source(s.clone());
        }
        self.rec.decisions.push(Decision {
            function,
            reference,
            source,
            literals,
            line: span.line,
            outcome: DecisionOutcome::Accepted { conditional },
        });
    }

    fn method(
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
                if let Some(result) = self.known_method(st, value, name, &args, span) {
                    return result;
                }
            }
            _ => {}
        }
        AValue::Unknown(self.unsupported(format!("method:{name}"), span))
    }

    fn heap_method(
        &mut self,
        st: &mut State,
        id: HeapId,
        name: &str,
        args: &[AValue],
        kwargs: &[(Arc<str>, AValue)],
        span: Span,
    ) -> Option<AValue> {
        let _ = kwargs;
        let obj = st.heap.get(&id)?.clone();
        match (obj, name) {
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

    fn known_method(
        &self,
        st: &State,
        value: &Value,
        name: &str,
        args: &[AValue],
        span: Span,
    ) -> Option<AValue> {
        let args = match self.freeze_all(st, args, span) {
            Ok(args) => args,
            Err(causes) => return Some(AValue::Unknown(causes)),
        };
        let string_arg = |i: usize| args.get(i).and_then(|v| v.as_str()).map(str::to_string);
        if let Some(text) = value.as_str() {
            let result = match name {
                "lower" => Value::from(text.to_lowercase()),
                "upper" => Value::from(text.to_uppercase()),
                "strip" => Value::from(text.trim()),
                "lstrip" => Value::from(text.trim_start()),
                "rstrip" => Value::from(text.trim_end()),
                "replace" => Value::from(text.replace(&string_arg(0)?, &string_arg(1)?)),
                "startswith" => Value::from(text.starts_with(&string_arg(0)?)),
                "endswith" => Value::from(text.ends_with(&string_arg(0)?)),
                "split" => match string_arg(0) {
                    Some(sep) => Value::from(
                        text.split(sep.as_str())
                            .map(Value::from)
                            .collect::<Vec<_>>(),
                    ),
                    None => {
                        Value::from(text.split_whitespace().map(Value::from).collect::<Vec<_>>())
                    }
                },
                "join" => {
                    let items: Vec<String> = args
                        .first()?
                        .try_iter()
                        .ok()?
                        .map(|v| v.to_string())
                        .collect();
                    Value::from(items.join(text))
                }
                _ => return None,
            };
            return Some(AValue::Known(result));
        }
        if value.kind() == ValueKind::Map {
            return match name {
                "get" => Some(AValue::Known(
                    value
                        .get_item(args.first()?)
                        .ok()
                        .filter(|v| !v.is_undefined())
                        .unwrap_or_else(|| args.get(1).cloned().unwrap_or_else(|| Value::from(()))),
                )),
                "keys" => Some(AValue::Known(Value::from(
                    value.try_iter().ok()?.collect::<Vec<_>>(),
                ))),
                "values" => Some(AValue::Known(Value::from(
                    value
                        .try_iter()
                        .ok()?
                        .filter_map(|k| value.get_item(&k).ok())
                        .collect::<Vec<_>>(),
                ))),
                "items" => Some(AValue::Known(Value::from(
                    value
                        .try_iter()
                        .ok()?
                        .filter_map(|k| value.get_item(&k).ok().map(|v| Value::from(vec![k, v])))
                        .collect::<Vec<_>>(),
                ))),
                _ => None,
            };
        }
        None
    }
}

/// The variables a macro defined here can read: only the names it uses, so
/// that defining many macros does not copy the scope each time.
fn capture(st: &State, names: &[Arc<str>]) -> Arc<Vec<Scope>> {
    let scope: Scope = names
        .iter()
        .filter_map(|name| st.lookup(name).map(|value| (name.clone(), value.clone())))
        .collect();
    Arc::new(vec![scope])
}

fn pop_scope(flow: &mut Flow) {
    for state in [&mut flow.normal, &mut flow.brk, &mut flow.cont]
        .into_iter()
        .flatten()
    {
        state.scopes.pop();
    }
    if let Some((state, _)) = &mut flow.ret {
        state.scopes.pop();
    }
}

/// Make everything that differs from `entry` unknown.
fn widen(entry: &State, state: State, causes: &Causes) -> State {
    let mut state = state;
    for (index, scope) in state.scopes.iter_mut().enumerate() {
        for (name, value) in scope.iter_mut() {
            let unchanged = entry
                .scopes
                .get(index)
                .and_then(|s| s.get(name))
                .is_some_and(|before| before.same(value));
            if !unchanged {
                let merged = match value {
                    AValue::Unknown(c) => c.union(causes),
                    _ => causes.clone(),
                };
                *value = AValue::Unknown(merged);
            }
        }
    }
    let ids: Vec<HeapId> = state.heap.keys().copied().collect();
    for id in ids {
        let changed = match (entry.heap.get(&id), state.heap.get(&id)) {
            (Some(before), Some(after)) => {
                !matches!((before, after), (HeapObj::Clobbered(a), HeapObj::Clobbered(b)) if a == b)
                    && format!("{before:?}") != format!("{after:?}")
            }
            _ => true,
        };
        if changed {
            state.heap.insert(id, HeapObj::Clobbered(causes.clone()));
        }
    }
    if let Output::Known(_) = state.out {
        state.out = Output::Unknown(causes.clone());
    }
    state
}

/// Literal dependency calls found without executing.
#[derive(Default)]
pub(crate) struct StaticCalls {
    pub(crate) refs: Vec<RefCall>,
    pub(crate) sources: Vec<SourceCall>,
    pub(crate) strings: Vec<String>,
    /// Whether a ref/source call with a non-literal argument was found.
    pub(crate) dynamic: bool,
}

impl StaticCalls {
    pub(crate) fn scan_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::Emit(e) | Stmt::Set { expr: e, .. } => self.scan_expr(e),
                Stmt::Raw(_) | Stmt::Break | Stmt::Continue | Stmt::Macro(_) => {}
                Stmt::For {
                    iter,
                    filter,
                    body,
                    else_body,
                    ..
                } => {
                    self.scan_expr(iter);
                    if let Some(f) = filter {
                        self.scan_expr(f);
                    }
                    self.scan_stmts(body);
                    self.scan_stmts(else_body);
                }
                Stmt::If {
                    cond,
                    then,
                    otherwise,
                } => {
                    self.scan_expr(cond);
                    self.scan_stmts(then);
                    self.scan_stmts(otherwise);
                }
                Stmt::With { assignments, body } => {
                    for (_, e) in assignments {
                        self.scan_expr(e);
                    }
                    self.scan_stmts(body);
                }
                Stmt::SetBlock { body, .. }
                | Stmt::FilterBlock { body, .. }
                | Stmt::AutoEscape { body } => self.scan_stmts(body),
                Stmt::CallBlock { call, caller } => {
                    self.scan_call(call);
                    self.scan_stmts(&caller.body);
                }
                Stmt::Do(call) => self.scan_call(call),
                Stmt::Unsupported { exprs, body, .. } => {
                    for e in exprs {
                        self.scan_expr(e);
                    }
                    self.scan_stmts(body);
                }
            }
        }
    }

    fn scan_call(&mut self, call: &Call) {
        if let ExprKind::Var(name) = &call.callee.kind {
            let literals: Option<Vec<String>> = call
                .args
                .iter()
                .map(|arg| match arg {
                    Arg::Pos(Expr {
                        kind: ExprKind::Const(value),
                        ..
                    }) => value.as_str().map(str::to_string),
                    _ => None,
                })
                .collect();
            match (name.as_ref(), literals) {
                ("ref", Some(mut parts)) if (1..=2).contains(&parts.len()) => {
                    let name = parts.pop().unwrap_or_default();
                    self.refs.push(RefCall {
                        package: parts.pop(),
                        name,
                        version: None,
                    });
                }
                ("source", Some(parts)) if parts.len() == 2 => self.sources.push(SourceCall {
                    source_name: parts[0].clone(),
                    table_name: parts[1].clone(),
                }),
                ("ref" | "source", _) => self.dynamic = true,
                _ => {}
            }
        }
        self.scan_expr(&call.callee);
        for arg in &call.args {
            self.scan_expr(arg.expr());
        }
    }

    pub(crate) fn scan_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Var(_) | ExprKind::Const(_) => {}
            ExprKind::Call(call) => self.scan_call(call),
            ExprKind::Slice {
                expr,
                start,
                stop,
                step,
            } => {
                self.scan_expr(expr);
                for e in [start, stop, step].into_iter().flatten() {
                    self.scan_expr(e);
                }
            }
            ExprKind::Unary { expr, .. } | ExprKind::GetAttr { expr, .. } => self.scan_expr(expr),
            ExprKind::Binary { left, right, .. } => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            ExprKind::Compare { expr, ops } => {
                self.scan_expr(expr);
                for (_, e) in ops {
                    self.scan_expr(e);
                }
            }
            ExprKind::IfExpr {
                test,
                then,
                otherwise,
            } => {
                self.scan_expr(test);
                self.scan_expr(then);
                if let Some(e) = otherwise {
                    self.scan_expr(e);
                }
            }
            ExprKind::Filter { expr, args, .. } => {
                if let Some(e) = expr {
                    self.scan_expr(e);
                }
                for arg in args {
                    self.scan_expr(arg.expr());
                }
            }
            ExprKind::Test { expr, args, .. } => {
                self.scan_expr(expr);
                for arg in args {
                    self.scan_expr(arg.expr());
                }
            }
            ExprKind::GetItem { expr, subscript } => {
                self.scan_expr(expr);
                self.scan_expr(subscript);
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for e in items {
                    self.scan_expr(e);
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.scan_expr(k);
                    self.scan_expr(v);
                }
            }
        }
    }

    /// Collect the string literals of an expression.
    pub(crate) fn literals(&mut self, expr: &Expr) {
        if let ExprKind::Const(value) = &expr.kind
            && let Some(text) = value.as_str()
        {
            self.strings.push(text.to_string());
        }
        match &expr.kind {
            ExprKind::Binary { left, right, .. } => {
                self.literals(left);
                self.literals(right);
            }
            ExprKind::Call(call) => {
                for arg in &call.args {
                    self.literals(arg.expr());
                }
            }
            ExprKind::Filter { expr: Some(e), .. } | ExprKind::Unary { expr: e, .. } => {
                self.literals(e)
            }
            _ => {}
        }
    }
}
