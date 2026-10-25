use crate::graph::node::NodeId;

#[derive(Debug, Clone)]
pub(crate) struct RawEdge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    pub is_recursive_back_edge: bool,
}

#[derive(Debug, Clone)]
pub(crate) enum EdgeKind {
    Direct,
    ViaExpression,
    ViaAggregation,
    ViaConditional,
}
