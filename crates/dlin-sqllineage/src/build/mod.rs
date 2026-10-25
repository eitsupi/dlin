pub(crate) mod expr;
pub(crate) mod function_semantics;
pub(crate) mod query;
pub(crate) mod select;
pub(crate) mod statement;

use crate::graph::RawGraph;
use crate::graph::scope::{Binding, ScopeId, ScopeKind, ScopeTree};
use crate::types::{Dialect, ParseError, StatementType, Warning};
use sqlparser::ast::Statement;

pub(crate) struct LineageBuilder {
    pub(crate) graph: RawGraph,
    pub(crate) current_scope: ScopeId,
    pub(crate) recursive_cte_name: Option<String>,
    pub(crate) warnings: Vec<Warning>,
    pub(crate) normalize_case: bool,
    pub(crate) inner_statement_type: Option<StatementType>,
    pub(crate) dialect: Dialect,
    /// First unsupported construct met while walking the statement. Once set,
    /// the walk stops adding to the graph and `build` reports the error.
    pub(crate) error: Option<ParseError>,
}

impl LineageBuilder {
    pub fn new(normalize_case: bool, dialect: Dialect) -> Self {
        let graph = RawGraph::new();
        let root = ScopeTree::root();
        Self {
            graph,
            current_scope: root,
            recursive_cte_name: None,
            warnings: Vec::new(),
            normalize_case,
            inner_statement_type: None,
            dialect,
            error: None,
        }
    }

    pub fn build(
        mut self,
        stmt: &Statement,
    ) -> Result<(RawGraph, Vec<Warning>, StatementType), ParseError> {
        let st = self.visit_statement(stmt);
        if let Some(error) = self.error {
            return Err(error);
        }
        let final_type = self.inner_statement_type.unwrap_or(st);
        Ok((self.graph, self.warnings, final_type))
    }

    /// Record an unsupported construct. Only the first one is kept.
    pub(crate) fn reject(&mut self, message: impl Into<String>) {
        if self.error.is_none() {
            self.error = Some(ParseError {
                message: message.into(),
            });
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

    pub(crate) fn add_binding(&mut self, name: String, binding: Binding) {
        self.graph
            .scopes
            .add_binding(self.current_scope, name, binding);
    }
}
