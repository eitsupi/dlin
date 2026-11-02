pub(crate) mod edge;
pub(crate) mod node;
pub(crate) mod scope;

use crate::Ident;
use crate::types::Dialect;
use crate::types::TableLineage;
use edge::{EdgeKind, RawEdge};
use node::{NodeId, RawNode, StarBase, StarOptions};
use scope::{ScopeId, ScopeTree};

pub(crate) struct RawGraph {
    pub dialect: Dialect,
    pub nodes: Vec<RawNode>,
    pub edges: Vec<RawEdge>,
    pub scopes: ScopeTree,
    pub tables: TableLineage,
}

impl RawGraph {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            nodes: Vec::new(),
            edges: Vec::new(),
            scopes: ScopeTree::new(dialect),
            tables: TableLineage::default(),
        }
    }

    pub fn add_node(&mut self, node: RawNode) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        id
    }

    pub fn add_output(&mut self, name: Ident, intrinsic_kind: EdgeKind) -> NodeId {
        self.add_node(RawNode::Output {
            name,
            intrinsic_kind,
        })
    }

    pub fn add_ref_with_binding(
        &mut self,
        name: Ident,
        qualifier: Option<Vec<Ident>>,
        scope: ScopeId,
        binding: Option<crate::graph::scope::Binding>,
    ) -> NodeId {
        self.add_node(RawNode::Ref {
            name,
            qualifier,
            scope,
            binding,
        })
    }

    pub fn add_unqualified_with_binding(
        &mut self,
        name: Ident,
        scope: ScopeId,
        binding: Option<crate::graph::scope::Binding>,
    ) -> NodeId {
        self.add_node(RawNode::Unqualified {
            name,
            scope,
            binding,
        })
    }

    pub fn add_row_value_candidate(
        &mut self,
        name: Ident,
        scope: ScopeId,
        binding: Option<crate::graph::scope::Binding>,
    ) -> NodeId {
        self.add_node(RawNode::RowValueCandidate {
            name,
            scope,
            binding,
        })
    }

    pub fn add_star(&mut self, base: StarBase, options: StarOptions, scope: ScopeId) -> NodeId {
        self.add_node(RawNode::Star {
            base,
            options,
            scope,
        })
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        self.edges.push(RawEdge {
            from,
            to,
            kind,
            is_recursive_back_edge: false,
        });
    }
}
