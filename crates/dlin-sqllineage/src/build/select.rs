use sqlparser::ast::{
    AccessExpr, Expr, Function, FunctionArg, FunctionArgExpr, FunctionArguments, GroupByExpr, Join,
    JoinConstraint, JoinOperator, ObjectName, Select, SelectFlavor, SelectItem,
    SelectItemQualifiedWildcardKind, Subscript, TableFactor, TableWithJoins,
    WildcardAdditionalOptions,
};

use std::ops::ControlFlow;

use crate::build::LineageBuilder;
use crate::build::expr::determine_edge_kind;
use crate::graph::StructValue;
use crate::graph::node::{StarBase, StarColumnName, StarOptions, StarReplacement};
use crate::graph::scope::{
    AnonymousRange, Binding, JoinMerge, JoinMergeKind, JoinOperand, MergedColumns, ScopeColumn,
    ScopeKind, VirtualColumn, VirtualColumnState,
};
use crate::types::SemanticError;
use crate::{Ident, OutputName};

impl LineageBuilder {
    /// Process a SELECT — FROM first, then projection.
    pub(crate) fn visit_select(&mut self, select: &Select) {
        if matches!(
            select.value_table_mode,
            Some(
                sqlparser::ast::ValueTableMode::AsValue
                    | sqlparser::ast::ValueTableMode::DistinctAsValue
            )
        ) {
            self.graph.as_value_scopes.insert(self.current_scope);
        }
        let input_mark = self.graph.tables.inputs.len();
        self.visit_from(&select.from);
        // Bind FROM first, but retain SQL clause order for read spellings:
        // projection subqueries appear before the FROM clause's relations.
        let from_inputs = self.graph.tables.inputs.split_off(input_mark);
        // The WINDOW clause is walked before the projection that uses it, but
        // the relations its subqueries read keep their place after HAVING.
        self.define_named_windows(&select.named_window);
        let window_inputs = self.graph.tables.inputs.split_off(input_mark);
        if select.flavor == SelectFlavor::FromFirstNoSelect && select.projection.is_empty() {
            // `FROM t` without a SELECT list is shorthand for `SELECT * FROM t`.
            self.visit_projection(&[SelectItem::Wildcard(WildcardAdditionalOptions::default())]);
        } else {
            self.visit_projection(&select.projection);
        }
        self.graph.tables.inputs.extend(from_inputs);
        // The clauses that only filter or group rows still read the
        // relations of their subqueries.
        if let Some(selection) = &select.selection {
            self.scan_expr_for_tables(selection);
        }
        if let GroupByExpr::Expressions(exprs, _) = &select.group_by {
            for expr in exprs {
                self.scan_expr_for_tables(expr);
            }
        }
        if let Some(having) = &select.having {
            self.scan_expr_for_tables(having);
        }
        self.graph.tables.inputs.extend(window_inputs);
        if let Some(qualify) = &select.qualify {
            self.scan_expr_for_tables(qualify);
        }
    }

