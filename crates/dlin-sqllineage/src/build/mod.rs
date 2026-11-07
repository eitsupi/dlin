pub(crate) mod expr;
pub(crate) mod function_semantics;
pub(crate) mod query;
pub(crate) mod relation;
pub(crate) mod select;
pub(crate) mod statement;

use crate::graph::RawGraph;
use crate::graph::scope::{Binding, BindingEnv, ScopeId, ScopeKind, ScopeTree};
use crate::types::{Dialect, SemanticError, StatementType, Warning};
use sqlparser::ast::Statement;

pub(crate) struct LineageBuilder {
    pub(crate) graph: RawGraph,
    pub(crate) current_scope: ScopeId,
    pub(crate) recursive_cte_name: Option<crate::Ident>,
    pub(crate) warnings: Vec<Warning>,
    pub(crate) inner_statement_type: Option<StatementType>,
    pub(crate) dialect: Dialect,
    /// First unsupported construct met while walking the statement. Once set,
    /// the walk stops adding to the graph and `build` reports the error.
    pub(crate) error: Option<SemanticError>,
}

impl LineageBuilder {
    pub fn new(dialect: Dialect) -> Self {
        let graph = RawGraph::new(dialect);
        let root = ScopeTree::root();
        Self {
            graph,
            current_scope: root,
            recursive_cte_name: None,
            warnings: Vec::new(),
            inner_statement_type: None,
            dialect,
            error: None,
        }
    }

    pub fn build(
        mut self,
        stmt: &Statement,
    ) -> Result<(RawGraph, Vec<Warning>, StatementType), BuildError> {
        self.graph.empty_projection = structural_diagnostics(stmt, self.dialect);
        let st = self.visit_statement(stmt);
        if let Some(error) = self.error {
            return Err(BuildError {
                error,
                empty_projection: self.graph.empty_projection.clone(),
            });
        }
        let final_type = self.inner_statement_type.unwrap_or(st);
        Ok((self.graph, self.warnings, final_type))
    }

    /// Record an unsupported construct. Only the first one is kept.
    pub(crate) fn reject(&mut self, error: SemanticError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    pub(crate) fn push_scope(&mut self, kind: ScopeKind) -> ScopeId {
        let new_scope = self.graph.scopes.push(self.current_scope, kind);
        self.current_scope = new_scope;
        new_scope
    }

    pub(crate) fn pop_scope(&mut self) {
        if let Some(parent) = self.graph.scopes.parent(self.current_scope) {
            self.current_scope = parent;
        }
    }

    /// The environment at the current position of the current scope.
    pub(crate) fn env(&self) -> BindingEnv {
        self.graph.scopes.env(self.current_scope)
    }

    pub(crate) fn add_binding(&mut self, name: crate::Ident, binding: Binding) {
        self.graph
            .scopes
            .add_binding(self.current_scope, name, binding);
    }
}

pub(crate) struct BuildError {
    pub error: SemanticError,
    pub empty_projection: Option<crate::Diagnostic>,
}

/// Collect structural diagnostics independently of the lineage traversal.
fn structural_diagnostics(stmt: &Statement, dialect: Dialect) -> Option<crate::Diagnostic> {
    use sqlparser::ast::{Query, Select, SelectFlavor, SetExpr, Visit, Visitor};
    use std::ops::ControlFlow;

    struct Diagnostics {
        dialect: Dialect,
        empty_projection: Option<crate::Diagnostic>,
    }

    impl Diagnostics {
        fn inspect_select(&mut self, select: &Select) {
            if !select.projection.is_empty() {
                return;
            }
            let severity = if select.flavor == SelectFlavor::FromFirstNoSelect
                || matches!(self.dialect, Dialect::Generic | Dialect::PostgreSql)
            {
                crate::Severity::Warning
            } else {
                crate::Severity::Error
            };
            if self.empty_projection.as_ref().is_none_or(|current| {
                current.severity == crate::Severity::Warning && severity == crate::Severity::Error
            }) {
                self.empty_projection = Some(crate::Diagnostic {
                    severity,
                    kind: crate::DiagnosticKind::EmptyProjection,
                    location: None,
                });
            }
        }

        fn inspect_body(&mut self, body: &SetExpr) {
            match body {
                SetExpr::Select(select) => self.inspect_select(select),
                SetExpr::SetOperation { left, right, .. } => {
                    self.inspect_body(left);
                    self.inspect_body(right);
                }
                _ => {}
            }
        }
    }

    impl Visitor for Diagnostics {
        type Break = ();

        fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<()> {
            self.inspect_body(&query.body);
            ControlFlow::Continue(())
        }
    }

    let mut diagnostics = Diagnostics {
        dialect,
        empty_projection: None,
    };
    let _ = stmt.visit(&mut diagnostics);
    diagnostics.empty_projection
}
