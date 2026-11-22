pub(crate) mod edge;
pub(crate) mod node;
pub(crate) mod scope;

use crate::Ident;
use crate::types::Dialect;
use crate::types::TableLineage;
use edge::{EdgeKind, RawEdge};
use node::{NodeId, RawNode, StarBase, StarOptions};
use scope::{BindingEnv, ScopeTree};
use std::collections::HashMap;

pub(crate) struct RawGraph {
    pub dialect: Dialect,
    pub nodes: Vec<RawNode>,
    pub edges: Vec<RawEdge>,
    pub target_columns: Vec<Ident>,
    pub output_names: HashMap<NodeId, crate::OutputName>,
    /// What decides the fields of an output column that may be a STRUCT. A
    /// star over the column expands these.
    pub struct_values: HashMap<NodeId, StructValue>,
    pub empty_projection: Option<crate::Diagnostic>,
    pub scopes: ScopeTree,
    pub tables: TableLineage,
}

/// How the value of an output column gets its STRUCT fields.
#[derive(Debug, Clone)]
pub(crate) enum StructValue {
    /// A STRUCT built in place: its fields in order, each with the node of
    /// its value.
    Fields(Vec<(Ident, NodeId)>),
    /// A column read as it is, through this reference node: its fields are
    /// those of the column the reference names.
    Column(NodeId),
}

impl RawGraph {
    pub fn new(dialect: Dialect) -> Self {
        Self {
            dialect,
            nodes: Vec::new(),
            edges: Vec::new(),
            output_names: HashMap::new(),
            struct_values: HashMap::new(),
            target_columns: Vec::new(),
            empty_projection: None,
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
        let output_name = crate::OutputName::Explicit(name.clone());
        let id = self.add_node(RawNode::Output {
            name,
            intrinsic_kind,
        });
        self.output_names.insert(id, output_name);
        id
    }

    pub fn set_output_name(&mut self, node_id: NodeId, output_name: crate::OutputName) {
        self.output_names.insert(node_id, output_name);
    }

    pub fn add_ref_with_binding(
        &mut self,
        name: Ident,
        qualifier: Option<Vec<Ident>>,
        env: BindingEnv,
        binding: Option<crate::graph::scope::Binding>,
    ) -> NodeId {
        self.add_node(RawNode::Ref {
            name,
            qualifier,
            env,
            binding,
        })
    }

    pub fn add_unqualified_with_binding(
        &mut self,
        name: Ident,
        env: BindingEnv,
        binding: Option<crate::graph::scope::Binding>,
    ) -> NodeId {
        self.add_node(RawNode::Unqualified { name, env, binding })
    }

    pub fn add_row_value_candidate(
        &mut self,
        name: Ident,
        env: BindingEnv,
        binding: Option<crate::graph::scope::Binding>,
    ) -> NodeId {
        self.add_node(RawNode::RowValueCandidate { name, env, binding })
    }

    pub fn add_unsupported(&mut self, what: &'static str) -> NodeId {
        self.add_node(RawNode::Unsupported { what })
    }

    pub fn add_star(&mut self, base: StarBase, options: StarOptions, env: BindingEnv) -> NodeId {
        self.add_node(RawNode::Star { base, options, env })
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
