#[cfg(test)]
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::catalog;
use super::context::{EvalContext, EvalKey};
use super::model::{ModelStore, OutputModel, OutputModels};
use super::setop_legacy::{merge_operands, retag};
use super::star::projection_mappings;

use crate::Ident;
use crate::graph::RawGraph;
use crate::graph::node::{NodeId, RawNode};
use crate::graph::scope::{Binding, OutputPlan, ScopeId, VirtualColumnState};
use crate::types::{ColumnMapping, ColumnOrigin, ColumnRef, TableRef, TransformKind};

pub(super) fn wildcard_mapping(
    output_table: Option<&TableRef>,
    source_table: TableRef,
) -> ColumnMapping {
    ColumnMapping {
        star_position: crate::types::StarPosition::Inferred,
        output_name: None,
        ordinal: None,
        target: ColumnRef {
            table: output_table.cloned(),
            column: Ident::unquoted("*"),
        },
        sources: vec![ColumnOrigin::Wildcard {
            table: source_table,
        }],
        transform: TransformKind::Direct,
        slot: None,
        unpublished_tail: false,
        merged_in: false,
    }
}

pub(super) fn known_empty_dependency(node_id: NodeId, graph: &RawGraph) -> bool {
    let (name, binding) = match &graph.nodes[node_id] {
        RawNode::Ref { name, binding, .. } | RawNode::Unqualified { name, binding, .. } => {
            (name, binding.as_ref())
        }
        _ => return false,
    };
    let Some(Binding::VirtualSource(source)) = binding else {
        return false;
    };
    let Some(column) = graph
        .scopes
        .virtual_source(*source)
        .columns
        .iter()
        .find(|column| column.name.column_key(graph.dialect) == name.column_key(graph.dialect))
    else {
        return false;
    };
    matches!(column.state, VirtualColumnState::KnownEmpty)
        && column
            .dependencies
            .iter()
            .all(|&dependency| known_empty_dependency(dependency, graph))
}

#[derive(Default)]
pub(super) struct ScopeMappingCache {
    /// The mappings of each published model of a scope, by publication.
    entries: HashMap<u32, ScopeMappingEntry>,
    /// The structural models mappings are evaluated from.
    pub(super) models: ModelStore,
}

#[cfg(test)]
impl ScopeMappingCache {
    pub(super) fn is_cached(&self, scope: usize) -> bool {
        self.models
            .cached(scope)
            .and_then(|model| model.publication)
            .is_some_and(|publication| self.entries.contains_key(&publication))
    }
}

/// Mappings already computed for a scope. A scope still being computed is on
/// the evaluation stack instead, so an entry here is always complete.
type ScopeMappingEntry = Arc<[ColumnMapping]>;

#[cfg(test)]
thread_local! {
    static SCOPE_MAPPING_COMPUTATIONS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
pub(super) fn reset_scope_mapping_stats() {
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(0));
}

#[cfg(test)]
pub(super) fn scope_mapping_computations() -> usize {
    SCOPE_MAPPING_COMPUTATIONS.with(Cell::get)
}

/// The mappings of a scope that is being computed, or that the budget no
/// longer allows computing, cannot be known. This is not an empty list of
/// mappings: a caller has to turn it into an unresolved value of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cut;

/// Resolve a scope's output plan into ordered mappings. Set-operation
/// branches are resolved independently so catalog expansion happens before
/// their positional merge.
///
/// A result computed while a cycle was cut depends on where the walk started
/// and is returned without being kept.
pub(super) fn resolve_scope_mappings(
    scope: ScopeId,
    cx: &mut EvalContext<'_, '_>,
) -> Result<Arc<[ColumnMapping]>, Cut> {
    let view = models(cx).scope(scope);
    mappings_of(scope, &view, cx)
}

