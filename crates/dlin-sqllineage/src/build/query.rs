use sqlparser::ast::{Query, SetExpr};

use crate::build::LineageBuilder;
use crate::graph::scope::{Binding, OutputPlan, ScopeKind};
use crate::types::SemanticError;

impl LineageBuilder {
    pub(crate) fn visit_query(&mut self, query: &Query) {
        self.visit_query_with_target(query, false);
    }

    /// Target-specific VALUES/TABLE handling follows only the query body and
    /// its set-operation branches; nested queries use ordinary analysis.
    pub(crate) fn visit_query_with_target(&mut self, query: &Query, target_values: bool) {
        // Pipe operators (`FROM t |> SELECT a`) reshape the result of the
        // query after its body. Ignoring them would report the lineage of the
        // unpiped body as if it were the query's output, so refuse instead.
        if !query.pipe_operators.is_empty() {
            self.reject(SemanticError::PipeOperators);
            // Continue building to retain structural diagnostics such as an
            // empty projection. The rejected graph will never be resolved.
        }

        let has_ctes = query.with.is_some();
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                let cte_scope = self.push_scope(ScopeKind::Cte);

                if with.recursive {
                    self.recursive_cte_name = Some(crate::Ident::from_ast(&cte.alias.name));
                }

                self.visit_query(&cte.query);

                self.recursive_cte_name = None;
                self.pop_scope();

                self.add_binding(
                    crate::Ident::from_ast(&cte.alias.name),
                    Binding::Cte(cte_scope),
                );
            }
        }

        // Push a child scope for the body when CTEs exist, so FROM bindings
        // don't mix with CTE registration bindings (avoids false ambiguity).
        if has_ctes {
            self.push_scope(ScopeKind::Root);
        }

        self.visit_set_expr(&query.body, target_values);

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

    fn visit_set_expr(&mut self, body: &SetExpr, target_values: bool) {
        match body {
            SetExpr::Select(select) => {
                self.visit_select(select);
            }
            SetExpr::SetOperation { left, right, .. } => {
                let left_scope = self.push_scope(ScopeKind::SetOperation);
                self.visit_set_expr(left, target_values);
                let left_outputs: Vec<(crate::Ident, crate::graph::node::NodeId)> = self
                    .graph
                    .scopes
                    .output_columns(left_scope)
                    .iter()
                    .map(|c| (c.name.clone(), c.node_id))
                    .collect();
                self.pop_scope();

                let right_scope = self.push_scope(ScopeKind::SetOperation);
                self.visit_set_expr(right, target_values);
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
                self.visit_query_with_target(q, target_values);
            }
            SetExpr::Values(values) if target_values => self.visit_target_values(values),
            SetExpr::Table(_) if target_values => self.graph.unknown_target_source = true,
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

    fn visit_target_values(&mut self, values: &sqlparser::ast::Values) {
        let Some(first) = values.rows.first() else {
            return;
        };
        if values.rows.iter().any(|row| row.len() != first.len()) {
            self.graph.unknown_target_source = true;
            return;
        }
        for index in 0..first.len() {
            let name = crate::Ident::unquoted(format!("column{}", index + 1));
            let intrinsic = values
                .rows
                .iter()
                .map(|row| crate::build::expr::determine_edge_kind(&row[index]))
                .max_by_key(|kind| match kind {
                    crate::graph::edge::EdgeKind::Direct => 0,
                    crate::graph::edge::EdgeKind::ViaExpression => 1,
                    crate::graph::edge::EdgeKind::ViaConditional => 2,
                    crate::graph::edge::EdgeKind::ViaAggregation => 3,
                })
                .unwrap_or(crate::graph::edge::EdgeKind::Direct);
            let output = self.graph.add_output(name.clone(), intrinsic.clone());
            self.graph
                .set_output_name(output, crate::OutputName::Anonymous);
            for row in &values.rows {
                let expr = &row[index];
                let kind = crate::build::expr::determine_edge_kind(expr);
                let ancestors = self.collect_ancestors(expr);
                let value = self.graph.add_output(name.clone(), kind.clone());
                for ancestor in ancestors {
                    self.graph.add_edge(ancestor, value, kind.clone());
                }
                self.graph.add_edge(value, output, intrinsic.clone());
            }
            self.graph.scopes.add_output_column(
                self.current_scope,
                crate::graph::scope::ScopeColumn {
                    name,
                    node_id: output,
                },
            );
        }
    }
}
