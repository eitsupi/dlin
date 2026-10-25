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
    /// Binding names in the order they were added, including names that a
    /// later relation took over. A join reads the slice added while its
    /// operands were visited to learn which relations it combines.
    binding_log: Vec<String>,
    /// Joins in this scope that merge columns, in source order.
    join_merges: Vec<JoinMerge>,
    anonymous_derived: Vec<ScopeId>,
    output_columns: Vec<ScopeColumn>,
    output_plan: OutputPlan,
    virtual_sources: Vec<VirtualSource>,
}

/// The columns a join merges into one column of its result.
#[derive(Debug, Clone)]
pub(crate) enum MergedColumns {
    /// `USING (...)`: the listed names.
    Using(Vec<String>),
    /// `NATURAL`: every name both sides publish.
    Natural,
}

/// A `USING` or `NATURAL` join. Each side lists the binding names of the
/// relations it combines (several for an earlier join or a parenthesized
/// join), so a merged column can be attributed to exactly those relations
/// rather than to the whole scope.
#[derive(Debug, Clone)]
pub(crate) struct JoinMerge {
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub columns: MergedColumns,
    pub kind: JoinMergeKind,
}

/// Which side supplies the value of a merged column.
#[derive(Debug, Clone, Copy)]
pub(crate) enum JoinMergeKind {
    Inner,
    Left,
    Right,
    Full,
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
                binding_log: Vec::new(),
                join_merges: Vec::new(),
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
            binding_log: Vec::new(),
            join_merges: Vec::new(),
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
        self.scopes[scope].binding_log.push(name.clone());
        if let Binding::Table(table) = &binding {
            self.scopes[scope].relations.push(table.clone());
        }
        self.scopes[scope].bindings.insert(name, binding);
    }

    /// Number of bindings added to `scope` so far; pass it to
    /// [`Self::bindings_added_since`] to learn what a FROM item introduced.
    pub fn binding_mark(&self, scope: ScopeId) -> usize {
        self.scopes[scope].binding_log.len()
    }

    /// Binding names added to `scope` since `mark`, in the order added.
    pub fn bindings_added_since(&self, scope: ScopeId, mark: usize) -> Vec<String> {
        self.scopes[scope].binding_log[mark..].to_vec()
    }

    pub fn add_join_merge(&mut self, scope: ScopeId, merge: JoinMerge) {
        self.scopes[scope].join_merges.push(merge);
    }

    /// Joins that merge columns among the relations an unqualified name is
    /// looked up in: those of the nearest scope with a FROM clause, in source
    /// order.
    pub fn join_merges(&self, scope: ScopeId) -> &[JoinMerge] {
        let mut current = Some(scope);
        while let Some(id) = current {
            let s = &self.scopes[id];
            if !s.bindings.is_empty() {
                return &s.join_merges;
            }
            current = s.parent;
        }
        &[]
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
