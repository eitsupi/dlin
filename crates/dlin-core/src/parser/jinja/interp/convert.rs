//! Conversion from minijinja's AST to dlin's IR.
//!
//! This is the only module that uses minijinja's `unstable_machinery` API,
//! which has no semver guarantee. The minijinja version is pinned exactly;
//! when it is updated, this module and the interpreter tests are where
//! changes show up. Every AST variant is matched explicitly so that a new
//! variant fails to compile instead of being ignored.

use std::sync::Arc;

use minijinja::machinery::{Token, ast};
use minijinja::syntax::SyntaxConfig;

use super::ir::{
    Arg, BinOp, Call, Expr, ExprKind, MacroDef, SourceId, Span, Stmt, Target, Template, UnaryOp,
};

/// Why a template could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseFailure {
    /// 1-based line of the error, when known.
    pub(crate) line: Option<u32>,
}

/// Parse a template and convert it to IR.
pub(crate) fn parse_template(source: &str, id: SourceId) -> Result<Template, ParseFailure> {
    parse_with_offsets(source, id, OffsetMap::default())
}

pub(super) enum VarTemplateParseError {
    Budget,
    Syntax(ParseFailure),
}

/// Bound the parser input and the size of the AST before converting it.
/// MiniJinja also limits recursive syntax, but a flat token sequence can
/// still produce a deep tree (for example a chain of binary operators).
pub(super) fn parse_var_template(
    source: &str,
    available_tokens: usize,
) -> Result<(Template, usize), VarTemplateParseError> {
    const MAX_BYTES: usize = 16 * 1024;
    if source.len() > MAX_BYTES {
        return Err(VarTemplateParseError::Budget);
    }
    let mut token_count = 0;
    for token in minijinja::machinery::tokenize(source, false, SyntaxConfig::default()) {
        if token_count == available_tokens {
            return Err(VarTemplateParseError::Budget);
        }
        token.map_err(|error| {
            VarTemplateParseError::Syntax(ParseFailure {
                line: error.line().and_then(|line| u32::try_from(line).ok()),
            })
        })?;
        token_count += 1;
    }
    let template =
        parse_template(source, SourceId::VarTemplate).map_err(VarTemplateParseError::Syntax)?;
    Ok((template, token_count))
}

/// Parse a macro file and convert it to IR.
///
/// A macro file may also define generic tests with dbt's
/// `{% test name(args) %}...{% endtest %}` block, which defines the macro
/// `test_name`, as dbt Fusion parses it. minijinja does not know the block,
/// so its tags are rewritten to `{% macro test_name(args) %}` and
/// `{% endmacro %}` before parsing. Only the tags the tokenizer reads are
/// rewritten, not text in comments, raw blocks or strings. Spans are mapped
/// back to the file as written.
pub(crate) fn parse_macro_file(source: &str, id: SourceId) -> Result<Template, ParseFailure> {
    match rewrite_test_blocks(source) {
        Some((rewritten, offsets)) => parse_with_offsets(&rewritten, id, offsets),
        None => parse_template(source, id),
    }
}

fn parse_with_offsets(
    source: &str,
    id: SourceId,
    offsets: OffsetMap,
) -> Result<Template, ParseFailure> {
    let stmt = minijinja::machinery::parse(source, "<template>", SyntaxConfig::default()).map_err(
        |error| ParseFailure {
            line: error.line().and_then(|line| u32::try_from(line).ok()),
        },
    )?;
    let converter = Converter {
        source: id,
        offsets,
    };
    let body = match &stmt {
        ast::Stmt::Template(template) => converter.stmts(&template.children),
        other => vec![converter.stmt(other)],
    };
    Ok(Template { body })
}

/// Rewrite the tags of dbt's `test` blocks to those of macros, returning the
/// rewritten source and how to map its offsets back. `None` when the source
/// has no such tag or cannot be tokenized; parsing it as written then
/// reports any error.
fn rewrite_test_blocks(source: &str) -> Option<(String, OffsetMap)> {
    let tokens = minijinja::machinery::tokenize(source, false, SyntaxConfig::default())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let mut replacements: Vec<(minijinja::machinery::Span, String)> = Vec::new();
    for (index, (token, _)) in tokens.iter().enumerate() {
        if !matches!(token, Token::BlockStart) {
            continue;
        }
        match tokens.get(index + 1) {
            Some((Token::Ident("test"), keyword)) => {
                if let Some((Token::Ident(name), name_span)) = tokens.get(index + 2) {
                    replacements.push((*keyword, "macro".to_string()));
                    replacements.push((*name_span, format!("test_{name}")));
                }
            }
            Some((Token::Ident("endtest"), keyword)) => {
                replacements.push((*keyword, "endmacro".to_string()));
            }
            _ => {}
        }
    }
    if replacements.is_empty() {
        return None;
    }

    let mut rewritten = String::with_capacity(source.len() + 16 * replacements.len());
    let mut offsets = OffsetMap::default();
    let mut copied = 0;
    for (span, replacement) in replacements {
        let (start, end) = (span.start_offset as usize, span.end_offset as usize);
        rewritten.push_str(&source[copied..start]);
        let rewritten_start = rewritten.len();
        rewritten.push_str(&replacement);
        offsets.edits.push(Edit {
            rewritten: to_u32(rewritten_start)..to_u32(rewritten.len()),
            original: span.start_offset..span.end_offset,
        });
        copied = end;
    }
    rewritten.push_str(&source[copied..]);
    Some((rewritten, offsets))
}

