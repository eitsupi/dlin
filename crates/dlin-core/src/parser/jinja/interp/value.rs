//! Abstract values and the interpreter state.
//!
//! A value is either known exactly or unknown with the reasons it is
//! unknown. Containers that templates mutate (`[]`, `{}`, `namespace()`) live
//! in a heap so that aliases see the same updates. Everything else that dbt
//! provides at run time is not modelled: it is unknown, with a reason that is
//! only reported if the value ends up selecting a dependency.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use minijinja::Value;

use super::ir::MacroDef;
use crate::graph::dependency::ExtractedDependencyReason;

pub(crate) type Reason = ExtractedDependencyReason;

/// A set of reasons. Unknown values always carry at least one; the path
/// condition of a state may be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Causes(Arc<BTreeSet<Reason>>);

impl Causes {
    pub(crate) fn one(reason: Reason) -> Self {
        Self(Arc::new(BTreeSet::from([reason])))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Reason> {
        self.0.iter()
    }

    pub(crate) fn union(&self, other: &Causes) -> Causes {
        if other.0.is_subset(&self.0) {
            return self.clone();
        }
        if self.0.is_subset(&other.0) {
            return other.clone();
        }
        Causes(Arc::new(self.0.union(&other.0).cloned().collect()))
    }

    pub(crate) fn intersection(&self, other: &Causes) -> Causes {
        if Arc::ptr_eq(&self.0, &other.0) {
            return self.clone();
        }
        Causes(Arc::new(self.0.intersection(&other.0).cloned().collect()))
    }

    pub(crate) fn difference(&self, other: &Causes) -> Causes {
        Causes(Arc::new(self.0.difference(&other.0).cloned().collect()))
    }
}

pub(crate) type HeapId = u32;

/// A macro that can be called, with the scope it was defined in.
#[derive(Debug, Clone)]
pub(crate) struct MacroRef {
    pub(crate) def: Arc<MacroDef>,
    pub(crate) closure: Closure,
}

/// What a macro's body can see besides its arguments.
#[derive(Debug, Clone)]
pub(crate) enum Closure {
    /// A project macro: only the project macros and the dbt context.
    Project,
    /// Defined at the top level of the template: it reads the template's
    /// variables as they are when it is called.
    TemplateRoot,
    /// Defined inside a macro, loop or block: the variables it reads, as
    /// they were where it was defined.
    Captured(Arc<Scope>),
}

/// A function provided by the interpreter rather than by the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Builtin {
    Ref,
    Source,
    Var,
    EnvVar,
    Config,
    ConfigGet,
    IsIncremental,
    Return,
    /// Evaluates its arguments and returns nothing (`log`, `print`, ...).
    Noop,
    RunQuery,
    LoadResult,
    Statement,
    Namespace,
    /// A pure minijinja global function (`range`, `dict`), run on known
    /// arguments.
    Minijinja(&'static str),
    /// A pure dbt helper whose result is not modelled (`tojson`, ...).
    Unmodelled(&'static str),
    Dispatch,
    RaiseCompilerError,
}

/// A dbt context object, such as `target` or `adapter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextObject {
    pub(crate) root: &'static str,
    /// The path read so far, such as `target.name`.
    pub(crate) path: Arc<str>,
}

#[derive(Debug, Clone)]
pub(crate) enum AValue {
    Known(Value),
    Unknown(Causes),
    Heap(HeapId),
    Macro(MacroRef),
    /// A package receiver for macro lookup. Ownership is checked by the library.
    MacroNamespace(Arc<str>),
    Builtin(Builtin),
    /// The relation returned by `ref()` or `source()`.
    Relation,
    Context(ContextObject),
}

impl AValue {
    pub(crate) fn undefined() -> Self {
        AValue::Known(Value::UNDEFINED)
    }

    pub(crate) fn none() -> Self {
        AValue::Known(Value::from(()))
    }

