//! dlin's own representation of a parsed Jinja template.
//!
//! The abstract interpreter works on this IR instead of minijinja's AST,
//! which is only available through minijinja's unstable API. Only
//! [`super::convert`] touches that API. The IR owns its strings, so it can be
//! shared between threads and outlive the parsed source.

use std::sync::Arc;

use minijinja::Value;

/// Index of a source file in a [`super::library::MacroLibrary`] or of the
/// template being analysed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum SourceId {
    /// The SQL file being analysed (a model, snapshot, test or analysis).
    Template,
    /// A project macro file, by index in the library.
    MacroFile(u32),
}

/// A location in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct Span {
    pub(crate) source: SourceId,
    /// 1-based line of the start.
    pub(crate) line: u32,
    /// Byte offsets into the source.
    pub(crate) start: u32,
    pub(crate) end: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct Expr {
    pub(crate) kind: ExprKind,
    pub(crate) span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnaryOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BinOp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
    Add,
    Sub,
    Mul,
    Div,
    FloorDiv,
    Rem,
    Pow,
    Concat,
    In,
    NotIn,
}

impl BinOp {
    /// The operator as written in a Jinja expression.
    pub(crate) fn symbol(self) -> &'static str {
        match self {
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Lte => "<=",
            BinOp::Gt => ">",
            BinOp::Gte => ">=",
            BinOp::And => "and",
            BinOp::Or => "or",
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::FloorDiv => "//",
            BinOp::Rem => "%",
            BinOp::Pow => "**",
            BinOp::Concat => "~",
            BinOp::In => "in",
            BinOp::NotIn => "not in",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ExprKind {
    Var(Arc<str>),
    Const(Value),
    Slice {
        expr: Box<Expr>,
        start: Option<Box<Expr>>,
        stop: Option<Box<Expr>>,
        step: Option<Box<Expr>>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// A chained comparison `a < b < c`, evaluated pairwise.
    Compare {
        expr: Box<Expr>,
        ops: Vec<(BinOp, Expr)>,
    },
    IfExpr {
        test: Box<Expr>,
        then: Box<Expr>,
        otherwise: Option<Box<Expr>>,
    },
    Filter {
        name: Arc<str>,
        expr: Option<Box<Expr>>,
        args: Vec<Arg>,
    },
    Test {
        name: Arc<str>,
        expr: Box<Expr>,
        args: Vec<Arg>,
    },
    GetAttr {
        expr: Box<Expr>,
        name: Arc<str>,
    },
    GetItem {
        expr: Box<Expr>,
        subscript: Box<Expr>,
    },
    Call(Call),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Map(Vec<(Expr, Expr)>),
}

#[derive(Debug, Clone)]
pub(crate) struct Call {
    pub(crate) callee: Box<Expr>,
    pub(crate) args: Vec<Arg>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) enum Arg {
    Pos(Expr),
    Kw(Arc<str>, Expr),
    PosSplat(Expr),
    KwSplat(Expr),
}

impl Arg {
    pub(crate) fn expr(&self) -> &Expr {
        match self {
            Arg::Pos(e) | Arg::Kw(_, e) | Arg::PosSplat(e) | Arg::KwSplat(e) => e,
        }
    }
}

/// The left-hand side of an assignment or a loop target.
#[derive(Debug, Clone)]
pub(crate) enum Target {
    Name(Arc<str>),
    Tuple(Vec<Target>),
    /// `ns.attr` on a namespace.
    Attr { expr: Expr, name: Arc<str> },
    /// Any other expression, which Jinja rejects at runtime.
    Unsupported(Expr),
}

#[derive(Debug)]
pub(crate) struct MacroDef {
    pub(crate) name: Arc<str>,
    pub(crate) params: Vec<Arc<str>>,
    /// Defaults for the last `defaults.len()` parameters.
    pub(crate) defaults: Vec<Expr>,
    pub(crate) body: Vec<Stmt>,
    pub(crate) span: Span,
}

impl MacroDef {
    /// The default expression of a parameter, if it has one.
    pub(crate) fn default_for(&self, index: usize) -> Option<&Expr> {
        let first = self.params.len().checked_sub(self.defaults.len())?;
        index.checked_sub(first).and_then(|i| self.defaults.get(i))
    }
}

#[derive(Debug, Clone)]
pub(crate) enum Stmt {
    Emit(Expr),
    Raw,
    For {
        target: Target,
        iter: Expr,
        filter: Option<Expr>,
        recursive: bool,
        body: Vec<Stmt>,
        else_body: Vec<Stmt>,
        span: Span,
    },
    If {
        cond: Expr,
        then: Vec<Stmt>,
        otherwise: Vec<Stmt>,
    },
    With {
        assignments: Vec<(Target, Expr)>,
        body: Vec<Stmt>,
    },
    Set {
        target: Target,
        expr: Expr,
    },
    SetBlock {
        target: Target,
        filter: Option<Expr>,
        body: Vec<Stmt>,
    },
    FilterBlock {
        filter: Expr,
        body: Vec<Stmt>,
    },
    AutoEscape {
        body: Vec<Stmt>,
    },
    Macro(Arc<MacroDef>),
    CallBlock {
        call: Call,
        caller: Arc<MacroDef>,
    },
    Do(Call),
    Break,
    Continue,
    /// A construct the interpreter does not model. Its parts are kept so a
    /// conservative scan can still find dependency calls inside them.
    Unsupported {
        construct: &'static str,
        span: Span,
        exprs: Vec<Expr>,
        body: Vec<Stmt>,
    },
}

/// A converted template.
#[derive(Debug)]
pub(crate) struct Template {
    pub(crate) body: Vec<Stmt>,
}