/// The mappings of `scope` for one publication of its model. The mappings
/// carry the identities that publication gave its outputs, so a reader that
/// holds one of them reads the mappings of the model it read.
pub(super) fn mappings_of(
    scope: ScopeId,
    view: &Rc<OutputModel>,
    cx: &mut EvalContext<'_, '_>,
) -> Result<Arc<[ColumnMapping]>, Cut> {
    let publication = view
        .publication
        .expect("the mappings of a scope are those of a published model");
    if let Some(mappings) = cx.mapping_cache.entries.get(&publication) {
        return Ok(mappings.clone());
    }
    let generation = cx.cut_generation();
    if !cx.enter(EvalKey::Mappings(scope)) {
        return Err(Cut);
    }
    #[cfg(test)]
    SCOPE_MAPPING_COMPUTATIONS.with(|computations| computations.set(computations.get() + 1));
    let computed = compute_scope_mappings(scope, view, cx);
    cx.leave(EvalKey::Mappings(scope));
    let mappings: Arc<[ColumnMapping]> = match computed {
        Ok(mappings) => mappings.into(),
        Err(cut) => {
            // A forwarding invariant can fail without an entry being refused.
            // Make every failed mapping walk visible to caches above it.
            cx.record_cut();
            return Err(cut);
        }
    };
    if cx.cut_generation() == generation {
        cx.mapping_cache
            .entries
            .insert(publication, mappings.clone());
    }
    Ok(mappings)
}

pub(super) fn models<'x, 'a, 'c>(cx: &'x mut EvalContext<'a, 'c>) -> OutputModels<'a, 'c, 'x> {
    OutputModels::new(cx.graph, cx.catalog, &mut cx.mapping_cache.models)
}

/// The publication of `operand` that `reader` forwarded, whose outputs the
/// operand's mappings have to be tagged with to be read for `reader`.
pub(super) fn operand_view(
    reader: &OutputModel,
    operand: ScopeId,
    cx: &mut EvalContext<'_, '_>,
) -> Rc<OutputModel> {
    match reader.forwarded.first() {
        Some(pair) => {
            let publication = cx.mapping_cache.models.arena.publication(pair.from);
            models(cx).view(publication)
        }
        None => models(cx).scope(operand),
    }
}

fn compute_scope_mappings(
    scope: ScopeId,
    view: &Rc<OutputModel>,
    cx: &mut EvalContext<'_, '_>,
) -> Result<Vec<ColumnMapping>, Cut> {
    let graph = cx.graph;
    let mappings = match graph.scopes.output_plan(scope).clone() {
        OutputPlan::Projection => {
            let mut mappings = projection_mappings(scope, view, cx);
            if let Some(cat) = cx.catalog {
                catalog::apply_catalog(&mut mappings, cat);
            }
            mappings
        }
        OutputPlan::Delegate(child) => {
            let child_view = operand_view(view, child, cx);
            let mut mappings = mappings_of(child, &child_view, cx)?.to_vec();
            retag(&mut mappings, view)?;
            mappings
        }
        OutputPlan::SetOperation {
            left,
            right,
            recursive,
        } => merge_operands(view, left, right, recursive, cx)?,
    };
    Ok(mappings)
}

pub(super) fn merge_transform(left: &TransformKind, right: &TransformKind) -> TransformKind {
    if matches!(left, TransformKind::Aggregation) || matches!(right, TransformKind::Aggregation) {
        TransformKind::Aggregation
    } else if matches!(left, TransformKind::Conditional)
        || matches!(right, TransformKind::Conditional)
    {
        TransformKind::Conditional
    } else if matches!(left, TransformKind::Expression)
        || matches!(right, TransformKind::Expression)
    {
        TransformKind::Expression
    } else if matches!(left, TransformKind::Window) || matches!(right, TransformKind::Window) {
        TransformKind::Window
    } else if matches!(left, TransformKind::Unknown) || matches!(right, TransformKind::Unknown) {
        TransformKind::Unknown
    } else {
        TransformKind::Direct
    }
}
