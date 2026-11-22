//! Window specifications: inline `OVER (...)`, named `OVER w` and windows
//! that refine a named one (`OVER (w ORDER BY ...)`).
//!
//! `PARTITION BY` and `ORDER BY` of a window decide the value of a window
//! function, so their columns are inputs of the result wherever the window is
//! written. The definitions of a `WINDOW` clause are walked once, where the
//! clause is, and a reference to one reuses the inputs collected there.

use sqlparser::ast::{NamedWindowDefinition, NamedWindowExpr, WindowSpec, WindowType};

use crate::Ident;
use crate::build::LineageBuilder;
use crate::graph::node::NodeId;

/// A definition of a `WINDOW` clause.
#[derive(Debug, Clone)]
pub(crate) struct NamedWindow {
    name: Ident,
    /// The window this one refines or renames (`w2 AS (w1 ORDER BY b)`,
    /// `w2 AS w1`).
    base: Option<Ident>,
    /// The columns of its own `PARTITION BY` and `ORDER BY`.
    inputs: Vec<NodeId>,
}

impl LineageBuilder {
    /// Walk the `WINDOW` clause of the SELECT being visited and make its
    /// definitions visible to the window functions of that SELECT only: a
    /// subquery has its own `WINDOW` clause and cannot use the outer one.
    pub(crate) fn define_named_windows(&mut self, definitions: &[NamedWindowDefinition]) {
        if definitions.is_empty() {
            return;
        }
        let windows = definitions
            .iter()
            .map(|NamedWindowDefinition(name, expr)| {
                let (base, inputs) = match expr {
                    NamedWindowExpr::NamedWindow(base) => (Some(base), Vec::new()),
                    NamedWindowExpr::WindowSpec(spec) => (
                        spec.window_name.as_ref(),
                        self.collect_window_spec_inputs(spec),
                    ),
                };
                NamedWindow {
                    name: Ident::from_ast(name),
                    base: base.map(Ident::from_ast),
                    inputs,
                }
            })
            .collect();
        self.named_windows.insert(self.current_scope, windows);
    }

    /// The inputs of a window function's `OVER` clause.
    pub(crate) fn collect_window_inputs(&mut self, over: &WindowType) -> Vec<NodeId> {
        let (mut inputs, mut name) = match over {
            WindowType::WindowSpec(spec) => (
                self.collect_window_spec_inputs(spec),
                spec.window_name.as_ref().map(Ident::from_ast),
            ),
            WindowType::NamedWindow(name) => (Vec::new(), Some(Ident::from_ast(name))),
        };
        let windows = self
            .named_windows
            .get(&self.current_scope)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut seen = Vec::new();
        while let Some(current) = name.take() {
            let key = current.column_key(self.dialect).into_owned();
            let mut matching = windows
                .iter()
                .filter(|window| window.name.column_key(self.dialect) == key);
            let window = matching.next().filter(|_| matching.next().is_none());
            let Some(window) = window.filter(|_| !seen.contains(&key)) else {
                // An undefined, ambiguous or circular window cannot be
                // resolved, so the inputs it would add are not known.
                inputs.push(self.graph.add_unsupported("named window"));
                return inputs;
            };
            seen.push(key);
            inputs.extend_from_slice(&window.inputs);
            name.clone_from(&window.base);
        }
        inputs
    }

    fn collect_window_spec_inputs(&mut self, spec: &WindowSpec) -> Vec<NodeId> {
        let mut inputs = Vec::new();
        for expr in &spec.partition_by {
            inputs.extend(self.collect_ancestors(expr));
        }
        for order in &spec.order_by {
            inputs.extend(self.collect_ancestors(&order.expr));
        }
        inputs
    }
}
