use sqlparser::ast::{
    Expr, Join, JoinConstraint, JoinOperator, ObjectName, Select, SelectFlavor, SelectItem,
    SelectItemQualifiedWildcardKind, TableFactor, TableWithJoins, WildcardAdditionalOptions,
};

use crate::Ident;
use crate::build::LineageBuilder;
use crate::build::expr::determine_edge_kind;
use crate::graph::node::{StarBase, StarColumnName, StarOptions, StarReplacement};
use crate::graph::scope::{
    Binding, JoinMerge, JoinMergeKind, MergedColumns, ScopeColumn, ScopeKind, VirtualColumn,
    VirtualColumnState,
};
use crate::types::SemanticError;

impl LineageBuilder {
    /// Process a SELECT — FROM first, then projection.
    pub(crate) fn visit_select(&mut self, select: &Select) {
        self.visit_from(&select.from);
        if select.flavor == SelectFlavor::FromFirstNoSelect && select.projection.is_empty() {
            // `FROM t` without a SELECT list is shorthand for `SELECT * FROM t`.
            self.visit_projection(&[SelectItem::Wildcard(WildcardAdditionalOptions::default())]);
        } else {
            self.visit_projection(&select.projection);
        }
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
                    let name = Ident::from_ast(alias);
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
                        let name = Ident::from_ast(alias);
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
                            name: Ident::unquoted("*"),
                            node_id: star,
                        },
                    );
                }
                SelectItem::QualifiedWildcard(kind, options) => {
                    let base = match kind {
                        SelectItemQualifiedWildcardKind::ObjectName(obj_name) => {
                            StarBase::Qualified(Self::object_name_parts(obj_name))
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
                            name: Ident::unquoted("*"),
                            node_id: star,
                        },
                    );
                }
            }
        }
    }

    fn object_name_parts(name: &ObjectName) -> Vec<Ident> {
        name.0
            .iter()
            .map(|part| {
                part.as_ident()
                    .map_or_else(|| Ident::unquoted(part.to_string()), Ident::from_ast)
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
                        parts: Self::object_name_parts(name),
                    }]
                }
                sqlparser::ast::ExcludeSelectItem::Multiple(names) => names
                    .iter()
                    .map(|name| StarColumnName {
                        parts: Self::object_name_parts(name),
                    })
                    .collect(),
            });
        }
        if let Some(except) = &options.opt_except {
            result.exclude.push(StarColumnName {
                parts: vec![Ident::from_ast(&except.first_element)],
            });
            result.exclude.extend(
                except
                    .additional_elements
                    .iter()
                    .map(|ident| StarColumnName {
                        parts: vec![Ident::from_ast(ident)],
                    }),
            );
        }
        if let Some(rename) = &options.opt_rename {
            let entries = match rename {
                sqlparser::ast::RenameSelectItem::Single(entry) => vec![entry],
                sqlparser::ast::RenameSelectItem::Multiple(entries) => entries.iter().collect(),
            };
            result.rename.extend(
                entries
                    .into_iter()
                    .map(|entry| (Ident::from_ast(&entry.ident), Ident::from_ast(&entry.alias))),
            );
        }
        if let Some(replace) = &options.opt_replace {
            for element in &replace.items {
                let node_id = self.graph.add_output(
                    Ident::unquoted("?wildcard-replace"),
                    determine_edge_kind(&element.expr),
                );
                for ancestor in self.collect_ancestors(&element.expr) {
                    self.graph
                        .add_edge(ancestor, node_id, determine_edge_kind(&element.expr));
                }
                result.replace.push(StarReplacement {
                    column: Ident::from_ast(&element.column_name),
                    node_id,
                });
            }
        }
        result
    }

    /// Process FROM clause items (including JOINs).
    pub(crate) fn visit_from(&mut self, from: &[TableWithJoins]) {
        for table_with_joins in from {
            self.visit_table_with_joins(table_with_joins);
        }
    }

    /// Visit a relation and the joins chained onto it. Each join combines
    /// everything to its left in the chain with its own relation, which is
    /// what a `USING` or `NATURAL` constraint merges columns across.
    fn visit_table_with_joins(&mut self, table_with_joins: &TableWithJoins) {
        let scope = self.current_scope;
        let start = self.graph.scopes.binding_mark(scope);
        self.visit_table_factor(&table_with_joins.relation);
        for join in &table_with_joins.joins {
            let left = self.graph.scopes.bindings_added_since(scope, start);
            let right_start = self.graph.scopes.binding_mark(scope);
            self.visit_table_factor(&join.relation);
            let right = self.graph.scopes.bindings_added_since(scope, right_start);
            self.note_join_constraint(join, left, right);
        }
    }

    /// Record the columns a join merges between the relations on its two
    /// sides, so an unqualified reference to one of them is not reported as
    /// ambiguous between those relations.
    fn note_join_constraint(&mut self, join: &Join, left: Vec<Ident>, right: Vec<Ident>) {
        let (constraint, kind) = match &join.join_operator {
            JoinOperator::Join(constraint)
            | JoinOperator::Inner(constraint)
            | JoinOperator::CrossJoin(constraint)
            | JoinOperator::StraightJoin(constraint) => (constraint, JoinMergeKind::Inner),
            JoinOperator::FullOuter(constraint) => (constraint, JoinMergeKind::Full),
            JoinOperator::Left(constraint)
            | JoinOperator::LeftOuter(constraint)
            | JoinOperator::Semi(constraint)
            | JoinOperator::LeftSemi(constraint)
            | JoinOperator::Anti(constraint)
            | JoinOperator::LeftAnti(constraint)
            | JoinOperator::AsOf { constraint, .. } => (constraint, JoinMergeKind::Left),
            JoinOperator::Right(constraint)
            | JoinOperator::RightOuter(constraint)
            | JoinOperator::RightSemi(constraint)
            | JoinOperator::RightAnti(constraint) => (constraint, JoinMergeKind::Right),
            JoinOperator::CrossApply
            | JoinOperator::OuterApply
            | JoinOperator::ArrayJoin
            | JoinOperator::LeftArrayJoin
            | JoinOperator::InnerArrayJoin => return,
        };
        let columns = match constraint {
            JoinConstraint::Using(columns) => {
                MergedColumns::Using(columns.iter().flat_map(Self::object_name_parts).collect())
            }
            JoinConstraint::Natural => MergedColumns::Natural,
            JoinConstraint::On(_) | JoinConstraint::None => return,
        };
        self.graph.scopes.add_join_merge(
            self.current_scope,
            JoinMerge {
                left,
                right,
                columns,
                kind,
            },
        );
    }

    /// Process a single table factor — register table as input and as scope binding.
    pub(crate) fn visit_table_factor(&mut self, factor: &TableFactor) {
        match factor {
            TableFactor::Table { name, alias, .. } => {
                let table_ref = Self::table_ref_from_object_name(name);

                let is_self_ref = self.recursive_cte_name.as_ref().is_some_and(|name| {
                    name.relation_key(self.dialect) == table_ref.table.relation_key(self.dialect)
                });

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
                    .map_or_else(|| table_ref.table.clone(), |a| Ident::from_ast(&a.name));

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
                    self.add_binding(
                        Ident::from_ast(&a.name),
                        Binding::DerivedTable(derived_scope),
                    );
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
                self.visit_table_with_joins(table_with_joins);
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
                        || Ident::from_ast(&alias.name),
                        |column| Ident::from_ast(&column.name),
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
                            .map_or_else(|| Ident::unquoted("offset"), Ident::from_ast),
                        dependencies: Vec::new(),
                        state: VirtualColumnState::KnownEmpty,
                    });
                } else if *with_ordinality {
                    columns.push(VirtualColumn {
                        name: Ident::unquoted("ordinality"),
                        dependencies: Vec::new(),
                        state: VirtualColumnState::KnownEmpty,
                    });
                }

                let virtual_id = self
                    .graph
                    .scopes
                    .add_virtual_source(self.current_scope, columns);
                self.add_binding(
                    Ident::from_ast(&alias.name),
                    Binding::VirtualSource(virtual_id),
                );
            }

            // The unpivoted value is derived from the expression, not from a
            // physical column of any relation in scope. Ignoring the factor
            // would make a reference to its alias resolve to a column that
            // does not exist, so refuse it.
            TableFactor::UnpivotExpr { .. } => {
                self.reject(SemanticError::UnpivotExpression);
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
fn infer_column_name(expr: &Expr) -> Ident {
    match expr {
        Expr::Identifier(ident) => Ident::from_ast(ident),
        Expr::CompoundIdentifier(parts) => parts
            .last()
            .map_or_else(|| Ident::unquoted(""), Ident::from_ast),
        Expr::Function(func) => Ident::unquoted(func.name.to_string()),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => infer_column_name(expr),
        _ => Ident::unquoted("?column?"),
    }
}
