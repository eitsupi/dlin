use crate::Ident;
use crate::graph::edge::EdgeKind;
use crate::graph::scope::{Binding, BindingEnv};
pub(crate) type NodeId = usize;

/// The expression which a wildcard expands from. Keeping the original parts
/// lets resolution distinguish a relation prefix from a nested field path.
#[derive(Debug, Clone)]
pub(crate) enum StarBase {
    Unqualified,
    Qualified(Vec<Ident>),
    Expr(Vec<NodeId>),
}

#[derive(Debug, Clone)]
pub(crate) struct StarColumnName {
    pub parts: Vec<Ident>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StarOptions {
    pub exclude: Vec<StarColumnName>,
    pub ilike: Option<String>,
    pub replace: Vec<StarReplacement>,
    pub rename: Vec<(Ident, Ident)>,
}

#[derive(Debug, Clone)]
pub(crate) struct StarReplacement {
    pub column: Ident,
    pub node_id: NodeId,
}

#[derive(Debug, Clone)]
pub(crate) enum RawNode {
    /// Output column — produced by a projection or assignment.
    Output {
        name: Ident,
        /// The edge kind the defining expression would carry to its own
        /// ancestors, kept even when it has none (e.g. `COUNT(*)` has no
        /// column ancestor but is still an aggregate). Used as a fallback
        /// classification when no ancestor edge exists to classify from.
        intrinsic_kind: EdgeKind,
    },
    /// Named reference — alias, CTE reference, derived table column.
    Ref {
        name: Ident,
        qualifier: Option<Vec<Ident>>,
        env: BindingEnv,
        /// Binding captured while building a FROM expression. This prevents
        /// a later table alias from changing the meaning of a lateral
        /// dependency (for example `base, UNNEST(base.items) AS base`).
        binding: Option<Binding>,
    },
    /// SELECT * or table.* — expandable with catalog.
    Star {
        base: StarBase,
        options: StarOptions,
        env: BindingEnv,
    },
    /// Unqualified column in multi-table scope.
    Unqualified {
        name: Ident,
        env: BindingEnv,
        /// Binding visible while the expression was built. This keeps a
        /// lateral FROM dependency attached to the preceding relation even
        /// when a later range variable shadows its name.
        binding: Option<Binding>,
    },
    /// A relation alias used where the dialect permits a whole-row value.
    /// Resolution uses the SQL dialect's precedence for a competing column.
    RowValueCandidate {
        name: Ident,
        env: BindingEnv,
        binding: Option<Binding>,
    },
    /// An expression the engine does not model, which may still depend on
    /// columns: its value is unresolved, never a source-free value.
    Unsupported { what: &'static str },
}
