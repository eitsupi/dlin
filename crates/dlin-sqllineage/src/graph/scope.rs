use std::collections::HashSet;

use indexmap::IndexMap;

use crate::graph::node::NodeId;
use crate::types::TableRef;

pub(crate) type ScopeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct VirtualSourceId {
    scope: ScopeId,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VirtualColumnState {
    KnownEmpty,
    Unknown,
}

#[derive(Debug, Clone)]
pub(crate) struct VirtualColumn {
    pub name: String,
    pub dependencies: Vec<NodeId>,
    pub state: VirtualColumnState,
}

#[derive(Debug, Clone)]
pub(crate) struct VirtualSource {
    pub columns: Vec<VirtualColumn>,
}

pub(crate) struct ScopeTree {
    scopes: Vec<Scope>,
}

struct Scope {
    parent: Option<ScopeId>,
    /// Relations introduced by this scope's FROM clause, in source order.
    /// Iteration order is observable (star expansion, ambiguity candidates),
    /// so it must not depend on hashing.
    bindings: IndexMap<String, Binding>,
    /// Every physical relation added to this scope, including ones whose
    /// binding name was later taken by another relation (`s.t` and `o.t`
    /// share the name `t`). Multi-part qualifiers are matched against these.
    relations: Vec<TableRef>,
    /// Column names merged by a `JOIN ... USING (...)` in this scope. Such a
    /// name denotes one column of the join result, not one per relation.
    using_columns: HashSet<String>,
    /// Whether a `NATURAL JOIN` merges an unknown set of columns.
    natural_join: bool,
    anonymous_derived: Vec<ScopeId>,
    output_columns: Vec<ScopeColumn>,
    output_plan: OutputPlan,
    virtual_sources: Vec<VirtualSource>,
}

#[derive(Debug, Clone)]
pub(crate) enum ScopeKind {
    Root,
    Cte,
    Subquery,
    DerivedTable,
    SetOperation,
}

/// Describes how a scope's output columns are assembled. This is retained in
/// the raw graph until catalog expansion and positional set-operation merge.
#[derive(Debug, Clone)]
pub(crate) enum OutputPlan {
    Projection,
    SetOperation {
        left: ScopeId,
        right: ScopeId,
        recursive: bool,
    },
    Delegate(ScopeId),
}

#[derive(Debug, Clone)]
pub(crate) enum Binding {
    Table(TableRef),
    Cte(ScopeId),
    DerivedTable(ScopeId),
    VirtualSource(VirtualSourceId),
}

#[derive(Debug, Clone)]
pub(crate) struct ScopeColumn {
    pub name: String,
    pub node_id: NodeId,
}

impl ScopeTree {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope {
                parent: None,
                bindings: IndexMap::new(),
                relations: Vec::new(),
                using_columns: HashSet::new(),
                natural_join: false,
                anonymous_derived: Vec::new(),
                output_columns: Vec::new(),
                output_plan: OutputPlan::Projection,
                virtual_sources: Vec::new(),
            }],
        }
    }

    pub fn root() -> ScopeId {
        0
    }

    pub fn push(&mut self, parent: ScopeId, _kind: ScopeKind) -> ScopeId {
        let id = self.scopes.len();
        self.scopes.push(Scope {
            parent: Some(parent),
            bindings: IndexMap::new(),
            relations: Vec::new(),
            using_columns: HashSet::new(),
            natural_join: false,
            anonymous_derived: Vec::new(),
            output_columns: Vec::new(),
            output_plan: OutputPlan::Projection,
            virtual_sources: Vec::new(),
        });
        id
    }

    pub fn parent(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scopes[scope].parent
    }

    pub fn add_binding(&mut self, scope: ScopeId, name: String, binding: Binding) {
        if let Binding::Table(table) = &binding {
            self.scopes[scope].relations.push(table.clone());
        }
        self.scopes[scope].bindings.insert(name, binding);
    }

    pub fn add_using_column(&mut self, scope: ScopeId, name: String) {
        self.scopes[scope].using_columns.insert(name);
    }

    pub fn mark_natural_join(&mut self, scope: ScopeId) {
        self.scopes[scope].natural_join = true;
    }

    /// Whether a join in the scope that supplies unqualified names merges
    /// `name`, so that a column of that name shared by several relations is
    /// not ambiguous. A `NATURAL JOIN` merges columns the engine cannot see,
    /// so it is treated as merging every name.
    pub fn is_merged_column(&self, scope: ScopeId, name: &str) -> bool {
        let mut current = Some(scope);
        while let Some(id) = current {
            let s = &self.scopes[id];
            if !s.bindings.is_empty() {
                return s.natural_join || s.using_columns.contains(name);
            }
            current = s.parent;
        }
        false
    }

    pub fn add_virtual_source(
        &mut self,
        scope: ScopeId,
        columns: Vec<VirtualColumn>,
    ) -> VirtualSourceId {
        let index = self.scopes[scope].virtual_sources.len();
        self.scopes[scope]
            .virtual_sources
            .push(VirtualSource { columns });
        VirtualSourceId { scope, index }
    }

    pub fn virtual_source(&self, id: VirtualSourceId) -> &VirtualSource {
        &self.scopes[id.scope].virtual_sources[id.index]
    }

    pub fn add_output_column(&mut self, scope: ScopeId, col: ScopeColumn) {
        self.scopes[scope].output_columns.push(col);
    }

    /// Lookup a name starting from `scope`, walking the parent chain.
    /// Returns the first match (nearest scope wins = shadowing).
    pub fn lookup(&self, scope: ScopeId, name: &str) -> Option<&Binding> {
        let s = &self.scopes[scope];
        if let Some(b) = s.bindings.get(name) {
            return Some(b);
        }
        s.parent.and_then(|p| self.lookup(p, name))
    }

    /// Physical relations visible from `scope`, nearest scope first and in
    /// FROM order within a scope.
    pub fn visible_relations(&self, scope: ScopeId) -> Vec<&TableRef> {
        let mut result = Vec::new();
        let mut current = Some(scope);
        while let Some(id) = current {
            result.extend(self.scopes[id].relations.iter());
            current = self.scopes[id].parent;
        }
        result
    }

    pub fn output_columns(&self, scope: ScopeId) -> &[ScopeColumn] {
        &self.scopes[scope].output_columns
    }

    pub fn set_output_plan(&mut self, scope: ScopeId, plan: OutputPlan) {
        self.scopes[scope].output_plan = plan;
    }

    pub fn output_plan(&self, scope: ScopeId) -> &OutputPlan {
        &self.scopes[scope].output_plan
    }

    pub fn add_anonymous_derived(&mut self, parent: ScopeId, child: ScopeId) {
        self.scopes[parent].anonymous_derived.push(child);
    }

    pub fn anonymous_derived(&self, scope: ScopeId) -> &[ScopeId] {
        &self.scopes[scope].anonymous_derived
    }

    /// Bindings in the immediate scope only (no parent chain walk).
    pub fn immediate_bindings(&self, scope: ScopeId) -> Vec<(String, Binding)> {
        self.scopes[scope]
            .bindings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// All bindings visible from `scope` (nearest scope wins for shadowing).
    pub fn visible_bindings(&self, scope: ScopeId) -> Vec<(String, Binding)> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        let mut current = Some(scope);
        while let Some(id) = current {
            for (name, binding) in &self.scopes[id].bindings {
                if seen.insert(name.clone()) {
                    result.push((name.clone(), binding.clone()));
                }
            }
            current = self.scopes[id].parent;
        }
        result
    }
}
