use std::collections::VecDeque;

use crate::graph::edge::RawEdge;
use crate::graph::node::{NodeId, RawNode};

pub(crate) struct CycleError;

/// Topological sort via Kahn's algorithm on forward edges only.
pub(crate) fn topological_sort(
    nodes: &[RawNode],
    edges: &[RawEdge],
) -> Result<Vec<NodeId>, CycleError> {
    let n = nodes.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<NodeId>> = vec![vec![]; n];

    for edge in edges {
        if !edge.is_recursive_back_edge {
            in_degree[edge.to] += 1;
            adj[edge.from].push(edge.to);
        }
    }

    let mut queue: VecDeque<NodeId> = in_degree
        .iter()
        .enumerate()
        .filter(|(_, d)| **d == 0)
        .map(|(i, _)| i)
        .collect();

    let mut order = Vec::with_capacity(n);

    while let Some(node) = queue.pop_front() {
        order.push(node);
        for &next in &adj[node] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push_back(next);
            }
        }
    }

    if order.len() != n {
        return Err(CycleError);
    }

    Ok(order)
}
