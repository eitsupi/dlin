use sqlparser::ast::{
    Expr, GroupByExpr, Join, JoinConstraint, JoinOperator, ObjectName, Select, SelectFlavor,
    SelectItem, SelectItemQualifiedWildcardKind, TableFactor, TableWithJoins,
    WildcardAdditionalOptions,
};

use std::ops::ControlFlow;

use crate::build::LineageBuilder;
use crate::build::expr::determine_edge_kind;
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
        let input_mark = self.graph.tables.inputs.len();
        self.visit_from(&select.from);
        // Bind FROM first, but retain SQL clause order for read spellings:
        // projection subqueries appear before the FROM clause's relations.
        let from_inputs = self.graph.tables.inputs.split_off(input_mark);
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
        if let Some(qualify) = &select.qualify {
            self.scan_expr_for_tables(qualify);
        }
    }

    /// Process projection items — creates Output nodes and edges.
    pub(crate) fn visit_projection(&mut self, items: &[SelectItem]) {
        for item in items {
            match item {
                SelectItem::UnnamedExpr(expr) => {
                    let ancestors = self.collect_ancestors(expr);
                    let kind = determine_edge_kind(expr);
                    let (name, output_name) = infer_output_name(expr);
                    let output = self.graph.add_output(name.clone(), kind.clone());
                    self.graph.set_output_name(output, output_name);
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
                    let base = match kind {
                        SelectItemQualifiedWildcardKind::ObjectName(obj_name) => {
                            StarBase::Qualified(Self::object_name_parts(obj_name))
                        }
                        SelectItemQualifiedWildcardKind::Expr(expr) => {
                            StarBase::Expr(self.collect_ancestors(expr))
                        }
                    };
                    let star_options = self.star_options(options);
                    let star = self.graph.add_star(base, star_options, self.env());
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
                let table_ref = Self::table_ref_from_object_name(name);
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
                let dependencies: Vec<_> = array_exprs
                    .iter()
                    .map(|expr| self.collect_ancestors(expr))
                    .collect();
                if alias.is_none()
                    && self.dialect == crate::Dialect::BigQuery
                    && let [array] = array_exprs.as_slice()
                {
                    let [dependencies] = <[_; 1]>::try_from(dependencies)
                        .expect("one dependency list per array expression");
                    // A scalar element has no field to publish; the
                    // expansion still publishes its offset.
                    let open_element = (!is_known_scalar_array(array)).then_some(dependencies);
                    if open_element.is_some() || *with_offset {
                        self.add_anonymous_unnest(
                            open_element,
                            *with_offset,
                            with_offset_alias.as_ref(),
                        );
                    }
                    return;
                }
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
                let table_ref = Self::table_ref_from_object_name(name);
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
    /// array of STRUCTs publishes its fields as columns of the FROM clause;
    /// when the element type is not known, `open_element` holds the array's
    /// dependencies and any name may be one of those fields. No name
    /// qualifies a reference to them. `WITH OFFSET` adds a column named by
    /// its alias, `offset` by default.
    fn add_anonymous_unnest(
        &mut self,
        open_element: Option<Vec<crate::graph::node::NodeId>>,
        with_offset: bool,
        with_offset_alias: Option<&sqlparser::ast::Ident>,
    ) {
        let columns = with_offset
            .then(|| VirtualColumn {
                name: with_offset_alias.map_or_else(|| Ident::unquoted("offset"), Ident::from_ast),
                state: VirtualColumnState::KnownEmpty,
                dependencies: Vec::new(),
            })
            .into_iter()
            .collect();
        let source =
            self.graph
                .scopes
                .add_virtual_source(self.current_scope, columns, open_element);
        self.graph
            .scopes
            .add_anonymous(self.current_scope, AnonymousRange::Virtual(source));
    }
}

/// Whether a `BigQuery` array is known to hold values that are not STRUCTs, so
/// that its element publishes no field names: a literal of literal values, a
/// cast to an array of another type, or a function that generates a series.
fn is_known_scalar_array(expr: &Expr) -> bool {
    match expr {
        Expr::Nested(inner) => is_known_scalar_array(inner),
        Expr::Array(array) => {
            !array.elem.is_empty() && array.elem.iter().all(|elem| matches!(elem, Expr::Value(_)))
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
        } => !matches!(**elem, sqlparser::ast::DataType::Struct(..)),
        Expr::Function(function) => {
            let [sqlparser::ast::ObjectNamePart::Identifier(name)] = function.name.0.as_slice()
            else {
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
        _ => false,
    }
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
