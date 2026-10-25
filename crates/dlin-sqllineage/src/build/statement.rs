use sqlparser::ast::{self, AssignmentTarget, FunctionArguments, Ident, MergeAction, Statement};

use crate::build::LineageBuilder;
use crate::build::expr::determine_edge_kind;
use crate::graph::scope::ScopeColumn;
use crate::types::{StatementType, TableRef};

impl LineageBuilder {
    pub(crate) fn visit_statement(&mut self, stmt: &Statement) -> StatementType {
        match stmt {
            Statement::Query(query) => {
                self.visit_query(query);
                StatementType::Query
            }

            Statement::Insert(insert) => {
                self.graph.tables.output = Some(self.table_ref_from_table_object(&insert.table));
                if let Some(ref source) = insert.source {
                    self.visit_query(source);
                }
                StatementType::Insert
            }

            Statement::CreateTable(ct) => {
                if let Some(ref query) = ct.query {
                    self.graph.tables.output = Some(self.table_ref_from_object_name(&ct.name));
                    self.visit_query(query);
                    StatementType::CreateTable
                } else {
                    StatementType::Other
                }
            }

            Statement::Update(update) => {
                if let Some(tref) = self.table_ref_from_table_factor(&update.table.relation) {
                    self.graph.tables.output = Some(tref);
                }
                for join in &update.table.joins {
                    self.visit_table_factor(&join.relation);
                }
                if let Some(from_kind) = &update.from {
                    match from_kind {
                        ast::UpdateTableFromKind::BeforeSet(tables)
                        | ast::UpdateTableFromKind::AfterSet(tables) => {
                            self.visit_from(tables);
                        }
                    }
                }
                for assignment in &update.assignments {
                    let col_name = assignment_target_name(&assignment.target);
                    let ancestors = self.collect_ancestors(&assignment.value);
                    let kind = determine_edge_kind(&assignment.value);
                    let output = self.graph.add_output(col_name.clone(), kind.clone());
                    for &anc in &ancestors {
                        self.graph.add_edge(anc, output, kind.clone());
                    }
                    self.graph.scopes.add_output_column(
                        self.current_scope,
                        ScopeColumn {
                            name: col_name,
                            node_id: output,
                        },
                    );
                }
                if let Some(sel) = &update.selection {
                    self.scan_expr_for_tables(sel);
                }
                StatementType::Update
            }

            Statement::Delete(delete) => {
                let from_tables = match &delete.from {
                    ast::FromTable::WithFromKeyword(tables)
                    | ast::FromTable::WithoutKeyword(tables) => tables,
                };
                if let Some(tref) = from_tables
                    .first()
                    .and_then(|f| self.table_ref_from_table_factor(&f.relation))
                {
                    self.graph.tables.output = Some(tref);
                }
                if let Some(using) = &delete.using {
                    self.visit_from(using);
                }
                if let Some(sel) = &delete.selection {
                    self.scan_expr_for_tables(sel);
                }
                StatementType::Delete
            }

            Statement::Merge(merge) => {
                if let Some(tref) = self.table_ref_from_table_factor(&merge.table) {
                    self.graph.tables.output = Some(tref);
                }
                self.visit_table_factor(&merge.source);

                for clause in &merge.clauses {
                    match &clause.action {
                        MergeAction::Update(upd) => {
                            for assignment in &upd.assignments {
                                let col_name = assignment_target_name(&assignment.target);
                                let ancestors = self.collect_ancestors(&assignment.value);
                                let kind = determine_edge_kind(&assignment.value);
                                let output = self.graph.add_output(col_name.clone(), kind.clone());
                                for &anc in &ancestors {
                                    self.graph.add_edge(anc, output, kind.clone());
                                }
                                self.graph.scopes.add_output_column(
                                    self.current_scope,
                                    ScopeColumn {
                                        name: col_name,
                                        node_id: output,
                                    },
                                );
                            }
                        }
                        MergeAction::Insert(ins) => {
                            if let ast::MergeInsertKind::Values(values) = &ins.kind {
                                let col_names: Vec<String> = ins
                                    .columns
                                    .iter()
                                    .map(|c| {
                                        c.0.last()
                                            .and_then(|p| p.as_ident())
                                            .map(|i| i.value.clone())
                                            .unwrap_or_default()
                                    })
                                    .collect();
                                for row in &values.rows {
                                    for (i, expr) in row.iter().enumerate() {
                                        let col_name = col_names
                                            .get(i)
                                            .cloned()
                                            .unwrap_or_else(|| format!("col{i}"));
                                        let ancestors = self.collect_ancestors(expr);
                                        let kind = determine_edge_kind(expr);
                                        let output =
                                            self.graph.add_output(col_name.clone(), kind.clone());
                                        for &anc in &ancestors {
                                            self.graph.add_edge(anc, output, kind.clone());
                                        }
                                        self.graph.scopes.add_output_column(
                                            self.current_scope,
                                            ScopeColumn {
                                                name: col_name,
                                                node_id: output,
                                            },
                                        );
                                    }
                                }
                            }
                        }
                        MergeAction::Delete { .. } => {}
                    }
                }
                StatementType::Merge
            }

            // Statements that do not carry data lineage.
            Statement::AlterCollation(_)
            | Statement::AlterConnector { .. }
            | Statement::AlterFunction(_)
            | Statement::AlterIndex { .. }
            | Statement::AlterOperator { .. }
            | Statement::AlterOperatorClass { .. }
            | Statement::AlterOperatorFamily { .. }
            | Statement::AlterPolicy { .. }
            | Statement::AlterRole { .. }
            | Statement::AlterSchema { .. }
            | Statement::AlterSession { .. }
            | Statement::AlterTable { .. }
            | Statement::AlterType { .. }
            | Statement::AlterUser { .. }
            | Statement::AlterView { .. }
            | Statement::Analyze { .. }
            | Statement::Assert { .. }
            | Statement::AttachDatabase { .. }
            | Statement::AttachDuckDBDatabase { .. }
            | Statement::Cache { .. }
            | Statement::Call(_)
            | Statement::Case(_)
            | Statement::Close { .. }
            | Statement::Comment { .. }
            | Statement::Commit { .. }
            | Statement::Copy { .. }
            | Statement::CopyIntoSnowflake { .. }
            | Statement::CreateCollation(_)
            | Statement::CreateConnector { .. }
            | Statement::CreateDatabase { .. }
            | Statement::CreateDomain { .. }
            | Statement::CreateExtension { .. }
            | Statement::CreateFunction { .. }
            | Statement::CreateIndex(_)
            | Statement::CreateMacro { .. }
            | Statement::CreateOperator { .. }
            | Statement::CreateOperatorClass { .. }
            | Statement::CreateOperatorFamily { .. }
            | Statement::CreatePolicy { .. }
            | Statement::CreateProcedure { .. }
            | Statement::CreateRole { .. }
            | Statement::CreateSchema { .. }
            | Statement::CreateSecret { .. }
            | Statement::CreateSequence { .. }
            | Statement::CreateServer { .. }
            | Statement::CreateStage { .. }
            | Statement::CreateTrigger { .. }
            | Statement::CreateType { .. }
            | Statement::CreateUser { .. }
            | Statement::CreateView { .. }
            | Statement::CreateVirtualTable { .. }
            | Statement::Deallocate { .. }
            | Statement::Declare { .. }
            | Statement::Deny { .. }
            | Statement::DetachDuckDBDatabase { .. }
            | Statement::Directory { .. }
            | Statement::Discard { .. }
            | Statement::Drop { .. }
            | Statement::DropConnector { .. }
            | Statement::DropDomain { .. }
            | Statement::DropExtension { .. }
            | Statement::DropFunction { .. }
            | Statement::DropOperator { .. }
            | Statement::DropOperatorClass { .. }
            | Statement::DropOperatorFamily { .. }
            | Statement::DropPolicy { .. }
            | Statement::DropProcedure { .. }
            | Statement::DropSecret { .. }
            | Statement::DropTrigger { .. }
            | Statement::Execute { .. }
            | Statement::Explain { .. }
            | Statement::ExplainTable { .. }
            | Statement::ExportData { .. }
            | Statement::Fetch { .. }
            | Statement::Flush { .. }
            | Statement::Grant { .. }
            | Statement::If(_)
            | Statement::Install { .. }
            | Statement::Kill { .. }
            | Statement::LISTEN { .. }
            | Statement::List(_)
            | Statement::Load { .. }
            | Statement::LoadData { .. }
            | Statement::Lock { .. }
            | Statement::LockTables { .. }
            | Statement::Msck { .. }
            | Statement::NOTIFY { .. }
            | Statement::Open(_)
            | Statement::OptimizeTable { .. }
            | Statement::Pragma { .. }
            | Statement::Prepare { .. }
            | Statement::Print(_)
            | Statement::RaisError { .. }
            | Statement::Raise { .. }
            | Statement::ReleaseSavepoint { .. }
            | Statement::Remove { .. }
            | Statement::RenameTable { .. }
            | Statement::Reset { .. }
            | Statement::Return(_)
            | Statement::Revoke { .. }
            | Statement::Rollback { .. }
            | Statement::Savepoint { .. }
            | Statement::Set(_)
            | Statement::ShowCatalogs { .. }
            | Statement::ShowCharset { .. }
            | Statement::ShowCollation { .. }
            | Statement::ShowColumns { .. }
            | Statement::ShowCreate { .. }
            | Statement::ShowDatabases { .. }
            | Statement::ShowFunctions { .. }
            | Statement::ShowObjects { .. }
            | Statement::ShowProcessList { .. }
            | Statement::ShowSchemas { .. }
            | Statement::ShowStatus { .. }
            | Statement::ShowTables { .. }
            | Statement::ShowVariable { .. }
            | Statement::ShowVariables { .. }
            | Statement::ShowViews { .. }
            | Statement::StartTransaction { .. }
            | Statement::Throw { .. }
            | Statement::Truncate { .. }
            | Statement::UNCache { .. }
            | Statement::UNLISTEN { .. }
            | Statement::Unload { .. }
            | Statement::UnlockTables
            | Statement::Use(_)
            | Statement::Vacuum { .. }
            | Statement::WaitFor { .. }
            | Statement::While { .. } => StatementType::Other,
        }
    }

