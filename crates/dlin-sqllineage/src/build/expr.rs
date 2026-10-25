use sqlparser::ast::{self, AccessExpr, Expr, FunctionArguments, Subscript, WindowType};

use crate::build::LineageBuilder;
use crate::build::function_semantics::{self, ArgumentSemantic};
use crate::graph::edge::EdgeKind;
use crate::graph::node::NodeId;
use crate::graph::scope::{Binding, ScopeKind};
use crate::types::TableRef;

impl LineageBuilder {
    pub(crate) fn collect_ancestors(&mut self, expr: &Expr) -> Vec<NodeId> {
        match expr {
            Expr::Identifier(ident) => {
                let binding = self
                    .graph
                    .scopes
                    .lookup(self.current_scope, &ident.value)
                    .cloned();
                if binding.as_ref().is_some_and(|binding| {
                    self.dialect.supports_relation_alias_row_value()
                        && matches!(
                            binding,
                            Binding::Table(_) | Binding::Cte(_) | Binding::DerivedTable(_)
                        )
                }) {
                    return vec![self.graph.add_row_value_candidate(
                        ident.value.clone(),
                        self.current_scope,
                        binding,
                    )];
                }
                let binding =
                    binding.filter(|binding| matches!(binding, Binding::VirtualSource(_)));
                let node = self.graph.add_unqualified_with_binding(
                    ident.value.clone(),
                    self.current_scope,
                    binding,
                );
                vec![node]
            }

            Expr::CompoundIdentifier(parts) => self.collect_compound_identifier_ancestors(parts),

            Expr::Value(_)
            | Expr::TypedString { .. }
            | Expr::Wildcard(..)
            | Expr::QualifiedWildcard(..) => vec![],

            Expr::Cast { expr, .. }
            | Expr::Nested(expr)
            | Expr::UnaryOp { expr, .. }
            | Expr::IsNull(expr)
            | Expr::IsNotNull(expr)
            | Expr::IsFalse(expr)
            | Expr::IsNotFalse(expr)
            | Expr::IsTrue(expr)
            | Expr::IsNotTrue(expr)
            | Expr::IsUnknown(expr)
            | Expr::IsNotUnknown(expr)
            | Expr::IsNormalized { expr, .. }
            | Expr::Collate { expr, .. }
            | Expr::Convert { expr, .. }
            | Expr::Ceil { expr, .. }
            | Expr::Floor { expr, .. }
            | Expr::Prefixed { value: expr, .. }
            | Expr::Prior(expr)
            | Expr::OuterJoin(expr)
            | Expr::Named { expr, .. } => self.collect_ancestors(expr),

            Expr::Extract { expr, .. } => self.collect_ancestors(expr),

            Expr::Trim {
                expr,
                trim_what,
                trim_characters,
                ..
            } => {
                let mut v = self.collect_ancestors(expr);
                if let Some(what) = trim_what {
                    v.extend(self.collect_ancestors(what));
                }
                if let Some(chars) = trim_characters {
                    for c in chars {
                        v.extend(self.collect_ancestors(c));
                    }
                }
                v
            }

            Expr::Substring {
                expr,
                substring_from,
                substring_for,
                ..
            } => {
                let mut v = self.collect_ancestors(expr);
                if let Some(from) = substring_from {
                    v.extend(self.collect_ancestors(from));
                }
                if let Some(for_expr) = substring_for {
                    v.extend(self.collect_ancestors(for_expr));
                }
                v
            }

            Expr::Overlay {
                expr,
                overlay_what,
                overlay_from,
                overlay_for,
                ..
            } => {
                let mut v = self.collect_ancestors(expr);
                v.extend(self.collect_ancestors(overlay_what));
                v.extend(self.collect_ancestors(overlay_from));
                if let Some(for_expr) = overlay_for {
                    v.extend(self.collect_ancestors(for_expr));
                }
                v
            }

            Expr::Position { expr, r#in } => {
                let mut v = self.collect_ancestors(expr);
                v.extend(self.collect_ancestors(r#in));
                v
            }

            Expr::AtTimeZone {
                timestamp,
                time_zone,
            } => {
                let mut v = self.collect_ancestors(timestamp);
                v.extend(self.collect_ancestors(time_zone));
                v
            }

            Expr::BinaryOp { left, right, .. }
            | Expr::Like {
                expr: left,
                pattern: right,
                ..
            }
            | Expr::ILike {
                expr: left,
                pattern: right,
                ..
            }
            | Expr::SimilarTo {
                expr: left,
                pattern: right,
                ..
            }
            | Expr::RLike {
                expr: left,
                pattern: right,
                ..
            }
            | Expr::IsDistinctFrom(left, right)
            | Expr::IsNotDistinctFrom(left, right) => {
                let mut v = self.collect_ancestors(left);
                v.extend(self.collect_ancestors(right));
                v
            }

            Expr::AnyOp { left, right, .. } | Expr::AllOp { left, right, .. } => {
                let mut v = self.collect_ancestors(left);
                v.extend(self.collect_ancestors(right));
                v
            }

            Expr::InUnnest {
                expr, array_expr, ..
            } => {
                let mut v = self.collect_ancestors(expr);
                v.extend(self.collect_ancestors(array_expr));
                v
            }

            Expr::MemberOf(m) => {
                let mut v = self.collect_ancestors(&m.value);
                v.extend(self.collect_ancestors(&m.array));
                v
            }

            Expr::Tuple(exprs) => {
                let mut v = Vec::new();
                for e in exprs {
                    v.extend(self.collect_ancestors(e));
                }
                v
            }

            Expr::Array(arr) => {
                let mut v = Vec::new();
                for e in &arr.elem {
                    v.extend(self.collect_ancestors(e));
                }
                v
            }

            Expr::Struct { values, .. } => {
                let mut v = Vec::new();
                for e in values {
                    v.extend(self.collect_ancestors(e));
                }
                v
            }

            Expr::Map(map) => {
                let mut v = Vec::new();
                for entry in &map.entries {
                    v.extend(self.collect_ancestors(&entry.key));
                    v.extend(self.collect_ancestors(&entry.value));
                }
                v
            }

            Expr::CompoundFieldAccess { root, access_chain } => {
                self.collect_compound_field_ancestors(root, access_chain)
            }
            Expr::JsonAccess { value, .. } => self.collect_ancestors(value),

            Expr::Function(func) => {
                let mut ancestors = Vec::new();
                if let FunctionArguments::List(list) = &func.args {
                    let semantics = function_semantics::classify_function(self.dialect, func)
                        .map(|signature| signature.arguments);
                    for (index, arg) in list.args.iter().enumerate() {
                        let arg_expr = match arg {
                            ast::FunctionArg::Unnamed(arg_expr)
                            | ast::FunctionArg::Named { arg: arg_expr, .. }
                            | ast::FunctionArg::ExprNamed { arg: arg_expr, .. } => arg_expr,
                        };
                        let ast::FunctionArgExpr::Expr(expr) = arg_expr else {
                            continue;
                        };
                        let is_static_syntax = semantics
                            .and_then(|semantics| semantics.get(index))
                            .is_some_and(|semantic| {
                                matches!(
                                    semantic,
                                    ArgumentSemantic::DatePart(grammar)
                                        if function_semantics::expression_is_static_date_part(
                                            expr, *grammar
                                        )
                                )
                            });
                        if !is_static_syntax {
                            ancestors.extend(self.collect_ancestors(expr));
                        }
                    }
                }
                if let FunctionArguments::Subquery(q) = &func.args {
                    let _sub = self.push_scope(ScopeKind::Subquery);
                    self.visit_query(q);
                    self.pop_scope();
                }
                if let Some(WindowType::WindowSpec(spec)) = &func.over {
                    for expr in &spec.partition_by {
                        ancestors.extend(self.collect_ancestors(expr));
                    }
                    for order in &spec.order_by {
                        ancestors.extend(self.collect_ancestors(&order.expr));
                    }
                }
                ancestors
            }

            Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                let mut v = Vec::new();
                if let Some(op) = operand {
                    v.extend(self.collect_ancestors(op));
                }
                for cw in conditions {
                    v.extend(self.collect_ancestors(&cw.condition));
                    v.extend(self.collect_ancestors(&cw.result));
                }
                if let Some(el) = else_result {
                    v.extend(self.collect_ancestors(el));
                }
                v
            }

            Expr::Subquery(query) => {
                let _sub = self.push_scope(ScopeKind::Subquery);
                self.visit_query(query);
                self.pop_scope();
                vec![]
            }

            Expr::InSubquery { expr, subquery, .. } => {
                let v = self.collect_ancestors(expr);
                let _sub = self.push_scope(ScopeKind::Subquery);
                self.visit_query(subquery);
                self.pop_scope();
                v
            }

            Expr::Exists { subquery, .. } => {
                let _sub = self.push_scope(ScopeKind::Subquery);
                self.visit_query(subquery);
                self.pop_scope();
                vec![]
            }

            Expr::Between {
                expr, low, high, ..
            } => {
                let mut v = self.collect_ancestors(expr);
                v.extend(self.collect_ancestors(low));
                v.extend(self.collect_ancestors(high));
                v
            }

            Expr::InList { expr, list, .. } => {
                let mut v = self.collect_ancestors(expr);
                for item in list {
                    v.extend(self.collect_ancestors(item));
                }
                v
            }

            Expr::GroupingSets(_)
            | Expr::Cube(_)
            | Expr::Rollup(_)
            | Expr::Dictionary(_)
            | Expr::Interval(_)
            | Expr::Lambda(_)
            | Expr::MatchAgainst { .. } => vec![],
        }
    }

    /// Collect the physical column at the root of a structured access chain.
    ///
    /// `base.items[0]` is ambiguous at the syntax level: `base` can be a
    /// visible relation binding, in which case `items` is its physical column,
    /// or it can be an unqualified top-level column (`payload.items[0]`). The
    /// scope binding, rather than rendered SQL text or dialect-specific names,
    /// is the structural distinction between those cases.
    fn collect_compound_field_ancestors(
        &mut self,
        root: &Expr,
        access_chain: &[AccessExpr],
    ) -> Vec<NodeId> {
        let mut ancestors = match (root, access_chain.first()) {
            (Expr::Identifier(binding_name), Some(AccessExpr::Dot(Expr::Identifier(field))))
                if self
                    .graph
                    .scopes
                    .lookup(self.current_scope, &binding_name.value)
                    .is_some() =>
            {
                let binding = self
                    .graph
                    .scopes
                    .lookup(self.current_scope, &binding_name.value)
                    .cloned();
                vec![self.add_bound_field_ancestor(
                    binding_name.value.clone(),
                    field.value.clone(),
                    binding,
                )]
            }
            (Expr::Identifier(column), Some(AccessExpr::Dot(Expr::Identifier(_)))) => {
                let binding = self
                    .graph
                    .scopes
                    .lookup(self.current_scope, &column.value)
                    .cloned();
                let binding =
                    binding.filter(|binding| matches!(binding, Binding::VirtualSource(_)));
                vec![self.graph.add_unqualified_with_binding(
                    column.value.clone(),
                    self.current_scope,
                    binding,
                )]
            }
            _ => self.collect_ancestors(root),
        };

        for access in access_chain {
            if let AccessExpr::Subscript(subscript) = access {
                ancestors.extend(self.collect_subscript_ancestors(subscript));
            }
        }
        ancestors
    }

    /// Resolve a plain dotted identifier by separating its relation binding
    /// from the top-level physical column.  The parser represents both
    /// `alias.column` and `alias.struct.field` as a flat compound identifier,
    /// so rendering all but the final component as one qualifier loses the
    /// distinction between a relation name and a nested field path.
    fn collect_compound_identifier_ancestors(
        &mut self,
        parts: &[sqlparser::ast::Ident],
    ) -> Vec<NodeId> {
        if let Some((prefix_len, binding)) = self.find_compound_binding(parts) {
            let qualifier = parts[..prefix_len]
                .iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let column = parts[prefix_len].value.clone();
            return vec![self.add_bound_field_ancestor(qualifier, column, Some(binding))];
        }

        // With no visible relation bindings, preserve the traditional
        // qualified-reference fallback.  This is used by callers that feed
        // already-qualified expressions without a FROM clause (for example
        // `orders.id`): the qualifier is still a physical relation name,
        // rather than an unqualified struct root.
        if self
            .graph
            .scopes
            .visible_bindings(self.current_scope)
            .is_empty()
            && parts.len() >= 2
        {
            let relation_parts = &parts[..parts.len() - 1];
            let qualifier = parts[..parts.len() - 1]
                .iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>()
                .join(".");
            let binding = match relation_parts {
                [table] => Some(Binding::Table(TableRef::new(table.value.clone()))),
                [schema, table] => Some(Binding::Table(TableRef::with_schema(
                    schema.value.clone(),
                    table.value.clone(),
                ))),
                [catalog, schema, table] => Some(Binding::Table(TableRef {
                    catalog: Some(catalog.value.clone()),
                    schema: Some(schema.value.clone()),
                    table: table.value.clone(),
                })),
                // Keep the legacy display-only fallback for an unsupported
                // number of relation components.  Standard SQL relation
                // names are at most catalog.schema.table, and this branch
                // avoids inventing a lossy structured interpretation beyond
                // that shape.
                _ => None,
            };
            return vec![self.graph.add_ref_with_binding(
                parts[parts.len() - 1].value.clone(),
                Some(qualifier),
                self.current_scope,
                binding,
            )];
        }

        // No relation prefix was found: `struct.field` is an unqualified
        // top-level column followed by a nested field path.  Only the
        // top-level column can be represented by the public ColumnOrigin API.
        let column = parts[0].value.clone();
        let binding = self
            .graph
            .scopes
            .lookup(self.current_scope, &column)
            .cloned()
            .filter(|binding| matches!(binding, Binding::VirtualSource(_)));
        vec![
            self.graph
                .add_unqualified_with_binding(column, self.current_scope, binding),
        ]
    }

    /// Find the longest visible relation prefix in a compound identifier.
    ///
    /// A one-component prefix is a SQL alias.  Longer prefixes are matched
    /// against the physical parts of a table binding, allowing references such
    /// as `catalog.schema.table.column` without turning the relation into a
    /// single quoted string containing dots.
    fn find_compound_binding(&self, parts: &[sqlparser::ast::Ident]) -> Option<(usize, Binding)> {
        let visible = self.graph.scopes.visible_bindings(self.current_scope);
        (1..parts.len()).rev().find_map(|prefix_len| {
            let prefix = parts[..prefix_len]
                .iter()
                .map(|part| part.value.as_str())
                .collect::<Vec<_>>();

            // Aliases are single identifiers and therefore only match the
            // first component of a compound identifier.
            if prefix_len == 1
                && let Some((_, binding)) = visible.iter().find(|(name, _)| name == prefix[0])
            {
                return Some((prefix_len, binding.clone()));
            }

            visible.iter().find_map(|(_, binding)| {
                let Binding::Table(table) = binding else {
                    return None;
                };
                (table_parts(table) == prefix).then(|| (prefix_len, binding.clone()))
            })
        })
    }

    fn add_bound_field_ancestor(
        &mut self,
        qualifier: String,
        column: String,
        binding: Option<Binding>,
    ) -> NodeId {
        self.graph
            .add_ref_with_binding(column, Some(qualifier), self.current_scope, binding)
    }

    fn collect_subscript_ancestors(&mut self, subscript: &Subscript) -> Vec<NodeId> {
        match subscript {
            Subscript::Index { index } => self.collect_ancestors(index),
            Subscript::Slice {
                lower_bound,
                upper_bound,
                stride,
            } => {
                let mut ancestors = Vec::new();
                if let Some(lower) = lower_bound {
                    ancestors.extend(self.collect_ancestors(lower));
                }
                if let Some(upper) = upper_bound {
                    ancestors.extend(self.collect_ancestors(upper));
                }
                if let Some(step) = stride {
                    ancestors.extend(self.collect_ancestors(step));
                }
                ancestors
            }
        }
    }
}

fn table_parts(table: &TableRef) -> Vec<&str> {
    let mut parts = Vec::with_capacity(3);
    if let Some(catalog) = &table.catalog {
        parts.push(catalog.as_str());
    }
    if let Some(schema) = &table.schema {
        parts.push(schema.as_str());
    }
    parts.push(table.table.as_str());
    parts
}

pub(crate) fn determine_edge_kind(expr: &Expr) -> EdgeKind {
    match expr {
        Expr::Identifier(_) | Expr::CompoundIdentifier(_) | Expr::Value(_) => EdgeKind::Direct,
        Expr::Function(f) => {
            if f.over.is_some() {
                EdgeKind::ViaExpression
            } else if is_aggregate(&f.name.to_string()) {
                EdgeKind::ViaAggregation
            } else {
                EdgeKind::ViaExpression
            }
        }
        Expr::Case { .. } => EdgeKind::ViaConditional,
        Expr::Cast { expr, .. } | Expr::Nested(expr) | Expr::Collate { expr, .. } => {
            determine_edge_kind(expr)
        }
        _ => EdgeKind::ViaExpression,
    }
}

fn is_aggregate(name: &str) -> bool {
    matches!(
        name.to_uppercase().as_str(),
        "SUM"
            | "COUNT"
            | "AVG"
            | "MIN"
            | "MAX"
            | "ARRAY_AGG"
            | "STRING_AGG"
            | "GROUP_CONCAT"
            | "LISTAGG"
            | "STDDEV"
            | "VARIANCE"
            | "MEDIAN"
    )
}
