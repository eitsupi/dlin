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
    AValue, Builtin, Causes, Closure, ContextObject, HeapId, HeapObj, MacroRef, Output, Reason,
    Scope, State, merge_states, merge_value, path_difference,
};
use crate::graph::dependency::{
    AnalysisLimit, DependencyFunction, DependencyReason, ExtractionSite, RuntimeValueSource,
};
use crate::parser::sql::{RefCall, SourceCall, SqlConfig, normalize_version_str};

mod builtins;
mod calls;
mod expressions;
mod methods;
mod scan;
mod statements;

pub(crate) use scan::{StaticCalls, collect_set_literals};

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
    /// Whether `materialized` was set to a value that is not known.
    pub(crate) materialized_unknown: bool,
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
    /// The template body, for explaining rejected computed names.
    pub(crate) template: Option<&'a [Stmt]>,
    template_literals: Option<HashMap<Arc<str>, Vec<String>>>,
    /// The template's scopes when the current call chain was entered.
    root_scopes: Vec<Scope>,
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
            template: None,
            template_literals: None,
            root_scopes: Vec::new(),
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
}

/// Whether an unknown value may be a macro or another callable, rather
/// than data.
fn may_be_callable(causes: &Causes) -> bool {
    causes.iter().any(|reason| {
        !matches!(
            reason,
            DependencyReason::RuntimeValueUnknown { .. }
                | DependencyReason::UnresolvedVar { .. }
                | DependencyReason::UnresolvedRef { .. }
                | DependencyReason::UnresolvedSource { .. }
                | DependencyReason::InvalidDependencyCall { .. }
        )
    })
}

/// The variables a macro defined here can read: only the names it uses, so
/// that defining many macros does not copy the scope each time.
fn capture(st: &State, names: &[Arc<str>]) -> Arc<Scope> {
    Arc::new(
        names
            .iter()
            .filter_map(|name| st.lookup(name).map(|value| (name.clone(), value.clone())))
            .collect(),
    )
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
