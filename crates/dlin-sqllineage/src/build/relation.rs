//! Binding of the relation names a query reads.

use crate::Dialect;
use crate::build::LineageBuilder;
use crate::graph::scope::{Binding, ScopeId};
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

/// The CTE whose body is being visited.
#[derive(Debug, Clone)]
pub(crate) struct OwnCte {
    pub name: crate::Ident,
    pub scope: ScopeId,
    pub rule: OwnName,
}

impl LineageBuilder {
    /// Whether the CTE being defined reads itself.
    pub(crate) fn defining_recursive_cte(&self) -> bool {
        self.own_cte
            .as_ref()
            .is_some_and(|own| own.rule == OwnName::Recursive)
    }

    /// Bind the relation named by a FROM item (or a `TABLE` query) and record
    /// that it is read. This is the only place that decides what a relation
    /// name refers to, so every syntax that names a relation follows the same
    /// rules:
    ///
    /// - A name written with a schema or catalog is always a physical
    ///   relation. CTEs have no schema, so `public.c` never reads the CTE `c`.
    /// - A bare name is the CTE being defined if it reads itself, then the
    ///   nearest CTE visible at this position, and otherwise a physical
    ///   relation.
    /// - A bare name that matches a CTE the position cannot see (a later one,
    ///   or the CTE itself) is a physical relation where the dialect's rule is
    ///   known. Where it is not, or where the query is not valid, the name is
    ///   bound as both the CTE and the physical relation: the same range
    ///   variable twice, which makes any use of it ambiguous between them.
    /// - Only a physical relation is recorded as read; a CTE's own definition
    ///   records what it reads.
    ///
    /// The result is the bindings the FROM item's range variable takes.
    pub(crate) fn bind_relation_reference(&mut self, table: TableRef) -> Vec<Binding> {
        if table.catalog.is_some() || table.schema.is_some() {
            return vec![self.read_physical(table)];
        }
        if let Some(own) = &self.own_cte
            && own.name.column_key(self.dialect) == table.table.column_key(self.dialect)
        {
            match own.rule {
                OwnName::Recursive => return vec![Binding::Table(table)],
                OwnName::Unresolved => {
                    let scope = own.scope;
                    return vec![Binding::Cte(scope), self.read_physical(table)];
                }
                OwnName::Physical => {}
            }
        }
        let found = self.graph.scopes.lookup_cte(self.env(), &table.table);
        if let Some(scope) = found.visible
            && (found.hidden.is_none() || cte_list_rule(self.dialect, false).modelled)
        {
            return vec![Binding::Cte(scope)];
        }
        match found.hidden {
            Some(scope) if !cte_list_rule(self.dialect, false).modelled => {
                vec![Binding::Cte(scope), self.read_physical(table)]
            }
            _ => vec![self.read_physical(table)],
        }
    }

    fn read_physical(&mut self, table: TableRef) -> Binding {
        self.graph.tables.inputs.push(table.clone());
        Binding::Table(table)
    }
}
