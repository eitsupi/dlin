use sqlparser::ast::{
    Expr, ObjectName, Select, SelectItem, SelectItemQualifiedWildcardKind, TableFactor,
    TableWithJoins, WildcardAdditionalOptions,
};

use crate::build::LineageBuilder;
use crate::build::expr::determine_edge_kind;
use crate::graph::node::{StarBase, StarColumnName, StarOptions, StarReplacement};
use crate::graph::scope::{Binding, ScopeColumn, ScopeKind, VirtualColumn, VirtualColumnState};

impl LineageBuilder {
    /// Process a SELECT — FROM first, then projection.
    pub(crate) fn visit_select(&mut self, select: &Select) {
        self.visit_from(&select.from);
        self.visit_projection(&select.projection);
        if let Some(selection) = &select.selection {
            self.scan_expr_for_tables(selection);
        }
    }

    /// Process projection items — creates Output nodes and edges.
    fn visit_projection(&mut self, items: &[SelectItem]) {
        for item in items {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let ancestors = self.collect_ancestors(expr);
                    let kind = determine_edge_kind(expr);
                    let name = infer_column_name(expr);
                    let output = self.graph.add_output(name.clone(), kind.clone());
                    for &anc in &ancestors {
                        self.graph.add_edge(anc, output, kind.clone());
                    }
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        ScopeColumn {
                            name,
                            node_id: output,
                        },
                    );
                }
                SelectItem::ExprWithAlias { expr, alias } => {
                    let ancestors = self.collect_ancestors(expr);
                    let kind = determine_edge_kind(expr);
                    let name = alias.value.clone();
                    let output = self.graph.add_output(name.clone(), kind.clone());
                    for &anc in &ancestors {
                        self.graph.add_edge(anc, output, kind.clone());
                    }
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        ScopeColumn {
                            name,
                            node_id: output,
                        },
                    );
                }
                SelectItem::ExprWithAliases { expr, aliases } => {
                    let ancestors = self.collect_ancestors(expr);
                    let kind = determine_edge_kind(expr);
                    for alias in aliases {
                        let name = alias.value.clone();
                        let output = self.graph.add_output(name.clone(), kind.clone());
                        for &anc in &ancestors {
                            self.graph.add_edge(anc, output, kind.clone());
                        }
                        self.graph.scopes.add_output_column(
                            self.current_scope,
                            ScopeColumn {
                                name,
                                node_id: output,
                            },
                        );
                    }
                }
                SelectItem::Wildcard(options) => {
                    let star_options = self.star_options(options);
                    let star = self.graph.add_star(
                        StarBase::Unqualified,
                        star_options,
                        self.current_scope,
                    );
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        ScopeColumn {
                            name: "*".to_string(),
                            node_id: star,
                        },
                    );
                }
                SelectItem::QualifiedWildcard(kind, options) => {
                    let base = match kind {
                        SelectItemQualifiedWildcardKind::ObjectName(obj_name) => {
                            StarBase::Qualified(self.object_name_parts(obj_name))
                        }
                        SelectItemQualifiedWildcardKind::Expr(expr) => {
                            StarBase::Expr(self.collect_ancestors(expr))
                        }
                    };
                    let star_options = self.star_options(options);
                    let star = self.graph.add_star(base, star_options, self.current_scope);
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        ScopeColumn {
                            name: "*".to_string(),
                            node_id: star,
                        },
                    );
                }
            }
        }
    }

    fn object_name_parts(&self, name: &ObjectName) -> Vec<String> {
        name.0
            .iter()
            .map(|part| {
                part.as_ident()
                    .map_or_else(|| part.to_string(), |ident| self.normalize_ident(ident))
            })
            .collect()
    }

    fn star_options(&mut self, options: &WildcardAdditionalOptions) -> StarOptions {
        let mut result = StarOptions {
            ilike: options
                .opt_ilike
                .as_ref()
                .map(|ilike| ilike.pattern.clone()),
            ..StarOptions::default()
        };

        if let Some(exclude) = &options.opt_exclude {
            result.exclude.extend(match exclude {
                sqlparser::ast::ExcludeSelectItem::Single(name) => {
                    vec![StarColumnName {
                        parts: self.object_name_parts(name),
                    }]
                }
                sqlparser::ast::ExcludeSelectItem::Multiple(names) => names
                    .iter()
                    .map(|name| StarColumnName {
                        parts: self.object_name_parts(name),
                    })
                    .collect(),
            });
        }
        if let Some(except) = &options.opt_except {
            result.exclude.push(StarColumnName {
                parts: vec![self.normalize_ident(&except.first_element)],
            });
            result.exclude.extend(
                except
                    .additional_elements
                    .iter()
                    .map(|ident| StarColumnName {
                        parts: vec![self.normalize_ident(ident)],
                    }),
            );
        }
        if let Some(rename) = &options.opt_rename {
            let entries = match rename {
                sqlparser::ast::RenameSelectItem::Single(entry) => vec![entry],
                sqlparser::ast::RenameSelectItem::Multiple(entries) => entries.iter().collect(),
            };
            result.rename.extend(entries.into_iter().map(|entry| {
                (
                    self.normalize_ident(&entry.ident),
                    self.normalize_ident(&entry.alias),
                )
            }));
        }
        if let Some(replace) = &options.opt_replace {
            for element in &replace.items {
                let node_id = self.graph.add_output(
                    "?wildcard-replace".to_string(),
                    determine_edge_kind(&element.expr),
                );
                for ancestor in self.collect_ancestors(&element.expr) {
                    self.graph
                        .add_edge(ancestor, node_id, determine_edge_kind(&element.expr));
                }
                result.replace.push(StarReplacement {
                    column: self.normalize_ident(&element.column_name),
                    node_id,
                });
            }
        }
        result
    }

    /// Process FROM clause items (including JOINs).
    pub(crate) fn visit_from(&mut self, from: &[TableWithJoins]) {
        for table_with_joins in from {
            self.visit_table_factor(&table_with_joins.relation);
            for join in &table_with_joins.joins {
                self.visit_table_factor(&join.relation);
            }
        }
    }

    /// Process a single table factor — register table as input and as scope binding.
    pub(crate) fn visit_table_factor(&mut self, factor: &TableFactor) {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let table_ref = self.table_ref_from_object_name(name);

                let is_self_ref = self.recursive_cte_name.as_deref() == Some(&*table_ref.table);

                let existing = self
                    .graph
                    .scopes
                    .lookup(self.current_scope, &table_ref.table)
                    .cloned();
                let is_cte_ref =
                    matches!(&existing, Some(Binding::Cte(_) | Binding::DerivedTable(_)));

                if !is_self_ref && !is_cte_ref {
                    self.graph.tables.inputs.push(table_ref.clone());
                }

                let alias_name = alias
                    .as_ref()
                    .map_or_else(|| table_ref.table.clone(), |a| a.name.value.clone());

                if is_cte_ref {
                    // Always add CTE/DerivedTable binding to current scope
                    // (needed for correct unqualified column resolution)
                    self.add_binding(alias_name, existing.unwrap());
                } else {
                    self.add_binding(alias_name, Binding::Table(table_ref));
                }
            }

            TableFactor::Derived {
                subquery, alias, ..
            } => {
                let derived_scope = self.push_scope(ScopeKind::DerivedTable);
                self.visit_query(subquery);
                self.pop_scope();

                if let Some(a) = alias {
                    self.add_binding(a.name.value.clone(), Binding::DerivedTable(derived_scope));
                } else {
                    self.graph
                        .scopes
                        .add_anonymous_derived(self.current_scope, derived_scope);
                }
            }

            TableFactor::NestedJoin {
                table_with_joins,
                alias,
            } => {
                self.visit_table_factor(&table_with_joins.relation);
                for join in &table_with_joins.joins {
                    self.visit_table_factor(&join.relation);
                }
                let _ = alias;
            }

            TableFactor::UNNEST {
                alias,
                array_exprs,
                with_offset,
                with_offset_alias,
                with_ordinality,
            } => {
                // The array expressions are evaluated in the scope visible
                // before this FROM item is introduced. Capture their nodes
                // first, then install the range-variable binding so it is
                // visible to subsequent lateral FROM items and projection.
                let dependencies = array_exprs
                    .iter()
                    .map(|expr| self.collect_ancestors(expr))
                    .collect::<Vec<_>>();

                let Some(alias) = alias else {
                    return;
                };

                let mut columns = Vec::with_capacity(
                    array_exprs.len() + usize::from(*with_offset) + usize::from(*with_ordinality),
                );
                for (index, deps) in dependencies.into_iter().enumerate() {
                    let name = alias.columns.get(index).map_or_else(
                        || alias.name.value.clone(),
                        |column| column.name.value.clone(),
                    );
                    columns.push(VirtualColumn {
                        name,
                        state: if deps.is_empty() {
                            VirtualColumnState::KnownEmpty
                        } else {
                            VirtualColumnState::Unknown
                        },
                        dependencies: deps,
                    });
                }
                if *with_offset {
                    columns.push(VirtualColumn {
                        name: with_offset_alias
                            .as_ref()
                            .map_or_else(|| "offset".to_string(), |ident| ident.value.clone()),
                        dependencies: Vec::new(),
                        state: VirtualColumnState::KnownEmpty,
                    });
                } else if *with_ordinality {
                    columns.push(VirtualColumn {
                        name: "ordinality".to_string(),
                        dependencies: Vec::new(),
                        state: VirtualColumnState::KnownEmpty,
                    });
                }

                let virtual_id = self
                    .graph
                    .scopes
                    .add_virtual_source(self.current_scope, columns);
                self.add_binding(alias.name.value.clone(), Binding::VirtualSource(virtual_id));
            }

            TableFactor::TableFunction { .. }
            | TableFactor::Function { .. }
            | TableFactor::JsonTable { .. }
            | TableFactor::OpenJsonTable { .. }
            | TableFactor::Pivot { .. }
            | TableFactor::Unpivot { .. }
            | TableFactor::MatchRecognize { .. }
            | TableFactor::XmlTable { .. }
            | TableFactor::SemanticView { .. } => {}
        }
    }
}

/// Infer a column name from an expression.
fn infer_column_name(expr: &Expr) -> String {
    match expr {
        Expr::Identifier(ident) => ident.value.clone(),
        Expr::CompoundIdentifier(parts) => {
            parts.last().map(|p| p.value.clone()).unwrap_or_default()
        }
        Expr::Function(func) => func.name.to_string(),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => infer_column_name(expr),
        _ => "?column?".to_string(),
    }
}