    /// Whether two values are the same. Known values compare by kind and
    /// value, so `1`, `1.0` and `true` are different.
    pub(crate) fn same(&self, other: &AValue) -> bool {
        match (self, other) {
            (AValue::Known(a), AValue::Known(b)) => a.kind() == b.kind() && a == b,
            (AValue::Unknown(a), AValue::Unknown(b)) => a == b,
            (AValue::Heap(a), AValue::Heap(b)) => a == b,
            (AValue::Macro(a), AValue::Macro(b)) => {
                Arc::ptr_eq(&a.def, &b.def)
                    && match (&a.closure, &b.closure) {
                        (Closure::Project, Closure::Project) => true,
                        (Closure::TemplateRoot, Closure::TemplateRoot) => true,
                        (Closure::Captured(x), Closure::Captured(y)) => Arc::ptr_eq(x, y),
                        _ => false,
                    }
            }
            (AValue::Builtin(a), AValue::Builtin(b)) => a == b,
            (AValue::MacroNamespace(a), AValue::MacroNamespace(b)) => a == b,
            (AValue::Relation, AValue::Relation) => true,
            (AValue::Context(a), AValue::Context(b)) => a == b,
            _ => false,
        }
    }

    pub(crate) fn causes(&self) -> Option<&Causes> {
        match self {
            AValue::Unknown(causes) => Some(causes),
            _ => None,
        }
    }
}

/// A mutable container.
#[derive(Debug, Clone)]
pub(crate) enum HeapObj {
    List(Vec<AValue>),
    /// Insertion-ordered entries with known keys.
    Map(Vec<(Value, AValue)>),
    Namespace(BTreeMap<Arc<str>, AValue>),
    /// The contents are unknown, e.g. after being passed to an unknown call
    /// or after diverging on two paths.
    Clobbered(Causes),
}

impl HeapObj {
    fn same(&self, other: &HeapObj) -> bool {
        match (self, other) {
            (HeapObj::List(a), HeapObj::List(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.same(y))
            }
            (HeapObj::Map(a), HeapObj::Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka == kb && va.same(vb))
            }
            (HeapObj::Namespace(a), HeapObj::Namespace(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka == kb && va.same(vb))
            }
            (HeapObj::Clobbered(a), HeapObj::Clobbered(b)) => a == b,
            _ => false,
        }
    }

    /// Values directly held by this container.
    pub(crate) fn values(&self) -> Vec<&AValue> {
        match self {
            HeapObj::List(items) => items.iter().collect(),
            HeapObj::Map(entries) => entries.iter().map(|(_, v)| v).collect(),
            HeapObj::Namespace(attrs) => attrs.values().collect(),
            HeapObj::Clobbered(_) => Vec::new(),
        }
    }
}

pub(crate) type Scope = BTreeMap<Arc<str>, AValue>;

/// Text rendered by the current frame, kept only where it can become a
/// value (macro output and `{% set %}` blocks).
#[derive(Debug, Clone)]
pub(crate) enum Output {
    /// Not tracked (the template's own output).
    Discard,
    Known(String),
    Unknown(Causes),
}

impl Output {
    pub(crate) fn push_str(&mut self, text: &str) {
        if let Output::Known(buffer) = self {
            buffer.push_str(text);
        }
    }

    pub(crate) fn push_unknown(&mut self, causes: &Causes) {
        match self {
            Output::Discard => {}
            Output::Known(_) => *self = Output::Unknown(causes.clone()),
            Output::Unknown(existing) => *existing = existing.union(causes),
        }
    }

    pub(crate) fn into_value(self) -> AValue {
        match self {
            Output::Discard => AValue::none(),
            Output::Known(text) => AValue::Known(Value::from_safe_string(text)),
            Output::Unknown(causes) => AValue::Unknown(causes),
        }
    }

    fn same(&self, other: &Output) -> bool {
        match (self, other) {
            (Output::Discard, Output::Discard) => true,
            (Output::Known(a), Output::Known(b)) => a == b,
            (Output::Unknown(a), Output::Unknown(b)) => a == b,
            _ => false,
        }
    }
}

/// The containers of a state. The states of different paths share a
/// container until one of them changes it, so that copying a state does not
/// copy every container.
#[derive(Debug, Clone, Default)]
pub(crate) struct Heap(BTreeMap<HeapId, Arc<HeapObj>>);

impl Heap {
    pub(crate) fn get(&self, id: &HeapId) -> Option<&HeapObj> {
        self.0.get(id).map(Arc::as_ref)
    }

    /// The container to change, copied first if another state shares it.
    pub(crate) fn get_mut(&mut self, id: &HeapId) -> Option<&mut HeapObj> {
        self.0.get_mut(id).map(Arc::make_mut)
    }

