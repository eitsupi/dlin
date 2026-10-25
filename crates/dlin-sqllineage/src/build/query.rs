use sqlparser::ast::{Query, SetExpr};

use crate::build::LineageBuilder;
use crate::graph::scope::{Binding, OutputPlan, ScopeKind};

impl LineageBuilder {
    pub(crate) fn visit_query(&mut self, query: &Query) {
        let has_ctes = query.with.is_some();
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let cte_scope = self.push_scope(ScopeKind::Cte);

                if with.recursive {
                    self.recursive_cte_name = Some(cte.alias.name.value.clone());
                }

                self.visit_query(&cte.query);

                self.recursive_cte_name = None;
                self.pop_scope();

                self.add_binding(cte.alias.name.value.clone(), Binding::Cte(cte_scope));
            }
        }

        // Push a child scope for the body when CTEs exist, so FROM bindings
        // don't mix with CTE registration bindings (avoids false ambiguity).
        if has_ctes {
            self.push_scope(ScopeKind::Root);
        }

        self.visit_set_expr(&query.body);

        if has_ctes {
            let body_outputs: Vec<_> = self
                .graph
                .scopes
                .output_columns(self.current_scope)
                .to_vec();
            let body_scope = self.current_scope;
            self.pop_scope();
            // The parent owns the query's public output. Keep the child plan
            // intact and delegate through it after returning to the parent.
            self.graph
                .scopes
                .set_output_plan(self.current_scope, OutputPlan::Delegate(body_scope));
            for col in body_outputs {
                self.graph.scopes.add_output_column(self.current_scope, col);
            }
        }
    }

    pub(crate) fn visit_set_expr(&mut self, body: &SetExpr) {
        match body {
            SetExpr::Select(select) => {
                self.visit_select(select);
            }
            SetExpr::SetOperation { left, right, .. } => {
                let left_scope = self.push_scope(ScopeKind::SetOperation);
                self.visit_set_expr(left);
                let left_outputs: Vec<(String, crate::graph::node::NodeId)> = self
                    .graph
                    .scopes
                    .output_columns(left_scope)
                    .iter()
                    .map(|c| (c.name.clone(), c.node_id))
                    .collect();
                self.pop_scope();

                let right_scope = self.push_scope(ScopeKind::SetOperation);
                self.visit_set_expr(right);
                self.pop_scope();

                let is_recursive = self.recursive_cte_name.is_some();
                self.graph.scopes.set_output_plan(
                    self.current_scope,
                    OutputPlan::SetOperation {
                        left: left_scope,
                        right: right_scope,
                        recursive: is_recursive,
                    },
                );

                for (name, node_id) in &left_outputs {
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        crate::graph::scope::ScopeColumn {
                            name: name.clone(),
                            node_id: *node_id,
                        },
                    );
                }
            }
            SetExpr::Query(q) => {
                self.visit_query(q);
            }
            SetExpr::Values(_) | SetExpr::Table(_) => {}
            SetExpr::Update(stmt)
            | SetExpr::Insert(stmt)
            | SetExpr::Delete(stmt)
            | SetExpr::Merge(stmt) => {
                let st = self.visit_statement(stmt);
                self.inner_statement_type = Some(st);
            }
        }
    }
}
