pub(crate) mod edge;
pub(crate) mod node;
pub(crate) mod scope;

use crate::types::TableLineage;
use edge::{EdgeKind, RawEdge};
use node::{NodeId, RawNode, StarBase, StarOptions};
use scope::{ScopeId, ScopeTree};

pub(crate) struct RawGraph {
    pub nodes: Vec<RawNode>,
    pub edges: Vec<RawEdge>,
    pub scopes: ScopeTree,
    pub tables: TableLineage,
}

impl RawGraph {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            scopes: ScopeTree::new(),
            tables: TableLineage::default(),
        }
    }

    pub fn add_node(&mut self, node: RawNode) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(node);
        id
    }

    pub fn add_output(&mut self, name: String, intrinsic_kind: EdgeKind) -> NodeId {
        self.add_node(RawNode::Output {
            name,
            intrinsic_kind,
        })
    }

    pub fn add_ref_with_binding(
        &mut self,
        name: String,
        qualifier: Option<String>,
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
        name: String,
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
        name: String,
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