    pub(crate) fn insert(&mut self, id: HeapId, obj: HeapObj) {
        self.0.insert(id, Arc::new(obj));
    }

    pub(crate) fn remove(&mut self, id: &HeapId) {
        self.0.remove(id);
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &HeapId> {
        self.0.keys()
    }

    /// Whether both heaps hold the same shared container at `id`, which
    /// then has not changed in either.
    pub(crate) fn shares(&self, other: &Heap, id: &HeapId) -> bool {
        matches!((self.0.get(id), other.0.get(id)), (Some(a), Some(b)) if Arc::ptr_eq(a, b))
    }
}

/// Whether two containers are the same, without comparing the contents of
/// one that both states share.
fn same_obj(a: &Arc<HeapObj>, b: &Arc<HeapObj>) -> bool {
    Arc::ptr_eq(a, b) || a.same(b)
}

/// The state of one frame on one execution path.
#[derive(Debug, Clone)]
pub(crate) struct State {
    pub(crate) scopes: Vec<Scope>,
    pub(crate) heap: Heap,
    pub(crate) out: Output,
    /// Reasons of the unknown conditions this path depends on.
    pub(crate) path: Causes,
}

impl State {
    pub(crate) fn new(scopes: Vec<Scope>, heap: Heap, out: Output, path: Causes) -> Self {
        Self {
            scopes,
            heap,
            out,
            path,
        }
    }

    /// Move the state out, leaving an empty one. Used where a statement
    /// continues on the moved state and the original is not read again, so
    /// that the scopes and heap are not copied.
    pub(crate) fn take(&mut self) -> State {
        std::mem::replace(
            self,
            State::new(
                Vec::new(),
                Heap::default(),
                Output::Discard,
                Causes::default(),
            ),
        )
    }

    pub(crate) fn lookup(&self, name: &str) -> Option<&AValue> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    pub(crate) fn assign(&mut self, name: Arc<str>, value: AValue) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, value);
        }
    }

    pub(crate) fn same(&self, other: &State) -> bool {
        self.path == other.path
            && self.out.same(&other.out)
            && self.scopes.len() == other.scopes.len()
            && self.scopes.iter().zip(&other.scopes).all(|(a, b)| {
                a.len() == b.len()
                    && a.iter()
                        .zip(b)
                        .all(|((ka, va), (kb, vb))| ka == kb && va.same(vb))
            })
            && self.heap.0.len() == other.heap.0.len()
            && self
                .heap
                .0
                .iter()
                .zip(&other.heap.0)
                .all(|((ia, a), (ib, b))| ia == ib && same_obj(a, b))
    }

    /// Make every container reachable from `values` unknown, because an
    /// unknown call may have changed it.
    pub(crate) fn clobber_reachable<'a>(
        &mut self,
        values: impl IntoIterator<Item = &'a AValue>,
        causes: &Causes,
    ) {
        let mut pending: Vec<HeapId> = values
            .into_iter()
            .filter_map(|value| match value {
                AValue::Heap(id) => Some(*id),
                _ => None,
            })
            .collect();
        let mut seen = BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            if let Some(obj) = self.heap.get(&id) {
                pending.extend(obj.values().into_iter().filter_map(|value| match value {
                    AValue::Heap(id) => Some(*id),
                    _ => None,
                }));
            }
            let merged = match self.heap.get(&id) {
                Some(HeapObj::Clobbered(existing)) => existing.union(causes),
                _ => causes.clone(),
            };
            self.heap.insert(id, HeapObj::Clobbered(merged));
        }
    }
}

/// Merge two values reached on different paths. `causes` explains why it is
/// not known which path was taken.
pub(crate) fn merge_value(a: &AValue, b: &AValue, causes: &Causes) -> AValue {
    if a.same(b) {
        return a.clone();
    }
    let mut merged = if causes.is_empty() {
        unexplained_merge()
    } else {
        causes.clone()
    };
    for value in [a, b] {
        if let AValue::Unknown(c) = value {
            merged = merged.union(c);
        }
    }
    AValue::Unknown(merged)
}

/// The reasons that tell two paths apart. Paths can depend on the same
/// conditions with opposite outcomes (one side of a branch returned), so when
/// the sets do not differ all their conditions are used.
pub(crate) fn path_difference(a: &Causes, b: &Causes, fallback: &Causes) -> Causes {
    let union = a.union(b);
    let diff = union.difference(&a.intersection(b));
    if !diff.is_empty() {
        diff
    } else if !fallback.is_empty() {
        fallback.clone()
    } else if !union.is_empty() {
        union
    } else {
        unexplained_merge()
    }
}