    /// Process projection items — creates Output nodes and edges.
    pub(crate) fn visit_projection(&mut self, items: &[SelectItem]) {
        for item in items {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let (ancestors, fields) = self.projected_value(expr);
                    let kind = determine_edge_kind(expr);
                    let (name, output_name) = infer_output_name(expr);
                    let output = self.graph.add_output(name.clone(), kind.clone());
                    self.graph.set_output_name(output, output_name);
                    for &anc in &ancestors {
                        self.graph.add_edge(anc, output, kind.clone());
                    }
                    if let Some(value) = fields {
                        self.graph.struct_values.insert(output, value);
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
                    let (ancestors, fields) = self.projected_value(expr);
                    let kind = determine_edge_kind(expr);
                    let name = Ident::from_ast(alias);
                    let output = self.graph.add_output(name.clone(), kind.clone());
                    for &anc in &ancestors {
                        self.graph.add_edge(anc, output, kind.clone());
                    }
                    if let Some(value) = fields {
                        self.graph.struct_values.insert(output, value);
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
                    let star = self
                        .graph
                        .add_star(StarBase::Unqualified, star_options, self.env());
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        ScopeColumn {
                            name: Ident::unquoted("*"),
                            node_id: star,
                        },
                    );
                }
                SelectItem::QualifiedWildcard(kind, options) => {
                    let (base, shape) = match kind {
                        SelectItemQualifiedWildcardKind::ObjectName(obj_name) => {
                            (StarBase::Qualified(Self::object_name_parts(obj_name)), None)
                        }
                        SelectItemQualifiedWildcardKind::Expr(expr)
                            if self.dialect == crate::Dialect::PostgreSql
                                || aggregate_element(expr, self.dialect).is_some() =>
                        {
                            let (ancestors, shape) = self.projected_value(expr);
                            (StarBase::Expr(ancestors), shape)
                        }
                        SelectItemQualifiedWildcardKind::Expr(expr) => {
                            (StarBase::Expr(self.collect_ancestors(expr)), None)
                        }
                    };
                    let star_options = self.star_options(options);
                    let star = self.graph.add_star(base, star_options, self.env());
                    if let Some(shape) = shape {
                        self.graph.struct_values.insert(star, shape);
                    }
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

    /// The ancestors of a projected expression and what decides its fields
    /// if it is a STRUCT: a STRUCT it builds whose fields are all named and
    /// distinct, with a node for the value of each field, or the reference
    /// to a column it reads as it is. The fields read the same reference
    /// nodes as the whole value.
    fn projected_value(
        &mut self,
        expr: &Expr,
    ) -> (Vec<crate::graph::node::NodeId>, Option<StructValue>) {
        if let Some((aggregate, input, index)) = aggregate_element(expr, self.dialect) {
            let (mut ancestors, shape) = self.projected_value(input);
            self.scan_function_selection_for_tables(aggregate);
            let index = self.collect_ancestors(index);
            ancestors.extend(index.iter().copied());
            return (
                ancestors,
                shape.map(|input| StructValue::AggregateElement {
                    input: Box::new(input),
                    index,
                }),
            );
        }
        let Some(fields) = struct_value_fields(expr, self.graph.dialect) else {
            let ancestors = self.collect_ancestors(expr);
            let column = match ancestors.as_slice() {
                [reference] if self.reads_named_value(expr, *reference) => {
                    Some(StructValue::Column(*reference))
                }
                _ => None,
            };
            return (ancestors, column);
        };
        let mut ancestors = Vec::new();
        let mut nodes = Vec::with_capacity(fields.len());
        for (name, value) in fields {
            let (field_ancestors, shape) = self.projected_value(value);
            let kind = determine_edge_kind(value);
            let node = self.graph.add_output(name.clone(), kind.clone());
            for &ancestor in &field_ancestors {
                self.graph.add_edge(ancestor, node, kind.clone());
            }
            if let Some(shape) = shape {
                self.graph.struct_values.insert(node, shape);
            }
            ancestors.extend(field_ancestors);
            nodes.push((name, node));
        }
        (ancestors, Some(StructValue::Fields(nodes)))
    }

    /// Whether the reference stands for the entire value named by `expr`.
    fn reads_named_value(&self, expr: &Expr, reference: crate::graph::node::NodeId) -> bool {
        use crate::graph::node::RawNode;
        match (expr, &self.graph.nodes[reference]) {
            (Expr::Nested(inner), _) => self.reads_named_value(inner, reference),
            (Expr::CompoundIdentifier(_) | Expr::CompoundFieldAccess { .. }, _)
                if self.graph.field_paths.contains_key(&reference) =>
            {
                true
            }
            (
                Expr::Identifier(_),
                RawNode::Unqualified { .. }
                | RawNode::RowValueCandidate { .. }
                | RawNode::Ref {
                    qualifier: None, ..
                },
            ) => true,
            (
                Expr::CompoundIdentifier(parts),
                RawNode::Ref {
                    qualifier: Some(qualifier),
                    ..
                },
            ) => qualifier.len() + 1 == parts.len(),
            _ => false,
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
            // The right side of an APPLY is evaluated per row of the left
            // side, like a LATERAL item.
            let applied = matches!(
                join.join_operator,
                JoinOperator::CrossApply | JoinOperator::OuterApply
            );
            self.visit_table_factor_in(&join.relation, applied);
            let right = self.graph.scopes.bindings_added_since(scope, right_start);
            if let Some(JoinConstraint::On(condition)) = join_constraint(&join.join_operator) {
                self.scan_expr_for_tables(condition);
            }
            self.note_join_constraint(join, left, right);
        }
    }

    /// Record the columns a join merges between the relations on its two
    /// sides, so an unqualified reference to one of them is not reported as
    /// ambiguous between those relations.
    fn note_join_constraint(
        &mut self,
        join: &Join,
        left: Vec<JoinOperand>,
        right: Vec<JoinOperand>,
    ) {
        let Some(constraint) = join_constraint(&join.join_operator) else {
            return;
        };
        let kind = match &join.join_operator {
            JoinOperator::Join(_)
            | JoinOperator::Inner(_)
            | JoinOperator::CrossJoin(_)
            | JoinOperator::StraightJoin(_) => JoinMergeKind::Inner,
            JoinOperator::FullOuter(_) => JoinMergeKind::Full,
            JoinOperator::Left(_)
            | JoinOperator::LeftOuter(_)
            | JoinOperator::Semi(_)
            | JoinOperator::LeftSemi(_)
            | JoinOperator::Anti(_)
            | JoinOperator::LeftAnti(_)
            | JoinOperator::AsOf { .. } => JoinMergeKind::Left,
            JoinOperator::Right(_)
            | JoinOperator::RightOuter(_)
            | JoinOperator::RightSemi(_)
            | JoinOperator::RightAnti(_) => JoinMergeKind::Right,
            JoinOperator::CrossApply
            | JoinOperator::OuterApply
            | JoinOperator::ArrayJoin
            | JoinOperator::LeftArrayJoin
            | JoinOperator::InnerArrayJoin => return,
        };
        let columns = match constraint {
            // `USING (...) AS alias` merges the listed columns as `USING`
            // does. The alias is not bound, so a column qualified by it is
            // left unresolved.
            JoinConstraint::Using(columns) | JoinConstraint::UsingAlias(columns, _) => {
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
                at: 0,
            },
        );
    }

    /// Process a single table factor — register table as input and as scope binding.
    pub(crate) fn visit_table_factor(&mut self, factor: &TableFactor) {
        self.visit_table_factor_in(factor, false);
    }

    /// `applied` is set for the right side of an `APPLY` join.
    fn visit_table_factor_in(&mut self, factor: &TableFactor, applied: bool) {
        match factor {
            TableFactor::Table {
                name,
                alias,
                args,
                with_ordinality,
                ..
            } => {
                let unnest = self.dialect == crate::Dialect::DuckDb
                    && is_unnest_function(name, self.dialect);
                if let Some(args) = args
                    && !unnest
                {
                    // A name with arguments is a table function, not a
                    // relation: it reads nothing by its name.
                    self.bind_unmodelled_factor(
                        "table function",
                        alias.as_ref(),
                        last_name_part(name),
                        ScopeKind::LateralDerivedTable,
                        |builder| builder.scan_outermost_exprs(&args.args),
                    );
                    return;
                }
                if unnest && let Some(args) = args {
                    let dependencies = args
                        .args
                        .iter()
                        .map(|arg| match arg {
                            sqlparser::ast::FunctionArg::Unnamed(
                                sqlparser::ast::FunctionArgExpr::Expr(expr),
                            )
                            | sqlparser::ast::FunctionArg::Named {
                                arg: sqlparser::ast::FunctionArgExpr::Expr(expr),
                                ..
                            }
                            | sqlparser::ast::FunctionArg::ExprNamed {
                                arg: sqlparser::ast::FunctionArgExpr::Expr(expr),
                                ..
                            } => self.collect_ancestors(expr),
                            _ => vec![self.graph.add_unsupported("UNNEST argument")],
                        })
                        .collect::<Vec<_>>();
                    let default_alias = alias.is_none().then(|| Ident::unquoted("unnest"));
                    self.add_virtual_unnest(
                        alias.as_ref(),
                        default_alias.as_ref(),
                        dependencies,
                        false,
                        None,
                        *with_ordinality,
                    );
                    return;
                }
                // A column alias list renames the relation's columns by
                // position, exactly as it does for `(SELECT * FROM t) AS
                // d(p)`. Read the relation that way, so the renaming follows
                // the one rule for CTE and derived-table aliases.
                if let Some(alias) = alias
                    && !alias.columns.is_empty()
                {
                    let mut inner = factor.clone();
                    if let TableFactor::Table {
                        alias: Some(inner_alias),
                        ..
                    } = &mut inner
                    {
                        inner_alias.columns.clear();
                    }
                    self.bind_star_scope(ScopeKind::DerivedTable, alias, |builder| {
                        builder.visit_table_factor(&inner);
                    });
                    return;
                }
                let table_ref = self.table_ref_from_object_name(name);
                let bindings = self.bind_relation_reference(table_ref.clone());
                let alias_name = alias
                    .as_ref()
                    .map_or_else(|| table_ref.table.clone(), |a| Ident::from_ast(&a.name));
                for binding in bindings {
                    self.add_binding(alias_name.clone(), binding);
                }
            }

            TableFactor::Derived {
                lateral,
                subquery,
                alias,
                ..
            } => {
                let kind = if *lateral || applied {
                    ScopeKind::LateralDerivedTable
                } else {
                    ScopeKind::DerivedTable
                };
                let derived_scope = self.visit_subquery(kind, subquery);

                if let Some(a) = alias {
                    self.set_scope_output_aliases(
                        derived_scope,
                        a.columns
                            .iter()
                            .map(|column| Ident::from_ast(&column.name))
                            .collect(),
                    );
                    self.add_binding(
                        Ident::from_ast(&a.name),
                        Binding::DerivedTable(derived_scope),
                    );
                } else {
                    self.graph
                        .scopes
                        .add_anonymous(self.current_scope, AnonymousRange::Derived(derived_scope));
                }
            }

            TableFactor::NestedJoin {
                table_with_joins,
                alias,
            } => match alias {
                // An alias makes the parenthesized join one range variable
                // that hides the relations inside it, as `(SELECT * FROM t
                // JOIN u) AS j` would.
                Some(alias) => {
                    self.bind_star_scope(ScopeKind::JoinGroup, alias, |builder| {
                        builder.visit_table_with_joins(table_with_joins);
                    });
                }
                None => self.visit_table_with_joins(table_with_joins),
            },

            TableFactor::UNNEST {
                alias,
                array_exprs,
                with_offset,
                with_offset_alias,
                with_ordinality,
            } => {
                if alias.is_none()
                    && self.dialect == crate::Dialect::BigQuery
                    && let [array] = array_exprs.as_slice()
                {
                    self.add_anonymous_unnest(array, *with_offset, with_offset_alias.as_ref());
                    return;
                }
                let dependencies = array_exprs
                    .iter()
                    .map(|expr| self.collect_ancestors(expr))
                    .collect();
                self.add_virtual_unnest(
                    alias.as_ref(),
                    None,
                    dependencies,
                    *with_offset,
                    with_offset_alias.as_ref(),
                    *with_ordinality,
                );
            }

            // The unpivoted value is derived from the expression, not from a
            // physical column of any relation in scope. Ignoring the factor
            // would make a reference to its alias resolve to a column that
            // does not exist, so refuse it.
            TableFactor::UnpivotExpr { .. } => {
                self.reject(SemanticError::UnpivotExpression);
            }

            // The output of these is not modelled. Each still binds its range
            // variable, so its columns are not mistaken for those of another
            // relation, and the relations its arguments read are recorded.
            // The arguments are not taken as the value of any output column.
            TableFactor::TableFunction { expr, alias } => {
                self.bind_unmodelled_factor(
                    "table function",
                    alias.as_ref(),
                    None,
                    ScopeKind::LateralDerivedTable,
                    |builder| builder.scan_outermost_exprs(expr),
                );
            }
            TableFactor::Function {
                name, args, alias, ..
            } => {
                self.bind_unmodelled_factor(
                    "table function",
                    alias.as_ref(),
                    last_name_part(name),
                    ScopeKind::LateralDerivedTable,
                    |builder| builder.scan_outermost_exprs(args),
                );
            }
            TableFactor::JsonTable {
                json_expr,
                columns,
                alias,
                ..
            } => {
                self.bind_unmodelled_factor(
                    "JSON_TABLE",
                    alias.as_ref(),
                    None,
                    ScopeKind::LateralDerivedTable,
                    |builder| {
                        builder.scan_outermost_exprs(json_expr);
                        builder.scan_outermost_exprs(columns);
                    },
                );
            }
            TableFactor::OpenJsonTable {
                json_expr,
                columns,
                alias,
                ..
            } => {
                self.bind_unmodelled_factor(
                    "OPENJSON",
                    alias.as_ref(),
                    None,
                    ScopeKind::LateralDerivedTable,
                    |builder| {
                        builder.scan_outermost_exprs(json_expr);
                        builder.scan_outermost_exprs(columns);
                    },
                );
            }
            TableFactor::XmlTable {
                namespaces,
                row_expression,
                passing,
                columns,
                alias,
            } => {
                self.bind_unmodelled_factor(
                    "XMLTABLE",
                    alias.as_ref(),
                    None,
                    ScopeKind::LateralDerivedTable,
                    |builder| {
                        builder.scan_outermost_exprs(namespaces);
                        builder.scan_outermost_exprs(row_expression);
                        builder.scan_outermost_exprs(passing);
                        builder.scan_outermost_exprs(columns);
                    },
                );
            }
            // The semantic view is a relation the query reads. The dimensions,
            // metrics and facts name its members, not columns of the query.
            TableFactor::SemanticView {
                name,
                where_clause,
                alias,
                ..
            } => {
                let table_ref = self.table_ref_from_object_name(name);
                let default_name = table_ref.table.clone();
                self.bind_unmodelled_factor(
                    "semantic view",
                    alias.as_ref(),
                    Some(default_name),
                    ScopeKind::DerivedTable,
                    |builder| {
                        builder.graph.tables.inputs.push(table_ref);
                        builder.scan_outermost_exprs(where_clause);
                    },
                );
            }
            // These reshape the rows of the FROM item they wrap. The wrapped
            // item is visited in a scope of its own, so its range variable is
            // hidden behind the reshaped output, as it is in SQL.
            TableFactor::Pivot {
                table,
                aggregate_functions,
                value_column,
                value_source,
                default_on_null,
                alias,
            } => {
                self.bind_unmodelled_factor(
                    "PIVOT",
                    alias.as_ref(),
                    None,
                    ScopeKind::JoinGroup,
                    |builder| {
                        builder.visit_table_factor_in(table, applied);
                        builder.scan_outermost_exprs(aggregate_functions);
                        builder.scan_outermost_exprs(value_column);
                        builder.scan_outermost_exprs(value_source);
                        builder.scan_outermost_exprs(default_on_null);
                    },
                );
            }
            TableFactor::Unpivot {
                table,
                value,
                columns,
                alias,
                ..
            } => {
                self.bind_unmodelled_factor(
                    "UNPIVOT",
                    alias.as_ref(),
                    None,
                    ScopeKind::JoinGroup,
                    |builder| {
                        builder.visit_table_factor_in(table, applied);
                        builder.scan_outermost_exprs(value);
                        builder.scan_outermost_exprs(columns);
                    },
                );
            }
            TableFactor::MatchRecognize {
                table,
                partition_by,
                order_by,
                measures,
                symbols,
                alias,
                ..
            } => {
                self.bind_unmodelled_factor(
                    "MATCH_RECOGNIZE",
                    alias.as_ref(),
                    None,
                    ScopeKind::JoinGroup,
                    |builder| {
                        builder.visit_table_factor_in(table, applied);
                        builder.scan_outermost_exprs(partition_by);
                        builder.scan_outermost_exprs(order_by);
                        builder.scan_outermost_exprs(measures);
                        builder.scan_outermost_exprs(symbols);
                    },
                );
            }
        }
    }

    /// Bind a FROM item whose output columns the analysis does not model. It
    /// gets a scope of its own, of the given kind, in which `visit_inputs`
    /// records what the item reads. The scope publishes an output of unknown
    /// width for `what`, which may carry any name. The item is bound under its
    /// alias, or else under `default_name`, or else as an anonymous relation
    /// that still takes part in unqualified lookups and stars.
    fn bind_unmodelled_factor(
        &mut self,
        what: &'static str,
        alias: Option<&sqlparser::ast::TableAlias>,
        default_name: Option<Ident>,
        kind: ScopeKind,
        visit_inputs: impl FnOnce(&mut Self),
    ) {
        let scope = self.push_scope(kind);
        visit_inputs(self);
        self.graph.scopes.mark_unknown_output_width(scope, what);
        self.pop_scope();
        if let Some(alias) = alias {
            self.set_scope_output_aliases(
                scope,
                alias
                    .columns
                    .iter()
                    .map(|column| Ident::from_ast(&column.name))
                    .collect(),
            );
        }
        match alias
            .map(|alias| Ident::from_ast(&alias.name))
            .or(default_name)
        {
            Some(name) => self.add_binding(name, Binding::DerivedTable(scope)),
            None => self
                .graph
                .scopes
                .add_anonymous(self.current_scope, AnonymousRange::Derived(scope)),
        }
    }

    /// Record the relations read by the outermost expressions and queries
    /// within `node`, as for a filter. Nested expressions are covered by the
    /// walk of the expression that contains them.
    fn scan_outermost_exprs<V: sqlparser::ast::Visit>(&mut self, node: &V) {
        struct Outermost<'b> {
            builder: &'b mut LineageBuilder,
            depth: usize,
        }
        impl sqlparser::ast::Visitor for Outermost<'_> {
            type Break = ();
            fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> ControlFlow<()> {
                if self.depth == 0 {
                    self.builder.visit_subquery(ScopeKind::Subquery, query);
                }
                self.depth += 1;
                ControlFlow::Continue(())
            }
            fn post_visit_query(&mut self, _: &sqlparser::ast::Query) -> ControlFlow<()> {
                self.depth -= 1;
                ControlFlow::Continue(())
            }
            fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<()> {
                if self.depth == 0 {
                    self.builder.scan_expr_for_tables(expr);
                }
                self.depth += 1;
                ControlFlow::Continue(())
            }
            fn post_visit_expr(&mut self, _: &Expr) -> ControlFlow<()> {
                self.depth -= 1;
                ControlFlow::Continue(())
            }
        }
        let _ = node.visit(&mut Outermost {
            builder: self,
            depth: 0,
        });
    }

    /// Bind `alias` to a scope that publishes `SELECT *` over the FROM items
    /// `visit_from` introduces, with the alias's column list applied to its
    /// output like a derived table's.
    fn bind_star_scope(
        &mut self,
        kind: ScopeKind,
        alias: &sqlparser::ast::TableAlias,
        visit_from: impl FnOnce(&mut Self),
    ) {
        let scope = self.push_scope(kind);
        visit_from(self);
        self.visit_projection(&[SelectItem::Wildcard(WildcardAdditionalOptions::default())]);
        self.pop_scope();
        self.set_scope_output_aliases(
            scope,
            alias
                .columns
                .iter()
                .map(|column| Ident::from_ast(&column.name))
                .collect(),
        );
        self.add_binding(Ident::from_ast(&alias.name), Binding::DerivedTable(scope));
    }

    fn add_virtual_unnest(
        &mut self,
        alias: Option<&sqlparser::ast::TableAlias>,
        default_alias: Option<&Ident>,
        dependencies: Vec<Vec<crate::graph::node::NodeId>>,
        with_offset: bool,
        with_offset_alias: Option<&sqlparser::ast::Ident>,
        with_ordinality: bool,
    ) {
        // The array expressions are evaluated before this range variable is
        // introduced. Capture their value dependencies, then publish virtual
        // output columns for later FROM items and the projection.
        let Some(alias_name) = alias
            .map(|alias| Ident::from_ast(&alias.name))
            .or_else(|| default_alias.cloned())
        else {
            return;
        };
        let array_count = dependencies.len();
        let output_count = array_count + usize::from(with_offset || with_ordinality);
        // PostgreSQL rejects an UNNEST column alias list wider than the
        // function's known output. Generic SQL has no dialect-specific rule
        // to rely on, so keep those slots unresolved there as well. DuckDB
        // deliberately ignores surplus aliases.
        let surplus_aliases = alias.is_some_and(|alias| alias.columns.len() > output_count)
            && matches!(
                self.dialect,
                crate::Dialect::PostgreSql | crate::Dialect::Generic
            );
        let surplus_alias_dependency =
            surplus_aliases.then(|| self.graph.add_unsupported("UNNEST alias count mismatch"));
        let mut columns = Vec::with_capacity(output_count);
        for (index, mut deps) in dependencies.into_iter().enumerate() {
            let name = alias
                .and_then(|alias| alias.columns.get(index))
                .map_or_else(
                    || alias_name.clone(),
                    |column| Ident::from_ast(&column.name),
                );
            if let Some(unsupported) = surplus_alias_dependency {
                deps.push(unsupported);
            }
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
        if with_offset {
            let mut dependencies = Vec::new();
            if let Some(unsupported) = surplus_alias_dependency {
                dependencies.push(unsupported);
            }
            columns.push(VirtualColumn {
                name: with_offset_alias
                    .map(Ident::from_ast)
                    .or_else(|| {
                        alias
                            .and_then(|alias| alias.columns.get(array_count))
                            .map(|column| Ident::from_ast(&column.name))
                    })
                    .unwrap_or_else(|| Ident::unquoted("offset")),
                state: if dependencies.is_empty() {
                    VirtualColumnState::KnownEmpty
                } else {
                    VirtualColumnState::Unknown
                },
                dependencies,
            });
        } else if with_ordinality {
            let mut dependencies = Vec::new();
            if let Some(unsupported) = surplus_alias_dependency {
                dependencies.push(unsupported);
            }
            columns.push(VirtualColumn {
                name: alias
                    .and_then(|alias| alias.columns.get(array_count))
                    .map_or_else(
                        || Ident::unquoted("ordinality"),
                        |column| Ident::from_ast(&column.name),
                    ),
                state: if dependencies.is_empty() {
                    VirtualColumnState::KnownEmpty
                } else {
                    VirtualColumnState::Unknown
                },
                dependencies,
            });
        }

        let virtual_id = self
            .graph
            .scopes
            .add_virtual_source(self.current_scope, columns, None);
        self.add_binding(alias_name, Binding::VirtualSource(virtual_id));
    }

    /// A `BigQuery` `UNNEST` without a range variable. The element of an
    /// array of STRUCTs publishes its fields as columns of the FROM clause,
    /// each taking its value from the array, and the element of any other
    /// array is one column without a name; no name qualifies a reference to
    /// them. `WITH OFFSET` adds a column named by its alias, `offset` by
    /// default.
    fn add_anonymous_unnest(
        &mut self,
        array: &Expr,
        with_offset: bool,
        with_offset_alias: Option<&sqlparser::ast::Ident>,
    ) {
        let (mut columns, open_element) = match element_shape(array, self.dialect) {
            ElementShape::Scalar => {
                // A scalar element is one column without a name: a star
                // reads it, but no name refers to it.
                let dependencies = self.collect_ancestors(array);
                let element = VirtualColumn {
                    name: Ident::unquoted(""),
                    state: if dependencies.is_empty() {
                        VirtualColumnState::KnownEmpty
                    } else {
                        VirtualColumnState::Unknown
                    },
                    dependencies,
                };
                (vec![element], None)
            }
            ElementShape::Fields(fields) => {
                // A field of STRUCT literals takes its value from its own
                // expressions; a field of a cast array from the whole array.
                let whole = fields
                    .iter()
                    .any(|(_, values)| values.is_none())
                    .then(|| self.collect_ancestors(array));
                let columns = fields
                    .into_iter()
                    .map(|(name, values)| {
                        let dependencies: Vec<_> = match values {
                            Some(values) => values
                                .into_iter()
                                .flat_map(|value| self.collect_ancestors(value))
                                .collect(),
                            None => whole.clone().unwrap_or_default(),
                        };
                        VirtualColumn {
                            name,
                            state: if dependencies.is_empty() {
                                VirtualColumnState::KnownEmpty
                            } else {
                                VirtualColumnState::Unknown
                            },
                            dependencies,
                        }
                    })
                    .collect();
                (columns, None)
            }
            ElementShape::Unknown => (Vec::new(), Some(self.collect_ancestors(array))),
        };
        columns.extend(with_offset.then(|| VirtualColumn {
            name: with_offset_alias.map_or_else(|| Ident::unquoted("offset"), Ident::from_ast),
            state: VirtualColumnState::KnownEmpty,
            dependencies: Vec::new(),
        }));
        let source =
            self.graph
                .scopes
                .add_virtual_source(self.current_scope, columns, open_element);
        self.graph
            .scopes
            .add_anonymous(self.current_scope, AnonymousRange::Virtual(source));
    }
}

/// What the element of a `BigQuery` array is known to be from the array
/// expression alone.
enum ElementShape<'a> {
    /// Not a STRUCT, so it publishes no field names.
    Scalar,
    /// A STRUCT with these fields, each with the expressions that supply its
    /// value, or `None` when only the whole array is known to.
    Fields(Vec<(Ident, Option<Vec<&'a Expr>>)>),
    Unknown,
}

/// The shape of the element of `expr`, read from an explicit type or a
/// literal: a cast to an array type, an array of literal values, an array of
/// STRUCT literals whose fields are named alike, or a function that
/// generates a series. Anything else, an empty literal and STRUCTs with
/// unnamed or repeated fields included, is unknown.
fn element_shape(expr: &Expr, dialect: crate::Dialect) -> ElementShape<'_> {
    match expr {
        Expr::Nested(inner) => element_shape(inner, dialect),
        Expr::Array(array) => {
            if !array.elem.is_empty()
                && array.elem.iter().all(|elem| matches!(elem, Expr::Value(_)))
            {
                return ElementShape::Scalar;
            }
            let Some(literals) = array
                .elem
                .iter()
                .map(|elem| struct_literal_fields(elem, dialect))
                .collect::<Option<Vec<_>>>()
            else {
                return ElementShape::Unknown;
            };
            let Some((first, rest)) = literals.split_first() else {
                return ElementShape::Unknown;
            };
            let names: Vec<Ident> = first.iter().map(|(name, _)| name.clone()).collect();
            let consistent = rest.iter().all(|other| {
                other.len() == names.len()
                    && other
                        .iter()
                        .zip(&names)
                        .all(|((other, _), name)| same_name(other, name, dialect))
            });
            if !consistent {
                return ElementShape::Unknown;
            }
            ElementShape::Fields(
                names
                    .into_iter()
                    .enumerate()
                    .map(|(index, name)| {
                        let values = literals.iter().map(|literal| literal[index].1).collect();
                        (name, Some(values))
                    })
                    .collect(),
            )
        }
        Expr::Cast {
            data_type:
                sqlparser::ast::DataType::Array(
                    sqlparser::ast::ArrayElemTypeDef::AngleBracket(elem)
                    | sqlparser::ast::ArrayElemTypeDef::SquareBracket(elem, _)
                    | sqlparser::ast::ArrayElemTypeDef::Parenthesis(elem)
                    | sqlparser::ast::ArrayElemTypeDef::ParenthesisNotNull(elem)
                    | sqlparser::ast::ArrayElemTypeDef::Qualified(elem, _),
                ),
            ..
        } => match &**elem {
            sqlparser::ast::DataType::Struct(fields, _) => distinct_names(
                fields.iter().map(|field| field.field_name.as_ref()),
                dialect,
            )
            .map_or(ElementShape::Unknown, |names| {
                ElementShape::Fields(names.into_iter().map(|name| (name, None)).collect())
            }),
            _ => ElementShape::Scalar,
        },
        Expr::Function(function) if is_series_generator(function) => ElementShape::Scalar,
        _ => ElementShape::Unknown,
    }
}

fn without_parens(mut expr: &Expr) -> &Expr {
    while let Expr::Nested(inner) = expr {
        expr = inner;
    }
    expr
}

fn plain_call(function: &Function, names: &[&str]) -> bool {
    matches!(function.name.0.as_slice(), [part]
        if part.as_ident().is_some_and(|name| name.quote_style.is_none()
            && names.iter().any(|builtin| name.value.eq_ignore_ascii_case(builtin))))
        && matches!(function.parameters, FunctionArguments::None)
        && !function.uses_odbc_syntax
        && function.over.is_none()
        && function.within_group.is_empty()
}

fn positional_expr(arg: &FunctionArg) -> Option<&Expr> {
    match arg {
        FunctionArg::Unnamed(FunctionArgExpr::Expr(expr)) => Some(expr),
        _ => None,
    }
}

fn aggregate_element(expr: &Expr, dialect: crate::Dialect) -> Option<(&Function, &Expr, &Expr)> {
    use crate::Dialect;
    let names: &[_] = match dialect {
        Dialect::BigQuery | Dialect::PostgreSql => &["array_agg"],
        Dialect::DuckDb => &["array_agg", "list"],
        Dialect::Spark | Dialect::Databricks => &["array_agg", "collect_list"],
        _ => return None,
    };
    let (root, mut index) = match without_parens(expr) {
        Expr::CompoundFieldAccess { root, access_chain } => {
            let [AccessExpr::Subscript(Subscript::Index { index })] = access_chain.as_slice()
            else {
                return None;
            };
            (&**root, index)
        }
        Expr::Function(access)
            if matches!(dialect, Dialect::Spark | Dialect::Databricks)
                && plain_call(access, &["element_at"])
                && access.filter.is_none()
                && access.null_treatment.is_none() =>
        {
            let FunctionArguments::List(list) = &access.args else {
                return None;
            };
            let [array, index] = list.args.as_slice() else {
                return None;
            };
            if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
                return None;
            }
            (positional_expr(array)?, positional_expr(index)?)
        }
        _ => return None,
    };
    if dialect == Dialect::BigQuery
        && let Expr::Function(offset) = without_parens(index)
        && plain_call(
            offset,
            &["offset", "ordinal", "safe_offset", "safe_ordinal"],
        )
        && let FunctionArguments::List(list) = &offset.args
        && offset.filter.is_none()
        && offset.null_treatment.is_none()
        && list.duplicate_treatment.is_none()
        && list.clauses.is_empty()
        && let [position] = list.args.as_slice()
    {
        index = positional_expr(position)?;
    }
    let Expr::Function(aggregate) = without_parens(root) else {
        return None;
    };
    if !plain_call(aggregate, names) {
        return None;
    }
    let FunctionArguments::List(list) = &aggregate.args else {
        return None;
    };
    let [input] = list.args.as_slice() else {
        return None;
    };
    Some((aggregate, positional_expr(input)?, index))
}

/// The fields of a STRUCT that `expr` builds in place, with the expression
/// of each: a STRUCT literal, or in `DuckDB` a struct literal (`{'a': 1}`)
/// or `struct_pack(a := 1)`.
fn struct_value_fields(expr: &Expr, dialect: crate::Dialect) -> Option<Vec<(Ident, &Expr)>> {
    match expr {
        Expr::Nested(inner) => struct_value_fields(inner, dialect),
        Expr::Struct { .. } => struct_literal_fields(expr, dialect),
        Expr::Function(function)
            if matches!(dialect, crate::Dialect::Spark | crate::Dialect::Databricks)
                && plain_call(function, &["named_struct"]) =>
        {
            let FunctionArguments::List(list) = &function.args else {
                return None;
            };
            if function.filter.is_some()
                || function.null_treatment.is_some()
                || list.duplicate_treatment.is_some()
                || !list.clauses.is_empty()
            {
                return None;
            }
            let mut pairs = Vec::new();
            let mut names = std::collections::HashSet::new();
            let (args, remainder) = list.args.as_chunks::<2>();
            for [name, field] in args {
                let Expr::Value(value) = positional_expr(name)? else {
                    return None;
                };
                let sqlparser::ast::Value::SingleQuotedString(name) = &value.value else {
                    return None;
                };
                let name = Ident::unquoted(name.clone());
                if !names.insert(name.column_key(dialect).into_owned()) {
                    return None;
                }
                pairs.push((name, positional_expr(field)?));
            }
            (!pairs.is_empty() && remainder.is_empty()).then_some(pairs)
        }
        Expr::Function(function) if dialect == crate::Dialect::DuckDb => {
            struct_pack_fields(function, dialect)
        }
        Expr::Dictionary(fields) if dialect == crate::Dialect::DuckDb => {
            let names = distinct_names(fields.iter().map(|field| Some(&field.key)), dialect)?;
            Some(
                names
                    .into_iter()
                    .zip(fields.iter().map(|field| &*field.value))
                    .collect(),
            )
        }
        _ => None,
    }
}

/// The fields of a `DuckDB` `struct_pack` call, each named by its argument
/// (`struct_pack(a := 1)`). A call with an unnamed argument, or any other
/// clause, is not one whose fields are known.
fn struct_pack_fields(
    function: &sqlparser::ast::Function,
    dialect: crate::Dialect,
) -> Option<Vec<(Ident, &Expr)>> {
    use sqlparser::ast::{FunctionArg, FunctionArgExpr, FunctionArgOperator, FunctionArguments};
    let [sqlparser::ast::ObjectNamePart::Identifier(name)] = function.name.0.as_slice() else {
        return None;
    };
    if name.quote_style.is_some()
        || !name.value.eq_ignore_ascii_case("struct_pack")
        || function.over.is_some()
        || function.filter.is_some()
        || !function.within_group.is_empty()
    {
        return None;
    }
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    if list.duplicate_treatment.is_some() || !list.clauses.is_empty() {
        return None;
    }
    let pairs: Vec<(&sqlparser::ast::Ident, &Expr)> = list
        .args
        .iter()
        .map(|arg| match arg {
            FunctionArg::Named {
                name,
                arg: FunctionArgExpr::Expr(expr),
                operator: FunctionArgOperator::Assignment,
            } => Some((name, expr)),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let names = distinct_names(pairs.iter().map(|(name, _)| Some(*name)), dialect)?;
    Some(
        names
            .into_iter()
            .zip(pairs.into_iter().map(|(_, expr)| expr))
            .collect(),
    )
}

/// The fields of a STRUCT literal with the expression of each: named by its
/// type when it has one (`STRUCT<a INT64>(1)`), otherwise by its values
/// (`STRUCT(1 AS a)`).
fn struct_literal_fields(expr: &Expr, dialect: crate::Dialect) -> Option<Vec<(Ident, &Expr)>> {
    let Expr::Struct { values, fields } = expr else {
        return None;
    };
    if fields.is_empty() {
        let pairs: Vec<(&sqlparser::ast::Ident, &Expr)> = values
            .iter()
            .map(|value| match value {
                Expr::Named { name, expr } => Some((name, &**expr)),
                _ => None,
            })
            .collect::<Option<_>>()?;
        let names = distinct_names(pairs.iter().map(|(name, _)| Some(*name)), dialect)?;
        Some(
            names
                .into_iter()
                .zip(pairs.into_iter().map(|(_, expr)| expr))
                .collect(),
        )
    } else {
        let names = distinct_names(
            fields.iter().map(|field| field.field_name.as_ref()),
            dialect,
        )?;
        (names.len() == values.len()).then(|| names.into_iter().zip(values).collect())
    }
}

/// The names, when there is at least one, every one is present and none
/// repeats another.
fn distinct_names<'a>(
    names: impl Iterator<Item = Option<&'a sqlparser::ast::Ident>>,
    dialect: crate::Dialect,
) -> Option<Vec<Ident>> {
    let names: Vec<Ident> = names
        .map(|name| name.map(Ident::from_ast))
        .collect::<Option<_>>()?;
    let mut keys = std::collections::HashSet::new();
    let distinct = names
        .iter()
        .all(|name| keys.insert(name.column_key(dialect).into_owned()));
    (!names.is_empty() && distinct).then_some(names)
}

fn same_name(left: &Ident, right: &Ident, dialect: crate::Dialect) -> bool {
    left.column_key(dialect) == right.column_key(dialect)
}

/// Whether `function` is a `BigQuery` function that generates a series of
/// scalars.
fn is_series_generator(function: &sqlparser::ast::Function) -> bool {
    let [sqlparser::ast::ObjectNamePart::Identifier(name)] = function.name.0.as_slice() else {
        return false;
    };
    name.quote_style.is_none()
        && [
            "GENERATE_ARRAY",
            "GENERATE_DATE_ARRAY",
            "GENERATE_TIMESTAMP_ARRAY",
        ]
        .iter()
        .any(|generator| generator.eq_ignore_ascii_case(&name.value))
}

/// The last part of a function name: the range variable PostgreSQL gives a
/// function in FROM that has no alias.
fn last_name_part(name: &ObjectName) -> Option<Ident> {
    name.0.last()?.as_ident().map(Ident::from_ast)
}

fn is_unnest_function(name: &ObjectName, dialect: crate::Dialect) -> bool {
    let [part] = name.0.as_slice() else {
        return false;
    };
    part.as_ident().is_some_and(|ident| {
        Ident::from_ast(ident).column_key(dialect) == Ident::unquoted("unnest").column_key(dialect)
    })
}

/// The constraint of a join, if its operator has one.
fn join_constraint(operator: &JoinOperator) -> Option<&JoinConstraint> {
    match operator {
        JoinOperator::Join(constraint)
        | JoinOperator::Inner(constraint)
        | JoinOperator::CrossJoin(constraint)
        | JoinOperator::StraightJoin(constraint)
        | JoinOperator::FullOuter(constraint)
        | JoinOperator::Left(constraint)
        | JoinOperator::LeftOuter(constraint)
        | JoinOperator::Semi(constraint)
        | JoinOperator::LeftSemi(constraint)
        | JoinOperator::Anti(constraint)
        | JoinOperator::LeftAnti(constraint)
        | JoinOperator::AsOf { constraint, .. }
        | JoinOperator::Right(constraint)
        | JoinOperator::RightOuter(constraint)
        | JoinOperator::RightSemi(constraint)
        | JoinOperator::RightAnti(constraint) => Some(constraint),
        JoinOperator::CrossApply
        | JoinOperator::OuterApply
        | JoinOperator::ArrayJoin
        | JoinOperator::LeftArrayJoin
        | JoinOperator::InnerArrayJoin => None,
    }
}

/// Infer a column name and its classification from an expression.
fn infer_output_name(expr: &Expr) -> (Ident, OutputName) {
    match expr {
        Expr::Identifier(ident) => {
            let name = Ident::from_ast(ident);
            (name.clone(), OutputName::Explicit(name))
        }
        Expr::CompoundIdentifier(parts) => parts.last().map(Ident::from_ast).map_or_else(
            || (Ident::unquoted(""), OutputName::Anonymous),
            |name| (name.clone(), OutputName::Explicit(name)),
        ),
        Expr::Cast { expr, .. } | Expr::Nested(expr) => infer_output_name(expr),
        Expr::Function(func) => func
            .name
            .0
            .last()
            .and_then(|part| part.as_ident())
            .map(Ident::from_ast)
            .map_or_else(
                || (Ident::unquoted(""), OutputName::Anonymous),
                |name| (name.clone(), OutputName::Inferred(name)),
            ),
        _ => (Ident::unquoted(""), OutputName::Anonymous),
    }
}
