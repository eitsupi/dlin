use sqlparser::ast::{OrderByKind, Query, SetExpr, SetOperator as SqlSetOperator, SetQuantifier};

use crate::build::{LineageBuilder, relation};
use crate::graph::scope::{OutputPlan, ScopeId, ScopeKind};
use crate::types::SemanticError;

impl LineageBuilder {
    pub(crate) fn visit_query(&mut self, query: &Query) {
        self.visit_query_with_target(query, false);
    }

    /// Visit a query that crosses a query boundary: a subquery in an
    /// expression, a derived table, an argument of a function. It gets a scope
    /// of its own, of the given kind, so the range variables and the outputs
    /// it introduces never reach the enclosing query. Every walk that enters
    /// such a query goes through here, whatever it collects, so that the
    /// walks cannot disagree about where a query's names end. Returns the
    /// scope, whose output columns are the query's.
    pub(crate) fn visit_subquery(&mut self, kind: ScopeKind, query: &Query) -> ScopeId {
        let scope = self.push_scope(kind);
        self.visit_query(query);
        self.pop_scope();
        scope
    }

    /// Register positional names declared by a CTE or derived table. The
    /// resolver applies them after catalog expansion, tying aliases to output
    /// positions rather than source names.
    pub(crate) fn set_scope_output_aliases(&mut self, scope: ScopeId, aliases: Vec<crate::Ident>) {
        if !aliases.is_empty() {
            self.graph.scopes.set_output_aliases(scope, aliases);
        }
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
            // Every CTE of the list is defined before any body is visited, so
            // that a RECURSIVE list can refer forward.
            let rule = relation::cte_list_rule(self.dialect, with.recursive);
            let names = with
                .cte_tables
                .iter()
                .map(|cte| crate::Ident::from_ast(&cte.alias.name))
                .collect();
            let scopes = self.graph.scopes.define_ctes(
                self.current_scope,
                names,
                rule.forward,
                rule.own_name == relation::OwnName::Recursive,
            );
            for (cte, cte_scope) in with.cte_tables.iter().zip(scopes) {
                self.current_scope = cte_scope;
                self.defining_ctes.push(relation::DefiningCte {
                    scope: cte_scope,
                    own_name: rule.own_name,
                });

                self.visit_query(&cte.query);

                if !cte.alias.columns.is_empty() {
                    self.set_scope_output_aliases(
                        cte_scope,
                        cte.alias
                            .columns
                            .iter()
                            .map(|column| crate::Ident::from_ast(&column.name))
                            .collect(),
                    );
                }

                self.defining_ctes.pop();
                self.pop_scope();
            }
        }

        // Push a child scope for the body when CTEs exist, so FROM bindings
        // don't mix with CTE registration bindings (avoids false ambiguity).
        if has_ctes {
            self.push_scope(ScopeKind::Body);
        }

