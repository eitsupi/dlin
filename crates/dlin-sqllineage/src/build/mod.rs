pub(crate) mod expr;
pub(crate) mod function_semantics;
pub(crate) mod query;
pub(crate) mod select;
pub(crate) mod statement;

use crate::graph::RawGraph;
use crate::graph::scope::{Binding, ScopeId, ScopeKind, ScopeTree};
use crate::types::{Dialect, SemanticError, StatementType, Warning};
use sqlparser::ast::Statement;

pub(crate) struct LineageBuilder {
    pub(crate) graph: RawGraph,
    pub(crate) current_scope: ScopeId,
    pub(crate) recursive_cte_name: Option<crate::Ident>,
    pub(crate) warnings: Vec<Warning>,
    pub(crate) input_positions: Vec<(u64, u64)>,
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
            input_positions: Vec::new(),
            inner_statement_type: None,
            dialect,
            error: None,
        }
    }

    pub fn build(
        mut self,
        stmt: &Statement,
    ) -> Result<(RawGraph, Vec<Warning>, StatementType), SemanticError> {
        let st = self.visit_statement(stmt);
        if let Some(error) = self.error {
            return Err(error);
        }
        let mut inputs: Vec<_> = self
            .input_positions
            .into_iter()
            .zip(std::mem::take(&mut self.graph.tables.inputs))
            .collect();
        inputs.sort_by_key(|(position, _)| *position);
        self.graph.tables.inputs = inputs.into_iter().map(|(_, table)| table).collect();
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

    pub(crate) fn add_binding(&mut self, name: crate::Ident, binding: Binding) {
        self.graph
            .scopes
            .add_binding(self.current_scope, name, binding);
    }
}
