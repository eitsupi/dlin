use crate::graph::edge::EdgeKind;
use crate::graph::scope::{Binding, ScopeId};
pub(crate) type NodeId = usize;

/// The expression which a wildcard expands from. Keeping the original parts
/// lets resolution distinguish a relation prefix from a nested field path.
#[derive(Debug, Clone)]
pub(crate) enum StarBase {
    Unqualified,
    Qualified(Vec<String>),
    Expr(Vec<NodeId>),
}

#[derive(Debug, Clone)]
pub(crate) struct StarColumnName {
    pub parts: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StarOptions {
    pub exclude: Vec<StarColumnName>,
    pub ilike: Option<String>,
    pub replace: Vec<StarReplacement>,
    pub rename: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub(crate) struct StarReplacement {
    pub column: String,
    pub node_id: NodeId,
}

#[derive(Debug, Clone)]
pub(crate) enum RawNode {
    /// Output column — produced by a projection or assignment.
    Output {
        name: String,
        /// The edge kind the defining expression would carry to its own
        /// ancestors, kept even when it has none (e.g. `COUNT(*)` has no
        /// column ancestor but is still an aggregate). Used as a fallback
        /// classification when no ancestor edge exists to classify from.
        intrinsic_kind: EdgeKind,
    },
    /// Named reference — alias, CTE reference, derived table column.
    Ref {
        name: String,
        qualifier: Option<String>,
        scope: ScopeId,
        /// Binding captured while building a FROM expression. This prevents
        /// a later table alias from changing the meaning of a lateral
        /// dependency (for example `base, UNNEST(base.items) AS base`).
        binding: Option<Binding>,
    },
    /// SELECT * or table.* — expandable with catalog.
    Star {
        base: StarBase,
        options: StarOptions,
        scope: ScopeId,
    },
    /// Unqualified column in multi-table scope.
    Unqualified {
        name: String,
        scope: ScopeId,
        /// Binding visible while the expression was built. This keeps a
        /// lateral FROM dependency attached to the preceding relation even
        /// when a later range variable shadows its name.
        binding: Option<Binding>,
    },
    /// A relation alias used where the dialect permits a whole-row value.
    /// Resolution must distinguish this from a source-free expression: a
    /// catalog or derived scope can still prove that the alias is an ordinary
    /// physical/output column with the same name.
    RowValueCandidate {
        name: String,
        scope: ScopeId,
        binding: Option<Binding>,
    },
}
