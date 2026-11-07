//! Binding of the relation names a query reads.

use crate::build::LineageBuilder;
use crate::graph::scope::Binding;
use crate::types::TableRef;

impl LineageBuilder {
    /// Bind the relation named by a FROM item (or a `TABLE` query) and record
    /// that it is read. This is the only place that decides what a relation
    /// name refers to, so every syntax that names a relation follows the same
    /// rules:
    ///
    /// - A name written with a schema or catalog is always a physical
    ///   relation. CTEs have no schema, so `public.c` never reads the CTE `c`.
    /// - A bare name is the recursive CTE being defined if it matches that
    ///   CTE, then the nearest CTE of that name visible at this position, and
    ///   otherwise a physical relation.
    /// - Only a physical relation is recorded as read; a CTE's own definition
    ///   records what it reads.
    ///
    /// The result is the binding the FROM item's range variable takes.
    pub(crate) fn bind_relation_reference(&mut self, table: TableRef) -> Binding {
        if table.catalog.is_none() && table.schema.is_none() {
            let recursive_self = self.recursive_cte_name.as_ref().is_some_and(|name| {
                name.column_key(self.dialect) == table.table.column_key(self.dialect)
            });
            if recursive_self {
                return Binding::Table(table);
            }
            if let Some(cte) = self.graph.scopes.lookup_cte(self.env(), &table.table) {
                return Binding::Cte(cte);
            }
        }
        self.graph.tables.inputs.push(table.clone());
        Binding::Table(table)
    }
}