    pub(crate) fn scan_expr_for_tables(&mut self, expr: &ast::Expr) {
        match expr {
            ast::Expr::Subquery(query) => {
                self.push_scope(crate::graph::scope::ScopeKind::Subquery);
                self.visit_query(query);
                self.pop_scope();
            }
            ast::Expr::InSubquery { subquery, expr, .. } => {
                self.push_scope(crate::graph::scope::ScopeKind::Subquery);
                self.visit_query(subquery);
                self.pop_scope();
                self.scan_expr_for_tables(expr);
            }
            ast::Expr::Exists { subquery, .. } => {
                self.push_scope(crate::graph::scope::ScopeKind::Subquery);
                self.visit_query(subquery);
                self.pop_scope();
            }
            ast::Expr::BinaryOp { left, right, .. } => {
                self.scan_expr_for_tables(left);
                self.scan_expr_for_tables(right);
            }
            ast::Expr::UnaryOp { expr, .. } => {
                self.scan_expr_for_tables(expr);
            }
            ast::Expr::Nested(inner) => {
                self.scan_expr_for_tables(inner);
            }
            ast::Expr::Between {
                expr, low, high, ..
            } => {
                self.scan_expr_for_tables(expr);
                self.scan_expr_for_tables(low);
                self.scan_expr_for_tables(high);
            }
            ast::Expr::InList { expr, list, .. } => {
                self.scan_expr_for_tables(expr);
                for item in list {
                    self.scan_expr_for_tables(item);
                }
            }
            ast::Expr::IsNull(e) | ast::Expr::IsNotNull(e) | ast::Expr::Cast { expr: e, .. } => {
                self.scan_expr_for_tables(e);
            }
            ast::Expr::Case {
                operand,
                conditions,
                else_result,
                ..
            } => {
                if let Some(op) = operand {
                    self.scan_expr_for_tables(op);
                }
                for cw in conditions {
                    self.scan_expr_for_tables(&cw.condition);
                    self.scan_expr_for_tables(&cw.result);
                }
                if let Some(el) = else_result {
                    self.scan_expr_for_tables(el);
                }
            }
            ast::Expr::Function(func) => {
                if let FunctionArguments::List(list) = &func.args {
                    for arg in &list.args {
                        match arg {
                            ast::FunctionArg::Unnamed(arg_expr)
                            | ast::FunctionArg::Named { arg: arg_expr, .. }
                            | ast::FunctionArg::ExprNamed { arg: arg_expr, .. } => {
                                if let ast::FunctionArgExpr::Expr(e) = arg_expr {
                                    self.scan_expr_for_tables(e);
                                }
                            }
                        }
                    }
                }
                if let FunctionArguments::Subquery(q) = &func.args {
                    self.visit_query(q);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn normalize_ident(&self, ident: &Ident) -> String {
        if self.normalize_case && ident.quote_style.is_none() {
            ident.value.to_lowercase()
        } else {
            ident.value.clone()
        }
    }

    pub(crate) fn table_ref_from_object_name(&self, name: &ast::ObjectName) -> TableRef {
        let parts: Vec<String> = name
            .0
            .iter()
            .filter_map(|p| p.as_ident())
            .map(|i| self.normalize_ident(i))
            .collect();
        match parts.as_slice() {
            [table] => TableRef::new(table.clone()),
            [schema, table] => TableRef::with_schema(schema.clone(), table.clone()),
            [catalog, schema, table] => TableRef {
                catalog: Some(catalog.clone()),
                schema: Some(schema.clone()),
                table: table.clone(),
            },
            _ => TableRef::new(name.to_string()),
        }
    }

    fn table_ref_from_table_object(&self, obj: &ast::TableObject) -> TableRef {
        match obj {
            ast::TableObject::TableName(name) => self.table_ref_from_object_name(name),
            _ => TableRef::new(obj.to_string()),
        }
    }

    fn table_ref_from_table_factor(&self, factor: &ast::TableFactor) -> Option<TableRef> {
        match factor {
            ast::TableFactor::Table { name, .. } => Some(self.table_ref_from_object_name(name)),
            _ => None,
        }
    }
}

fn assignment_target_name(target: &AssignmentTarget) -> String {
    match target {
        AssignmentTarget::ColumnName(name) => name
            .0
            .last()
            .and_then(|p| p.as_ident())
            .map(|i| i.value.clone())
            .unwrap_or_default(),
        AssignmentTarget::Tuple(names) => names
            .first()
            .and_then(|n| n.0.last())
            .and_then(|p| p.as_ident())
            .map(|i| i.value.clone())
            .unwrap_or_default(),
    }
}
