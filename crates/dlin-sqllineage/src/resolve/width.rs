//! Output widths read from the output models: whether every star of a
//! statement can be expanded, and whether the branches of a set operation have
//! the same number of columns.

use std::collections::HashSet;

use super::model::OutputModels;

use crate::graph::RawGraph;
use crate::graph::node::RawNode;
use crate::graph::scope::{OutputPlan, ScopeTree, SetCorrespondence};
use crate::types::SemanticError;

/// Inspect the complete graph rather than only the root projection. A star
/// under a JOIN/CTE/derived-table boundary is still an unresolved schema
/// dependency and must be visible to consumers of the public result.
pub(super) fn graph_has_unresolved_stars(
    graph: &RawGraph,
    models: &mut OutputModels<'_, '_, '_>,
) -> bool {
    (0..graph.nodes.len()).any(|node_id| {
        matches!(graph.nodes[node_id], RawNode::Star { .. })
            && models.star(node_id).has_unknown_segment()
    })
}

pub(super) fn validate_set_arities(
    models: &mut OutputModels<'_, '_, '_>,
) -> Result<Option<usize>, SemanticError> {
    let mut active = HashSet::new();
    scope_arity(ScopeTree::root(), models, &mut active)
}

/// Return an exact output width when every star in a scope can be expanded;
/// otherwise return `None` and leave the eventual merge conservative.
fn scope_arity(
    scope: usize,
    models: &mut OutputModels<'_, '_, '_>,
    active: &mut HashSet<usize>,
) -> Result<Option<usize>, SemanticError> {
    let graph = models.graph();
    if graph.scopes.output_width_unknown(scope) {
        return Ok(None);
    }
    if !active.insert(scope) {
        return Ok(None);
    }
    let result = match graph.scopes.output_plan(scope).clone() {
        OutputPlan::Projection => models.scope(scope).width.exact(),
        OutputPlan::Delegate(child) => scope_arity(child, models, active)?,
        OutputPlan::SetOperation {
            left,
            right,
            correspondence,
            ..
        } => {
            let left_width = scope_arity(left, models, active)?;
            let right_width = scope_arity(right, models, active)?;
            if correspondence == SetCorrespondence::ByName {
                None
            } else {
                match (left_width, right_width) {
                    (Some(left), Some(right)) if left != right => {
                        return Err(SemanticError::SetOperationArityMismatch { left, right });
                    }
                    // Validation inspects both operands, but the output
                    // width belongs to the same model that publishes its
                    // names and slots. An unknown right width does not add
                    // output slots beyond an exhaustive left operand.
                    _ => models.scope(scope).width.exact(),
                }
            }
        }
    };
    active.remove(&scope);
    Ok(result)
}