/// A merge whose paths could not be told apart. This should not happen; it
/// keeps an unknown value from having no reason.
fn unexplained_merge() -> Causes {
    Causes::one(
        crate::graph::dependency::DependencyReason::UnsupportedJinjaConstruct {
            construct: "path merge".to_string(),
            at: crate::graph::dependency::ExtractionSite::default(),
        },
    )
}

/// Merge two containers that differ.
fn merge_heap_obj(a: &HeapObj, b: &HeapObj, causes: &Causes) -> HeapObj {
    match (a, b) {
        (HeapObj::List(x), HeapObj::List(y)) if x.len() == y.len() => HeapObj::List(
            x.iter()
                .zip(y)
                .map(|(p, q)| merge_value(p, q, causes))
                .collect(),
        ),
        (HeapObj::Map(x), HeapObj::Map(y))
            if x.len() == y.len() && x.iter().zip(y).all(|((kx, _), (ky, _))| kx == ky) =>
        {
            HeapObj::Map(
                x.iter()
                    .zip(y)
                    .map(|((k, p), (_, q))| (k.clone(), merge_value(p, q, causes)))
                    .collect(),
            )
        }
        (HeapObj::Namespace(x), HeapObj::Namespace(y)) => {
            let keys: BTreeSet<&Arc<str>> = x.keys().chain(y.keys()).collect();
            HeapObj::Namespace(
                keys.into_iter()
                    .map(|key| {
                        let value = match (x.get(key), y.get(key)) {
                            (Some(p), Some(q)) => merge_value(p, q, causes),
                            _ => AValue::Unknown(causes.clone()),
                        };
                        (key.clone(), value)
                    })
                    .collect(),
            )
        }
        (HeapObj::Clobbered(x), HeapObj::Clobbered(y)) => {
            HeapObj::Clobbered(x.union(y).union(causes))
        }
        (HeapObj::Clobbered(x), _) | (_, HeapObj::Clobbered(x)) => {
            HeapObj::Clobbered(x.union(causes))
        }
        _ => HeapObj::Clobbered(causes.clone()),
    }
}

/// Merge two states of the same frame reached on different paths.
pub(crate) fn merge_states(a: State, b: State, fallback: &Causes) -> State {
    if a.same(&b) {
        return a;
    }
    let common = a.path.intersection(&b.path);
    let causes = path_difference(&a.path, &b.path, fallback);
    let scopes = a
        .scopes
        .iter()
        .zip(&b.scopes)
        .map(|(x, y)| {
            let keys: BTreeSet<&Arc<str>> = x.keys().chain(y.keys()).collect();
            keys.into_iter()
                .map(|key| {
                    let value = match (x.get(key), y.get(key)) {
                        (Some(p), Some(q)) => merge_value(p, q, &causes),
                        _ => AValue::Unknown(causes.clone()),
                    };
                    (key.clone(), value)
                })
                .collect()
        })
        .collect();
    let ids: BTreeSet<HeapId> = a.heap.keys().chain(b.heap.keys()).copied().collect();
    let heap = Heap(
        ids.into_iter()
            .map(|id| {
                let obj = match (a.heap.0.get(&id), b.heap.0.get(&id)) {
                    (Some(x), Some(y)) if same_obj(x, y) => x.clone(),
                    (Some(x), Some(y)) => Arc::new(merge_heap_obj(x, y, &causes)),
                    (Some(x), None) | (None, Some(x)) => x.clone(),
                    (None, None) => unreachable!(),
                };
                (id, obj)
            })
            .collect(),
    );
    let out = match (&a.out, &b.out) {
        (x, y) if x.same(y) => a.out.clone(),
        (Output::Discard, _) | (_, Output::Discard) => Output::Discard,
        (Output::Unknown(x), Output::Unknown(y)) => Output::Unknown(x.union(y).union(&causes)),
        (Output::Unknown(x), _) | (_, Output::Unknown(x)) => Output::Unknown(x.union(&causes)),
        _ => Output::Unknown(causes.clone()),
    };
    State {
        scopes,
        heap,
        out,
        path: common,
    }
}
