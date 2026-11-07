//! Binding of the relation names a query reads.

use crate::Dialect;
use crate::build::LineageBuilder;
use crate::graph::scope::{Binding, ScopeId, ScopeKind};
use crate::types::TableRef;

/// What a CTE's reference to its own name means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OwnName {
    /// The CTE is recursive and reads itself.
    Recursive,
    /// The name is not the CTE here, so it is a physical relation.
    Physical,
    /// The query is not valid, or the dialect's rule is not modelled: the
    /// name may be the CTE or a physical relation, and which cannot be told.
    Unresolved,
}

/// Which CTEs of one WITH list a CTE body can see. It differs between
/// dialects, so it is decided in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CteListRule {
    /// Later CTEs of the list, and the CTE itself, are visible in a body.
    pub forward: bool,
    /// What a reference to the CTE's own name means.
    pub own_name: OwnName,
    /// Whether the dialect's rule is known. Without it, a name that matches a
    /// CTE the body cannot see is not assumed to be a physical relation.
    pub modelled: bool,
}

/// The visibility rule of a WITH list; `recursive` is whether the list was
/// written with `RECURSIVE`.
///
/// - `PostgreSQL`, `BigQuery`, ANSI and the generic dialect (which follow the
///   SQL standard): without `RECURSIVE` a body sees the CTEs before it; with it,
///   every CTE of the list.
/// - `DuckDB`: a body sees only earlier CTEs even with `RECURSIVE`, which is
///   needed for a CTE to read itself.
/// - Snowflake and SQL Server: a body sees only earlier CTEs, and a CTE may
///   read itself with or without `RECURSIVE`.
/// - `SQLite`: every CTE of the list is visible with or without `RECURSIVE`,
///   and a CTE may read itself without the keyword.
/// - Any other dialect: earlier CTEs, and the CTE itself only under
///   `RECURSIVE`. A name that matches a CTE the body cannot see stays
///   unresolved between the CTE and a physical relation.
pub(crate) fn cte_list_rule(dialect: Dialect, recursive: bool) -> CteListRule {
    let own_name = if recursive {
        OwnName::Recursive
    } else {
        OwnName::Unresolved
    };
    match dialect {
        Dialect::Generic | Dialect::Ansi | Dialect::PostgreSql | Dialect::BigQuery => CteListRule {
            forward: recursive,
            own_name: if recursive {
                OwnName::Recursive
            } else {
                OwnName::Physical
            },
            modelled: true,
        },
        Dialect::DuckDb => CteListRule {
            forward: false,
            own_name,
            modelled: true,
        },
        // These read themselves without the keyword, so `RECURSIVE` changes
        // nothing; SQL Server has no such keyword at all.
        Dialect::Snowflake | Dialect::MsSql => CteListRule {
            forward: false,
            own_name: OwnName::Recursive,
            modelled: true,
        },
        Dialect::SQLite => CteListRule {
            forward: true,
            own_name: OwnName::Recursive,
            modelled: true,
        },
        _ => CteListRule {
            forward: false,
            own_name,
            modelled: false,
        },
    }
}

/// A CTE whose body is being visited.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DefiningCte {
    pub scope: ScopeId,
    /// What a reference to the CTE's own name means in its body.
    pub own_name: OwnName,
}

impl LineageBuilder {
    /// The CTE scope a set operation built in `scope` is the body of, when it
    /// is one: the scope itself, or the scopes a body is split into (the
    /// body of a nested WITH, an operand of an enclosing set operation).
    /// A set operation inside a derived table or an expression subquery of
    /// the CTE is an ordinary one.
    pub(crate) fn cte_of_body_scope(&self, scope: ScopeId) -> Option<ScopeId> {
        let mut scope = scope;
        loop {
            match self.graph.scopes.kind(scope) {
                ScopeKind::Cte => return Some(scope),
                ScopeKind::Body | ScopeKind::SetOperation => {
                    scope = self.graph.scopes.parent(scope)?;
                }
                _ => return None,
            }
        }
    }

    /// Bind the relation named by a FROM item (or a `TABLE` query) and record
    /// that it is read. This is the only place that decides what a relation
    /// name refers to, so every syntax that names a relation follows the same
    /// rules:
    ///
    /// - A name written with a schema or catalog is always a physical
    ///   relation. CTEs have no schema, so `public.c` never reads the CTE `c`.
    /// - A bare name is the nearest CTE visible at this position, and
    ///   otherwise a physical relation. A WITH nested in a CTE body therefore
    ///   shadows the CTE being defined like any other outer CTE. Whether a CTE
    ///   is visible in its own body is decided when the WITH list is defined,
    ///   by the dialect's rule.
    /// - A bare name that matches a CTE the position cannot see (a later one,
    ///   or the CTE itself) is a physical relation where the dialect's rule is
    ///   known. Where it is not, or where the query is not valid, the name is
    ///   bound as both the CTE and the physical relation: the same range
    ///   variable twice, which makes any use of it ambiguous between them.
    /// - Only a physical relation is recorded as read; a CTE's own definition
    ///   records what it reads.
    /// - A name that is bound to a CTE whose body is being visited is that
    ///   CTE reading itself. It is recorded, so that the set operation it
    ///   appears in is known to be recursive, and it binds a relation that is
    ///   not read, since the recursive term is not evaluated.
    ///
    /// The result is the bindings the FROM item's range variable takes.
    pub(crate) fn bind_relation_reference(&mut self, table: TableRef) -> Vec<Binding> {
        if table.catalog.is_some() || table.schema.is_some() {
            return vec![self.read_physical(table)];
        }
        let modelled = cte_list_rule(self.dialect, false).modelled;
        let found = self.graph.scopes.lookup_cte(self.env(), &table.table);
        if let Some(scope) = found.visible
            && (found.hidden.is_none() || modelled)
        {
            if self.defining_ctes.iter().any(|cte| cte.scope == scope) {
                self.self_references.push(scope);
                return vec![Binding::Table(table)];
            }
            return vec![Binding::Cte(scope)];
        }
        match found.hidden {
            Some(scope) if !modelled || self.cte_own_name_is_unresolved(scope) => {
                vec![Binding::Cte(scope), self.read_physical(table)]
            }
            _ => vec![self.read_physical(table)],
        }
    }

    /// Whether `scope` is a CTE being defined whose own name cannot be told
    /// from a physical relation.
    fn cte_own_name_is_unresolved(&self, scope: ScopeId) -> bool {
        self.defining_ctes
            .iter()
            .any(|cte| cte.scope == scope && cte.own_name == OwnName::Unresolved)
    }

    fn read_physical(&mut self, table: TableRef) -> Binding {
        self.graph.tables.inputs.push(table.clone());
        Binding::Table(table)
    }
}