fn to_u32(offset: usize) -> u32 {
    u32::try_from(offset).unwrap_or(u32::MAX)
}

/// Maps byte offsets in a rewritten source back to the source as written.
/// Rewriting replaces identifiers only, so lines do not move.
#[derive(Debug, Default)]
struct OffsetMap {
    /// The replaced ranges, in order.
    edits: Vec<Edit>,
}

#[derive(Debug)]
struct Edit {
    rewritten: std::ops::Range<u32>,
    original: std::ops::Range<u32>,
}

impl OffsetMap {
    /// The offset in the source as written. An offset inside a replacement
    /// maps into the text it replaced.
    fn original(&self, offset: u32) -> u32 {
        let mut shifted = offset;
        for edit in &self.edits {
            if offset >= edit.rewritten.end {
                shifted = offset - edit.rewritten.end + edit.original.end;
            } else {
                if offset > edit.rewritten.start {
                    let within = offset - edit.rewritten.start;
                    return (edit.original.start + within).min(edit.original.end);
                }
                break;
            }
        }
        shifted
    }
}

struct Converter {
    source: SourceId,
    offsets: OffsetMap,
}

impl Converter {
    fn span(&self, span: minijinja::machinery::Span) -> Span {
        Span {
            source: self.source,
            line: u32::from(span.start_line),
            start: self.offsets.original(span.start_offset),
            end: self.offsets.original(span.end_offset),
        }
    }