        self.visit_set_expr(&query.body, target_values);
        // ORDER BY only orders the rows, but a subquery in it still reads
        // its relations. It sees the FROM clause of the body.
        if let Some(order_by) = &query.order_by
            && let OrderByKind::Expressions(exprs) = &order_by.kind
        {
            for order in exprs {
                self.scan_expr_for_tables(&order.expr);
            }
        }

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
            SetExpr::SetOperation {
                left,
                right,
                op,
                set_quantifier,
            } => {
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

                let references_before = self.self_references.len();
                let right_scope = self.push_scope(ScopeKind::SetOperation);
                self.visit_set_expr(right, target_values);
                self.pop_scope();

                // The operation is recursive when it is the body of a CTE and
                // its right term reads that CTE. A CTE that merely may read
                // itself is an ordinary one.
                let is_recursive = self
                    .cte_of_body_scope(self.current_scope)
                    .is_some_and(|cte| self.self_references[references_before..].contains(&cte));
                let correspondence = match set_quantifier {
                    SetQuantifier::ByName
                    | SetQuantifier::AllByName
                    | SetQuantifier::DistinctByName => {
                        crate::graph::scope::SetCorrespondence::ByName
                    }
                    SetQuantifier::All | SetQuantifier::Distinct | SetQuantifier::None => {
                        crate::graph::scope::SetCorrespondence::Positional
                    }
                };
                let operator = match op {
                    SqlSetOperator::Union => crate::graph::scope::SetOperator::Union,
                    SqlSetOperator::Intersect => crate::graph::scope::SetOperator::Intersect,
                    SqlSetOperator::Except | SqlSetOperator::Minus => {
                        crate::graph::scope::SetOperator::Except
                    }
                };
                let quantifier = match set_quantifier {
                    SetQuantifier::All | SetQuantifier::AllByName => {
                        crate::graph::scope::SetQuantifier::All
                    }
                    SetQuantifier::Distinct
                    | SetQuantifier::None
                    | SetQuantifier::ByName
                    | SetQuantifier::DistinctByName => crate::graph::scope::SetQuantifier::Distinct,
                };
                self.graph.scopes.set_output_plan(
                    self.current_scope,
                    OutputPlan::SetOperation {
                        left: left_scope,
                        right: right_scope,
                        recursive: is_recursive,
                        operator,
                        quantifier,
                        correspondence,
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
            SetExpr::Values(values) if target_values => {
                if self.check_values_rows(values) {
                    self.visit_target_values(values);
                }
            }
            // The columns of other VALUES lists are not modelled; their
            // expressions still read relations.
            SetExpr::Values(values) => {
                if self.check_values_rows(values) {
                    for expr in values.rows.iter().flat_map(|row| row.iter()) {
                        self.scan_expr_for_tables(expr);
                    }
                }
                self.graph
                    .scopes
                    .mark_unknown_output_width(self.current_scope, "VALUES source");
            }
            // `TABLE t` is `SELECT * FROM t`. The parser keeps the name only as
            // text, without its quoting, so it is read that way only where
            // quoting cannot change what the name refers to. Otherwise the
            // relation is recorded as read and the output is not modelled.
            SetExpr::Table(table) => {
                let Some(table_name) = &table.table_name else {
                    self.graph
                        .scopes
                        .mark_unknown_output_width(self.current_scope, "TABLE source");
                    return;
                };
                let modelled = table
                    .schema_name
                    .iter()
                    .chain([table_name])
                    .all(|part| quoting_is_irrelevant(part, self.dialect));
                let table_ref = crate::types::TableRef {
                    catalog: None,
                    schema: table.schema_name.as_deref().map(crate::Ident::unquoted),
                    table: crate::Ident::unquoted(table_name.as_str()),
                };
                let range = table_ref.table.clone();
                let bindings = self.bind_relation_reference(table_ref);
                if modelled {
                    for binding in bindings {
                        self.add_binding(range.clone(), binding);
                    }
                    self.visit_projection(&[sqlparser::ast::SelectItem::Wildcard(
                        sqlparser::ast::WildcardAdditionalOptions::default(),
                    )]);
                } else {
                    self.graph
                        .scopes
                        .mark_unknown_output_width(self.current_scope, "TABLE source");
                }
            }
            SetExpr::Update(stmt)
            | SetExpr::Insert(stmt)
            | SetExpr::Delete(stmt)
            | SetExpr::Merge(stmt) => {
                let st = self.visit_statement(stmt);
                self.inner_statement_type = Some(st);
            }
        }
    }

    /// Reject a VALUES list whose rows have different widths, which no
    /// dialect accepts. Returns whether the rows agree.
    fn check_values_rows(&mut self, values: &sqlparser::ast::Values) -> bool {
        let Some(first) = values.rows.first() else {
            return true;
        };
        match values.rows.iter().find(|row| row.len() != first.len()) {
            Some(row) => {
                self.reject(SemanticError::ValuesRowArityMismatch {
                    first: first.len(),
                    row: row.len(),
                });
                false
            }
            None => true,
        }
    }

    fn visit_target_values(&mut self, values: &sqlparser::ast::Values) {
        let Some(first) = values.rows.first() else {
            return;
        };
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

/// Whether the dialect compares a name part spelled `text` the same way
/// whether or not it is quoted, as a relation name and as a CTE name.
fn quoting_is_irrelevant(text: &str, dialect: crate::Dialect) -> bool {
    let unquoted = crate::Ident::unquoted(text);
    let quoted = crate::Ident::quoted(text, crate::Quote::Double);
    unquoted.column_key(dialect) == quoted.column_key(dialect)
        && unquoted.relation_key(dialect) == quoted.relation_key(dialect)
}
