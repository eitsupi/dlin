use std::collections::HashSet;

use indexmap::IndexMap;

use crate::Ident;
use crate::graph::node::NodeId;
use crate::types::{Dialect, TableRef};

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
    pub name: Ident,
    pub dependencies: Vec<NodeId>,
    pub state: VirtualColumnState,
}

#[derive(Debug, Clone)]
pub(crate) struct VirtualSource {
    pub columns: Vec<VirtualColumn>,
}

pub(crate) struct ScopeTree {
    scopes: Vec<Scope>,
    dialect: Dialect,
}

/// A point in the statement from which names are looked up: the entries of
/// `scope` added before position `visible`, then whatever the scope's outer
/// environment makes visible. Environments form a persistent chain through
/// [`Scope::outer`], so a reference point costs two words and never copies the
/// bindings it can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BindingEnv {
    pub scope: ScopeId,
    /// Number of entries of `scope` visible at this point, counting range
    /// variables and CTE definitions in the order they were introduced.
    pub visible: usize,
}

/// A range variable of a FROM clause.
#[derive(Debug, Clone)]
struct BindingEntry {
    name: Ident,
    binding: Binding,
    /// Position among the entries introduced to the scope.
    seq: usize,
    /// Position of the entry that took this one's name over, if any. The
    /// entry stays visible to environments from before that position.
    until: Option<usize>,
}

impl BindingEntry {
    fn visible_at(&self, visible: usize) -> bool {
        self.seq < visible && self.until.is_none_or(|until| visible <= until)
    }
}

/// A CTE defined by a WITH clause. CTE names live in their own namespace:
/// they are read by relation names and never by qualifying a column.
#[derive(Debug, Clone)]
struct CteEntry {
    name: Ident,
    scope: ScopeId,
    seq: usize,
}

struct Scope {
    parent: Option<ScopeId>,
    /// How the scope was introduced, which decides what it falls back to when
    /// its own FROM clause does not settle a name (see [`ScopeTree::outer`]).
    kind: ScopeKind,
    /// How many entries `parent` had when this scope was created.
    parent_visible: usize,
    /// Relations introduced by this scope's FROM clause, in source order.
    /// Iteration order is observable (star expansion, ambiguity candidates),
    /// so it must not depend on hashing.
    bindings: IndexMap<String, Vec<BindingEntry>>,
    /// CTEs defined in this scope, in source order.
    ctes: Vec<CteEntry>,
    /// Every physical relation added to this scope, including ones whose
    /// binding name was later taken by another relation (`s.t` and `o.t`
    /// share the name `t`), with the position it was added at. Multi-part
    /// qualifiers are matched against these.
    relations: Vec<(usize, TableRef)>,
    /// Names in the order entries were introduced, including names that a
    /// later relation took over. A join reads the slice added while its
    /// operands were visited to learn which relations it combines.
    binding_log: Vec<Ident>,
    /// Joins in this scope that merge columns, in source order.
    join_merges: Vec<JoinMerge>,
    /// Derived tables without a range variable, with the position they were
    /// added at.
    anonymous_derived: Vec<(usize, ScopeId)>,
    output_columns: Vec<ScopeColumn>,
    output_plan: OutputPlan,
    unknown_output_width: bool,
    unknown_output_reasons: Vec<&'static str>,
    virtual_sources: Vec<VirtualSource>,
}

impl Scope {
    fn new(parent: Option<ScopeId>, kind: ScopeKind, parent_visible: usize) -> Self {
        Self {
            parent,
            kind,
            parent_visible,
            bindings: IndexMap::new(),
            ctes: Vec::new(),
            relations: Vec::new(),
            binding_log: Vec::new(),
            join_merges: Vec::new(),
            anonymous_derived: Vec::new(),
            output_columns: Vec::new(),
            output_plan: OutputPlan::Projection,
            unknown_output_width: false,
            unknown_output_reasons: Vec::new(),
            virtual_sources: Vec::new(),
        }
    }

    /// Entries of the range-variable namespace visible at `visible`.
    fn visible_entries(&self, visible: usize) -> impl Iterator<Item = &BindingEntry> {
        self.bindings
            .values()
            .flatten()
            .filter(move |entry| entry.visible_at(visible))
    }
}

