//! Static literal-call scanning used by recovery and comparison.

use super::*;

/// Record the string literals of the expressions assigned to each name.
pub(crate) fn collect_set_literals(stmts: &[Stmt], out: &mut HashMap<Arc<str>, Vec<String>>) {
    for stmt in stmts {
        match stmt {
            Stmt::Set {
                target: Target::Name(name),
                expr,
            } => {
                let mut found = StaticCalls::default();
                found.literals(expr);
                out.entry(name.clone()).or_default().extend(found.strings);
            }
            Stmt::For {
                body, else_body, ..
            } => {
                collect_set_literals(body, out);
                collect_set_literals(else_body, out);
            }
            Stmt::If {
                then, otherwise, ..
            } => {
                collect_set_literals(then, out);
                collect_set_literals(otherwise, out);
            }
            Stmt::With { body, .. }
            | Stmt::SetBlock { body, .. }
            | Stmt::FilterBlock { body, .. }
            | Stmt::AutoEscape { body }
            | Stmt::Unsupported { body, .. } => collect_set_literals(body, out),
            Stmt::Macro(def) => collect_set_literals(&def.body, out),
            Stmt::CallBlock { caller, .. } => collect_set_literals(&caller.body, out),
            _ => {}
        }
    }
}

/// Literal dependency calls found without executing.
#[derive(Default)]
pub(crate) struct StaticCalls {
    pub(crate) refs: Vec<RefCall>,
    pub(crate) sources: Vec<SourceCall>,
    pub(crate) strings: Vec<String>,
    /// Whether a ref/source call with a non-literal argument was found.
    pub(crate) dynamic: bool,
}

impl StaticCalls {
    pub(crate) fn scan_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::Emit(e) | Stmt::Set { expr: e, .. } => self.scan_expr(e),
                Stmt::Raw(_) | Stmt::Break | Stmt::Continue | Stmt::Macro(_) => {}
                Stmt::For {
                    iter,
                    filter,
                    body,
                    else_body,
                    ..
                } => {
                    self.scan_expr(iter);
                    if let Some(f) = filter {
                        self.scan_expr(f);
                    }
                    self.scan_stmts(body);
                    self.scan_stmts(else_body);
                }
                Stmt::If {
                    cond,
                    then,
                    otherwise,
                } => {
                    self.scan_expr(cond);
                    self.scan_stmts(then);
                    self.scan_stmts(otherwise);
                }
                Stmt::With { assignments, body } => {
                    for (_, e) in assignments {
                        self.scan_expr(e);
                    }
                    self.scan_stmts(body);
                }
                Stmt::SetBlock { body, .. }
                | Stmt::FilterBlock { body, .. }
                | Stmt::AutoEscape { body } => self.scan_stmts(body),
                Stmt::CallBlock { call, caller } => {
                    self.scan_call(call);
                    self.scan_stmts(&caller.body);
                }
                Stmt::Do(call) => self.scan_call(call),
                Stmt::Unsupported { exprs, body, .. } => {
                    for e in exprs {
                        self.scan_expr(e);
                    }
                    self.scan_stmts(body);
                }
            }
        }
    }

    fn scan_call(&mut self, call: &Call) {
        if let ExprKind::Var(name) = &call.callee.kind {
            let literals: Option<Vec<String>> = call
                .args
                .iter()
                .map(|arg| match arg {
                    Arg::Pos(Expr {
                        kind: ExprKind::Const(value),
                        ..
                    }) => value.as_str().map(str::to_string),
                    _ => None,
                })
                .collect();
            match (name.as_ref(), literals) {
                ("ref", Some(mut parts)) if (1..=2).contains(&parts.len()) => {
                    let name = parts.pop().unwrap_or_default();
                    self.refs.push(RefCall {
                        package: parts.pop(),
                        name,
                        version: None,
                    });
                }
                ("source", Some(parts)) if parts.len() == 2 => self.sources.push(SourceCall {
                    source_name: parts[0].clone(),
                    table_name: parts[1].clone(),
                }),
                ("ref" | "source", _) => self.dynamic = true,
                _ => {}
            }
        }
        self.scan_expr(&call.callee);
        for arg in &call.args {
            self.scan_expr(arg.expr());
        }
    }

    pub(crate) fn scan_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Var(_) | ExprKind::Const(_) => {}
            ExprKind::Call(call) => self.scan_call(call),
            ExprKind::Slice {
                expr,
                start,
                stop,
                step,
            } => {
                self.scan_expr(expr);
                for e in [start, stop, step].into_iter().flatten() {
                    self.scan_expr(e);
                }
            }
            ExprKind::Unary { expr, .. } | ExprKind::GetAttr { expr, .. } => self.scan_expr(expr),
            ExprKind::Binary { left, right, .. } => {
                self.scan_expr(left);
                self.scan_expr(right);
            }
            ExprKind::Compare { expr, ops } => {
                self.scan_expr(expr);
                for (_, e) in ops {
                    self.scan_expr(e);
                }
            }
            ExprKind::IfExpr {
                test,
                then,
                otherwise,
            } => {
                self.scan_expr(test);
                self.scan_expr(then);
                if let Some(e) = otherwise {
                    self.scan_expr(e);
                }
            }
            ExprKind::Filter { expr, args, .. } => {
                if let Some(e) = expr {
                    self.scan_expr(e);
                }
                for arg in args {
                    self.scan_expr(arg.expr());
                }
            }
            ExprKind::Test { expr, args, .. } => {
                self.scan_expr(expr);
                for arg in args {
                    self.scan_expr(arg.expr());
                }
            }
            ExprKind::GetItem { expr, subscript } => {
                self.scan_expr(expr);
                self.scan_expr(subscript);
            }
            ExprKind::List(items) | ExprKind::Tuple(items) => {
                for e in items {
                    self.scan_expr(e);
                }
            }
            ExprKind::Map(pairs) => {
                for (k, v) in pairs {
                    self.scan_expr(k);
                    self.scan_expr(v);
                }
            }
        }
    }

    /// Collect the string literals of an expression.
    pub(crate) fn literals(&mut self, expr: &Expr) {
        if let ExprKind::Const(value) = &expr.kind
            && let Some(text) = value.as_str()
        {
            self.strings.push(text.to_string());
        }
        match &expr.kind {
            ExprKind::Binary { left, right, .. } => {
                self.literals(left);
                self.literals(right);
            }
            ExprKind::Call(call) => {
                for arg in &call.args {
                    self.literals(arg.expr());
                }
            }
            ExprKind::Filter { expr: Some(e), .. } | ExprKind::Unary { expr: e, .. } => {
                self.literals(e)
            }
            _ => {}
        }
    }
}