    fn stmts(&self, stmts: &[ast::Stmt<'_>]) -> Vec<Stmt> {
        stmts.iter().map(|stmt| self.stmt(stmt)).collect()
    }

    fn stmt(&self, stmt: &ast::Stmt<'_>) -> Stmt {
        match stmt {
            ast::Stmt::Template(template) => Stmt::Unsupported {
                construct: "nested template",
                span: self.span(template.span()),
                exprs: Vec::new(),
                body: self.stmts(&template.children),
            },
            ast::Stmt::EmitExpr(emit) => Stmt::Emit(self.expr(&emit.expr)),
            ast::Stmt::EmitRaw(raw) => Stmt::Raw(Arc::from(raw.raw)),
            ast::Stmt::ForLoop(for_loop) => Stmt::For {
                target: self.target(&for_loop.target),
                iter: self.expr(&for_loop.iter),
                filter: for_loop.filter_expr.as_ref().map(|e| self.expr(e)),
                recursive: for_loop.recursive,
                body: self.stmts(&for_loop.body),
                else_body: self.stmts(&for_loop.else_body),
                span: self.span(for_loop.span()),
            },
            ast::Stmt::IfCond(cond) => Stmt::If {
                cond: self.expr(&cond.expr),
                then: self.stmts(&cond.true_body),
                otherwise: self.stmts(&cond.false_body),
            },
            ast::Stmt::WithBlock(with) => Stmt::With {
                assignments: with
                    .assignments
                    .iter()
                    .map(|(target, expr)| (self.target(target), self.expr(expr)))
                    .collect(),
                body: self.stmts(&with.body),
            },
            ast::Stmt::Set(set) => Stmt::Set {
                target: self.target(&set.target),
                expr: self.expr(&set.expr),
            },
            ast::Stmt::SetBlock(set) => Stmt::SetBlock {
                target: self.target(&set.target),
                filter: set.filter.as_ref().map(|e| self.expr(e)),
                body: self.stmts(&set.body),
            },
            ast::Stmt::AutoEscape(auto) => Stmt::AutoEscape {
                body: self.stmts(&auto.body),
            },
            ast::Stmt::FilterBlock(block) => Stmt::FilterBlock {
                filter: self.expr(&block.filter),
                body: self.stmts(&block.body),
            },
            ast::Stmt::Block(block) => Stmt::Unsupported {
                construct: "block",
                span: self.span(block.span()),
                exprs: Vec::new(),
                body: self.stmts(&block.body),
            },
            ast::Stmt::Import(import) => Stmt::Unsupported {
                construct: "import",
                span: self.span(import.span()),
                exprs: vec![self.expr(&import.expr)],
                body: Vec::new(),
            },
            ast::Stmt::FromImport(import) => Stmt::Unsupported {
                construct: "from import",
                span: self.span(import.span()),
                exprs: vec![self.expr(&import.expr)],
                body: Vec::new(),
            },
            ast::Stmt::Extends(extends) => Stmt::Unsupported {
                construct: "extends",
                span: self.span(extends.span()),
                exprs: vec![self.expr(&extends.name)],
                body: Vec::new(),
            },
            ast::Stmt::Include(include) => Stmt::Unsupported {
                construct: "include",
                span: self.span(include.span()),
                exprs: vec![self.expr(&include.name)],
                body: Vec::new(),
            },
            ast::Stmt::Macro(def) => Stmt::Macro(Arc::new(self.macro_def(def))),
            ast::Stmt::CallBlock(block) => Stmt::CallBlock {
                call: self.call(&block.call),
                caller: Arc::new(self.macro_def(&block.macro_decl)),
            },
            ast::Stmt::Continue(_) => Stmt::Continue,
            ast::Stmt::Break(_) => Stmt::Break,
            ast::Stmt::Do(do_stmt) => Stmt::Do(self.call(&do_stmt.call)),
        }
    }

    fn macro_def(&self, def: &ast::Spanned<ast::Macro<'_>>) -> MacroDef {
        let params = def
            .args
            .iter()
            .map(|arg| match arg {
                ast::Expr::Var(var) => Arc::from(var.id),
                // minijinja only accepts names as macro parameters.
                other => Arc::from(other.description()),
            })
            .collect();
        let defaults: Vec<Expr> = def.defaults.iter().map(|e| self.expr(e)).collect();
        let body = self.stmts(&def.body);
        let mut names = Vec::new();
        super::ir::collect_names(&body, &defaults, &mut names);
        names.sort();
        names.dedup();
        MacroDef {
            name: Arc::from(def.name),
            params,
            defaults,
            body,
            names,
        }
    }

    fn call(&self, call: &ast::Spanned<ast::Call<'_>>) -> Call {
        let callee = self.expr(&call.expr);
        let mut receiver = &callee;
        while let ExprKind::GetAttr { expr, .. } | ExprKind::GetItem { expr, .. } = &receiver.kind {
            receiver = expr;
        }
        let mut span = self.span(call.span());
        if receiver.span.start < span.start {
            span.start = receiver.span.start;
            span.line = receiver.span.line;
        }
        Call {
            callee: Box::new(callee),
            args: self.args(&call.args),
            span,
        }
    }

    fn args(&self, args: &[ast::CallArg<'_>]) -> Vec<Arg> {
        args.iter()
            .map(|arg| match arg {
                ast::CallArg::Pos(e) => Arg::Pos(self.expr(e)),
                ast::CallArg::Kwarg(name, e) => Arg::Kw(Arc::from(*name), self.expr(e)),
                ast::CallArg::PosSplat(e) => Arg::PosSplat(self.expr(e)),
                ast::CallArg::KwargSplat(e) => Arg::KwSplat(self.expr(e)),
            })
            .collect()
    }

    fn target(&self, expr: &ast::Expr<'_>) -> Target {
        match expr {
            ast::Expr::Var(var) => Target::Name(Arc::from(var.id)),
            ast::Expr::List(list) => {
                Target::Tuple(list.items.iter().map(|e| self.target(e)).collect())
            }
            ast::Expr::Tuple(tuple) => {
                Target::Tuple(tuple.items.iter().map(|e| self.target(e)).collect())
            }
            ast::Expr::GetAttr(attr) => Target::Attr {
                expr: self.expr(&attr.expr),
                name: Arc::from(attr.name),
            },
            other => Target::Unsupported(self.expr(other)),
        }
    }

    fn boxed(&self, expr: &ast::Expr<'_>) -> Box<Expr> {
        Box::new(self.expr(expr))
    }

    fn expr(&self, expr: &ast::Expr<'_>) -> Expr {
        let span = self.span(expr.span());
        let kind = match expr {
            ast::Expr::Var(var) => ExprKind::Var(Arc::from(var.id)),
            ast::Expr::Const(c) => ExprKind::Const(c.value.clone()),
            ast::Expr::Slice(slice) => ExprKind::Slice {
                expr: self.boxed(&slice.expr),
                start: slice.start.as_ref().map(|e| self.boxed(e)),
                stop: slice.stop.as_ref().map(|e| self.boxed(e)),
                step: slice.step.as_ref().map(|e| self.boxed(e)),
            },
            ast::Expr::UnaryOp(op) => ExprKind::Unary {
                op: match op.op {
                    ast::UnaryOpKind::Not => UnaryOp::Not,
                    ast::UnaryOpKind::Neg => UnaryOp::Neg,
                },
                expr: self.boxed(&op.expr),
            },
            ast::Expr::BinOp(op) => ExprKind::Binary {
                op: bin_op(op.op),
                left: self.boxed(&op.left),
                right: self.boxed(&op.right),
            },
            ast::Expr::Compare(cmp) => ExprKind::Compare {
                expr: self.boxed(&cmp.expr),
                ops: cmp
                    .ops
                    .iter()
                    .map(|op| (compare_op(op.op), self.expr(&op.expr)))
                    .collect(),
            },
            ast::Expr::IfExpr(if_expr) => ExprKind::IfExpr {
                test: self.boxed(&if_expr.test_expr),
                then: self.boxed(&if_expr.true_expr),
                otherwise: if_expr.false_expr.as_ref().map(|e| self.boxed(e)),
            },
            ast::Expr::Filter(filter) => ExprKind::Filter {
                name: Arc::from(filter.name),
                expr: filter.expr.as_ref().map(|e| self.boxed(e)),
                args: self.args(&filter.args),
            },
            ast::Expr::Test(test) => ExprKind::Test {
                name: Arc::from(test.name),
                expr: self.boxed(&test.expr),
                args: self.args(&test.args),
            },
            ast::Expr::GetAttr(attr) => ExprKind::GetAttr {
                expr: self.boxed(&attr.expr),
                name: Arc::from(attr.name),
            },
            ast::Expr::GetItem(item) => ExprKind::GetItem {
                expr: self.boxed(&item.expr),
                subscript: self.boxed(&item.subscript_expr),
            },
            ast::Expr::Call(call) => {
                let call = self.call(call);
                return Expr {
                    span: call.span,
                    kind: ExprKind::Call(call),
                };
            }
            ast::Expr::List(list) => {
                ExprKind::List(list.items.iter().map(|e| self.expr(e)).collect())
            }
            ast::Expr::Tuple(tuple) => {
                ExprKind::Tuple(tuple.items.iter().map(|e| self.expr(e)).collect())
            }
            ast::Expr::Map(map) => ExprKind::Map(
                map.keys
                    .iter()
                    .zip(&map.values)
                    .map(|(k, v)| (self.expr(k), self.expr(v)))
                    .collect(),
            ),
        };
        Expr { kind, span }
    }
}

fn bin_op(op: ast::BinOpKind) -> BinOp {
    match op {
        ast::BinOpKind::Eq => BinOp::Eq,
        ast::BinOpKind::Ne => BinOp::Ne,
        ast::BinOpKind::Lt => BinOp::Lt,
        ast::BinOpKind::Lte => BinOp::Lte,
        ast::BinOpKind::Gt => BinOp::Gt,
        ast::BinOpKind::Gte => BinOp::Gte,
        ast::BinOpKind::ScAnd => BinOp::And,
        ast::BinOpKind::ScOr => BinOp::Or,
        ast::BinOpKind::Add => BinOp::Add,
        ast::BinOpKind::Sub => BinOp::Sub,
        ast::BinOpKind::Mul => BinOp::Mul,
        ast::BinOpKind::Div => BinOp::Div,
        ast::BinOpKind::FloorDiv => BinOp::FloorDiv,
        ast::BinOpKind::Rem => BinOp::Rem,
        ast::BinOpKind::Pow => BinOp::Pow,
        ast::BinOpKind::Concat => BinOp::Concat,
        ast::BinOpKind::In => BinOp::In,
    }
}

fn compare_op(op: ast::CompareOpKind) -> BinOp {
    match op {
        ast::CompareOpKind::Eq => BinOp::Eq,
        ast::CompareOpKind::Ne => BinOp::Ne,
        ast::CompareOpKind::Lt => BinOp::Lt,
        ast::CompareOpKind::Lte => BinOp::Lte,
        ast::CompareOpKind::Gt => BinOp::Gt,
        ast::CompareOpKind::Gte => BinOp::Gte,
        ast::CompareOpKind::In => BinOp::In,
        ast::CompareOpKind::NotIn => BinOp::NotIn,
    }
}