/// The columns a join merges into one column of its result.
#[derive(Debug, Clone)]
pub(crate) enum MergedColumns {
    /// `USING (...)`: the listed names.
    Using(Vec<Ident>),
    /// `NATURAL`: every name both sides publish.
    Natural,
}

/// A `USING` or `NATURAL` join. Each side lists the binding names of the
/// relations it combines (several for an earlier join or a parenthesized
/// join), so a merged column can be attributed to exactly those relations
/// rather than to the whole scope.
#[derive(Debug, Clone)]
pub(crate) struct JoinMerge {
    pub left: Vec<Ident>,
    pub right: Vec<Ident>,
    pub columns: MergedColumns,
    pub kind: JoinMergeKind,
    /// Number of entries the scope had when the join was complete. An
    /// environment from before that position does not see the join.
    pub at: usize,
}

/// Which side supplies the value of a merged column.
#[derive(Debug, Clone, Copy)]
pub(crate) enum JoinMergeKind {
    Inner,
    Left,
    Right,
    Full,
}

/// The CTEs a relation name may denote; see [`ScopeTree::lookup_cte`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CteLookup {
    pub visible: Option<ScopeId>,
    pub hidden: Option<ScopeId>,
}

/// How a scope is introduced, which decides what it can see of the scope
/// around it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScopeKind {
    /// The statement itself.
    Root,
    /// A CTE definition; it sees the CTEs defined before it.
    Cte,
    /// The body of a query that has a WITH clause; it sees every CTE.
    Body,
    /// A subquery inside an expression; it sees everything visible at the
    /// expression.
    Subquery,
    /// A derived table in FROM. The other items of the FROM clause it belongs
    /// to are not visible to it.
    DerivedTable,
    /// A `LATERAL` derived table (or the right side of an `APPLY`); the items
    /// of its FROM clause before it are visible.
    LateralDerivedTable,
    /// One operand of a set operation.
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
    pub name: Ident,
    pub node_id: NodeId,
}

impl ScopeTree {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            scopes: vec![Scope::new(None, ScopeKind::Root, 0)],
        }
    }

    pub fn root() -> ScopeId {
        0
    }

    /// Create a scope under `parent`. What the new scope can see of the
    /// enclosing query follows from its kind and from how much of `parent` has
    /// been introduced so far.
    pub fn push(&mut self, parent: ScopeId, kind: ScopeKind) -> ScopeId {
        let parent_visible = self.scopes[parent].binding_log.len();
        let id = self.scopes.len();
        self.scopes
            .push(Scope::new(Some(parent), kind, parent_visible));
        id
    }

    /// What names fall back to when the scope's own FROM clause does not
    /// settle them. It depends on how the scope is introduced:
    ///
    /// - an ordinary derived table cannot see the other items of the FROM
    ///   clause it belongs to, only what its parent could see from outside;
    /// - a lateral derived table also sees the items before it;
    /// - a subquery in an expression, a set-operation operand, a CTE and the
    ///   body of a query with a WITH clause see what was introduced in their
    ///   parent before them (for a CTE, the CTEs defined earlier).
    fn outer(&self, scope: ScopeId) -> Option<BindingEnv> {
        let this = &self.scopes[scope];
        let parent = this.parent?;
        match this.kind {
            ScopeKind::Root => None,
            ScopeKind::DerivedTable => self.outer(parent),
            ScopeKind::Cte
            | ScopeKind::Body
            | ScopeKind::Subquery
            | ScopeKind::LateralDerivedTable
            | ScopeKind::SetOperation => Some(BindingEnv {
                scope: parent,
                visible: this.parent_visible,
            }),
        }
    }

    pub fn parent(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scopes[scope].parent
    }

    /// The environment at the current end of `scope`: everything introduced so
    /// far is visible.
    pub fn env(&self, scope: ScopeId) -> BindingEnv {
        BindingEnv {
            scope,
            visible: self.scopes[scope].binding_log.len(),
        }
    }

    /// The scopes an environment looks names up in, nearest first, each with
    /// the number of its entries that are visible.
    fn chain(&self, env: BindingEnv) -> impl Iterator<Item = BindingEnv> + '_ {
        std::iter::successors(Some(env), |step| self.outer(step.scope))
    }

    pub fn add_binding(&mut self, scope: ScopeId, name: Ident, binding: Binding) {
        let key = name.column_key(self.dialect).into_owned();
        let seq = self.scopes[scope].binding_log.len();
        self.scopes[scope].binding_log.push(name.clone());
        if let Binding::Table(table) = &binding {
            self.scopes[scope].relations.push((seq, table.clone()));
        }
        let group = self.scopes[scope].bindings.entry(key).or_default();
        // Lateral virtual sources intentionally replace the range variable
        // whose value they expand; its captured dependencies remain intact.
        // Environments from before the replacement still see the old entry.
        if matches!(binding, Binding::VirtualSource(_)) {
            for entry in group.iter_mut().filter(|entry| entry.until.is_none()) {
                entry.until = Some(seq);
            }
        }
        group.push(BindingEntry {
            name,
            binding,
            seq,
            until: None,
        });
    }

    /// Define the CTEs of one WITH list in `holder`, in source order, and
    /// create a scope for each body. All of them are defined before any body
    /// is visited, so a body can refer to a CTE that comes later.
    ///
    /// With `forward`, a body sees every CTE of the list, itself and later
    /// ones included. Without it, a body sees only the CTEs before it. Either
    /// way it sees what the holder's own surroundings allow at its position.
    pub fn define_ctes(
        &mut self,
        holder: ScopeId,
        names: Vec<Ident>,
        forward: bool,
    ) -> Vec<ScopeId> {
        let base = self.scopes[holder].binding_log.len();
        let total = names.len();
        let mut scopes = Vec::with_capacity(total);
        for (index, name) in names.into_iter().enumerate() {
            let visible = if forward { base + total } else { base + index };
            let id = self.scopes.len();
            self.scopes
                .push(Scope::new(Some(holder), ScopeKind::Cte, visible));
            self.scopes[holder].binding_log.push(name.clone());
            self.scopes[holder].ctes.push(CteEntry {
                name,
                scope: id,
                seq: base + index,
            });
            scopes.push(id);
        }
        scopes
    }

    /// The CTEs a relation name written without a schema may denote from
    /// `env`.
    ///
    /// `visible` is the nearest CTE of that name the position can see.
    /// `hidden` is a CTE of that name defined in a WITH list the position is
    /// inside of but cannot see (a later one, or the CTE itself), when no
    /// visible one is nearer. Whether a hidden CTE is simply not there depends
    /// on the dialect, so the caller decides what it means.
    pub fn lookup_cte(&self, env: BindingEnv, name: &Ident) -> CteLookup {
        let key = name.column_key(self.dialect);
        let mut lookup = CteLookup::default();
        for step in self.chain(env) {
            let mut matching = self.scopes[step.scope]
                .ctes
                .iter()
                .filter(|cte| cte.name.column_key(self.dialect) == key);
            if let Some(cte) = matching.clone().rev().find(|cte| cte.seq < step.visible) {
                lookup.visible = Some(cte.scope);
                return lookup;
            }
            if lookup.hidden.is_none() {
                lookup.hidden = matching.next().map(|cte| cte.scope);
            }
        }
        lookup
    }

    /// Number of entries added to `scope` so far; pass it to
    /// [`Self::bindings_added_since`] to learn what a FROM item introduced.
    pub fn binding_mark(&self, scope: ScopeId) -> usize {
        self.scopes[scope].binding_log.len()
    }

    /// Binding names added to `scope` since `mark`, in the order added.
    pub fn bindings_added_since(&self, scope: ScopeId, mark: usize) -> Vec<Ident> {
        self.scopes[scope].binding_log[mark..].to_vec()
    }

    pub fn add_join_merge(&mut self, scope: ScopeId, mut merge: JoinMerge) {
        merge.at = self.scopes[scope].binding_log.len();
        self.scopes[scope].join_merges.push(merge);
    }

    /// Whether `scope` or a scope around it has introduced a range variable,
    /// visible from `scope` or not.
    pub fn has_enclosing_bindings(&self, scope: ScopeId) -> bool {
        std::iter::successors(Some(scope), |&id| self.scopes[id].parent)
            .any(|id| !self.scopes[id].bindings.is_empty())
    }

    /// The extent of the FROM clause that decides unqualified names at `env`.
    /// Two environments that see the same range variables of the same scope
    /// have the same FROM environment, so it can key a cache. A scope with no
    /// visible range variable of its own is normalized to none, and falls back
    /// to its surroundings.
    pub fn clause_env(&self, env: BindingEnv) -> BindingEnv {
        let visible = self.scopes[env.scope]
            .visible_entries(env.visible)
            .map(|entry| entry.seq + 1)
            .max()
            .unwrap_or(0);
        BindingEnv {
            scope: env.scope,
            visible,
        }
    }

    /// Joins that merge columns among the relations an unqualified name is
    /// looked up in: those of the nearest scope with a FROM clause, in source
    /// order.
    pub fn join_merges(&self, env: BindingEnv) -> &[JoinMerge] {
        for step in self.chain(env) {
            let scope = &self.scopes[step.scope];
            if scope.visible_entries(step.visible).next().is_some() {
                let count = scope
                    .join_merges
                    .partition_point(|merge| merge.at <= step.visible);
                return &scope.join_merges[..count];
            }
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

    /// Look up a range variable from `env`. The nearest scope that has the
    /// name wins; a conflicting group has no unique binding.
    pub fn lookup(&self, env: BindingEnv, name: &Ident) -> Option<&Binding> {
        let key = name.column_key(self.dialect);
        for step in self.chain(env) {
            let mut group = self.scopes[step.scope]
                .bindings
                .get(key.as_ref())
                .into_iter()
                .flatten()
                .filter(|entry| entry.visible_at(step.visible));
            if let Some(first) = group.next() {
                return group.next().is_none().then_some(&first.binding);
            }
        }
        None
    }

    /// Conflicting range variables in the nearest scope that publishes this name.
    pub fn ambiguous_binding_candidates(
        &self,
        env: BindingEnv,
        name: &Ident,
    ) -> Option<Vec<crate::Candidate>> {
        let key = name.column_key(self.dialect);
        for step in self.chain(env) {
            let group: Vec<&BindingEntry> = self.scopes[step.scope]
                .bindings
                .get(key.as_ref())
                .into_iter()
                .flatten()
                .filter(|entry| entry.visible_at(step.visible))
                .collect();
            if group.is_empty() {
                continue;
            }
            return (group.len() > 1).then(|| {
                group
                    .into_iter()
                    .filter_map(|entry| match &entry.binding {
                        Binding::Table(table) => {
                            Some(crate::Candidate::Relation(crate::Relation {
                                catalog: table.catalog.clone(),
                                schema: table.schema.clone(),
                                name: table.table.clone(),
                            }))
                        }
                        Binding::Cte(_) => Some(crate::Candidate::Cte(entry.name.clone())),
                        Binding::DerivedTable(_) => {
                            Some(crate::Candidate::Derived(Some(entry.name.clone())))
                        }
                        Binding::VirtualSource(_) => None,
                    })
                    .collect()
            });
        }
        None
    }

    /// Physical relations visible from `env`, nearest scope first and in FROM
    /// order within a scope.
    pub fn visible_relations(&self, env: BindingEnv) -> Vec<&TableRef> {
        self.chain(env)
            .flat_map(|step| {
                self.scopes[step.scope]
                    .relations
                    .iter()
                    .filter(move |(seq, _)| *seq < step.visible)
                    .map(|(_, table)| table)
            })
            .collect()
    }

    pub fn output_columns(&self, scope: ScopeId) -> &[ScopeColumn] {
        &self.scopes[scope].output_columns
    }

    pub fn mark_unknown_output_width(&mut self, scope: ScopeId, reason: &'static str) {
        self.scopes[scope].unknown_output_width = true;
        if !self.scopes[scope].unknown_output_reasons.contains(&reason) {
            self.scopes[scope].unknown_output_reasons.push(reason);
        }
    }

    pub fn output_width_unknown(&self, scope: ScopeId) -> bool {
        self.scopes[scope].unknown_output_width
    }

    /// Follow only the output plan, not nested FROM bindings or CTE definitions.
    pub fn has_unknown_output_width(&self, scope: ScopeId) -> bool {
        self.output_width_unknown(scope)
            || match self.output_plan(scope) {
                OutputPlan::Projection => false,
                OutputPlan::Delegate(child) => self.has_unknown_output_width(*child),
                OutputPlan::SetOperation { left, right, .. } => {
                    self.has_unknown_output_width(*left) || self.has_unknown_output_width(*right)
                }
            }
    }

    /// Reasons an output's width is unknown, following only output-producing
    /// scopes so unrelated nested queries do not taint this result.
    pub fn unknown_output_reasons(&self, scope: ScopeId) -> Vec<&'static str> {
        let current = &self.scopes[scope];
        let mut reasons = current.unknown_output_reasons.clone();
        match &current.output_plan {
            OutputPlan::Projection => {}
            OutputPlan::Delegate(child) => {
                for reason in self.unknown_output_reasons(*child) {
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
            OutputPlan::SetOperation { left, right, .. } => {
                let mut branches = self.unknown_output_reasons(*left);
                branches.extend(self.unknown_output_reasons(*right));
                for reason in branches {
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
        }
        reasons
    }

    pub fn set_output_plan(&mut self, scope: ScopeId, plan: OutputPlan) {
        self.scopes[scope].output_plan = plan;
    }

    pub fn output_plan(&self, scope: ScopeId) -> &OutputPlan {
        &self.scopes[scope].output_plan
    }

    pub fn add_anonymous_derived(&mut self, parent: ScopeId, child: ScopeId) {
        let at = self.scopes[parent].binding_log.len();
        self.scopes[parent].anonymous_derived.push((at, child));
    }

    /// Derived tables without a range variable in the FROM clause at `env`.
    pub fn anonymous_derived(&self, env: BindingEnv) -> Vec<ScopeId> {
        self.scopes[env.scope]
            .anonymous_derived
            .iter()
            .filter(|(at, _)| *at <= env.visible)
            .map(|(_, child)| *child)
            .collect()
    }

    /// Range variables of the FROM clause that decides unqualified names at
    /// `env`: those of the scope itself when it has any visible, otherwise
    /// everything its surroundings make visible.
    pub fn clause_bindings(&self, env: BindingEnv) -> Vec<(Ident, Binding)> {
        let own: Vec<_> = self.scopes[env.scope]
            .visible_entries(env.visible)
            .map(|entry| (entry.name.clone(), entry.binding.clone()))
            .collect();
        if own.is_empty() {
            self.visible_bindings(env)
        } else {
            own
        }
    }

    /// The first range variable, in scope creation order, that reads `target`
    /// as a CTE or derived table. A CTE is named by its definition.
    pub fn reading_binding(&self, target: ScopeId) -> Option<(Ident, Binding)> {
        self.scopes.iter().find_map(|scope| {
            scope
                .ctes
                .iter()
                .find(|cte| cte.scope == target)
                .map(|cte| (cte.name.clone(), Binding::Cte(target)))
                .or_else(|| {
                    scope
                        .bindings
                        .values()
                        .flatten()
                        .find(|entry| {
                            matches!(&entry.binding, Binding::Cte(read) | Binding::DerivedTable(read) if *read == target)
                        })
                        .map(|entry| (entry.name.clone(), entry.binding.clone()))
                })
        })
    }

    /// All range variables visible from `env` (nearest scope wins for
    /// shadowing).
    pub fn visible_bindings(&self, env: BindingEnv) -> Vec<(Ident, Binding)> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for step in self.chain(env) {
            for (key, group) in &self.scopes[step.scope].bindings {
                let mut visible = group
                    .iter()
                    .filter(|entry| entry.visible_at(step.visible))
                    .peekable();
                if visible.peek().is_some() && seen.insert(key.clone()) {
                    result.extend(visible.map(|entry| (entry.name.clone(), entry.binding.clone())));
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> Binding {
        Binding::Table(TableRef::new(name))
    }

    fn sees(tree: &ScopeTree, env: BindingEnv, name: &str) -> bool {
        tree.lookup(env, &Ident::unquoted(name)).is_some()
    }

    #[test]
    fn a_derived_table_does_not_see_the_items_of_its_own_from_clause() {
        let mut tree = ScopeTree::new(Dialect::Generic);
        let root = ScopeTree::root();
        tree.add_binding(root, Ident::unquoted("before"), table("before"));
        let ordinary = tree.push(root, ScopeKind::DerivedTable);
        let lateral = tree.push(root, ScopeKind::LateralDerivedTable);
        tree.add_binding(root, Ident::unquoted("after"), table("after"));

        for scope in [ordinary, lateral] {
            assert!(!sees(&tree, tree.env(scope), "after"));
        }
        assert!(!sees(&tree, tree.env(ordinary), "before"));
        assert!(sees(&tree, tree.env(lateral), "before"));
        // The enclosing query sees both.
        assert!(sees(&tree, tree.env(root), "before"));
        assert!(sees(&tree, tree.env(root), "after"));
    }

    #[test]
    fn a_derived_table_still_sees_what_surrounds_its_query() {
        let mut tree = ScopeTree::new(Dialect::Generic);
        let root = ScopeTree::root();
        tree.add_binding(root, Ident::unquoted("outer"), table("outer"));
        let subquery = tree.push(root, ScopeKind::Subquery);
        tree.add_binding(subquery, Ident::unquoted("sibling"), table("sibling"));
        let derived = tree.push(subquery, ScopeKind::DerivedTable);
        assert!(sees(&tree, tree.env(derived), "outer"));
        assert!(!sees(&tree, tree.env(derived), "sibling"));
    }

    #[test]
    fn an_environment_taken_earlier_does_not_see_later_items() {
        let mut tree = ScopeTree::new(Dialect::Generic);
        let root = ScopeTree::root();
        tree.add_binding(root, Ident::unquoted("a"), table("a"));
        let earlier = tree.env(root);
        tree.add_binding(root, Ident::unquoted("b"), table("b"));
        assert!(sees(&tree, earlier, "a"));
        assert!(!sees(&tree, earlier, "b"));
        assert_eq!(tree.visible_bindings(earlier).len(), 1);
        assert_eq!(tree.visible_bindings(tree.env(root)).len(), 2);
    }

    #[test]
    fn a_lateral_source_replaces_a_name_only_for_later_environments() {
        let mut tree = ScopeTree::new(Dialect::Generic);
        let root = ScopeTree::root();
        tree.add_binding(root, Ident::unquoted("base"), table("base"));
        let before = tree.env(root);
        let source = tree.add_virtual_source(root, Vec::new());
        tree.add_binding(
            root,
            Ident::unquoted("base"),
            Binding::VirtualSource(source),
        );
        assert!(matches!(
            tree.lookup(before, &Ident::unquoted("base")),
            Some(Binding::Table(_))
        ));
        assert!(matches!(
            tree.lookup(tree.env(root), &Ident::unquoted("base")),
            Some(Binding::VirtualSource(_))
        ));
    }

    #[test]
    fn ctes_are_a_namespace_of_their_own() {
        let mut tree = ScopeTree::new(Dialect::Generic);
        let root = ScopeTree::root();
        let ctes = tree.define_ctes(
            root,
            vec![Ident::unquoted("c1"), Ident::unquoted("c2")],
            false,
        );
        let (first, second) = (ctes[0], ctes[1]);
        let first_env = tree.env(first);
        let body = tree.push(root, ScopeKind::Body);

        let c1 = Ident::unquoted("c1");
        let c2 = Ident::unquoted("c2");
        // A CTE is not a range variable.
        assert!(!sees(&tree, tree.env(body), "c1"));
        // A CTE sees the ones defined before it, not itself or later ones.
        let visible = |env, name: &Ident| tree.lookup_cte(env, name).visible;
        assert_eq!(visible(first_env, &c1), None);
        // The CTE itself and a later one are hidden, not absent.
        assert_eq!(tree.lookup_cte(first_env, &c1).hidden, Some(first));
        assert_eq!(tree.lookup_cte(tree.env(first), &c2).hidden, Some(second));
        assert_eq!(visible(tree.env(second), &c1), Some(first));
        assert_eq!(visible(tree.env(second), &c2), None);
        // The body sees all of them.
        assert_eq!(visible(tree.env(body), &c2), Some(second));
        // A range variable in the body does not hide a CTE.
        tree.add_binding(body, Ident::unquoted("c1"), table("t2"));
        assert_eq!(tree.lookup_cte(tree.env(body), &c1).visible, Some(first));
    }

    #[test]
    fn a_recursive_list_makes_every_cte_visible_to_every_body() {
        let mut tree = ScopeTree::new(Dialect::Generic);
        let root = ScopeTree::root();
        let ctes = tree.define_ctes(
            root,
            vec![Ident::unquoted("c1"), Ident::unquoted("c2")],
            true,
        );
        for &scope in &ctes {
            assert_eq!(
                tree.lookup_cte(tree.env(scope), &Ident::unquoted("c1"))
                    .visible,
                Some(ctes[0])
            );
            assert_eq!(
                tree.lookup_cte(tree.env(scope), &Ident::unquoted("c2"))
                    .visible,
                Some(ctes[1])
            );
        }
    }
}
