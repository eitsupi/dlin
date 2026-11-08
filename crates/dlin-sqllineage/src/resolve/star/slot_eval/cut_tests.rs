//! A value computed while a cycle was cut is never kept, so what an output
//! evaluates to does not depend on the order outputs are asked for.

use std::collections::HashSet;

use super::harness::{
    Pair, cte, derived_reading, evaluate, evaluate_limited, pair, parse, show, slot_ids,
};
use super::take_evaluated_slots;
use crate::graph::RawGraph;
use crate::graph::node::NodeId;
use crate::graph::scope::{OutputPlan, ScopeId, ScopeTree};
use crate::resolve::context::EvalKey;
use crate::resolve::lineage::collect_leaf_origins;
use crate::resolve::mapping::{Cut, resolve_scope_mappings};
use crate::resolve::model::{BranchTail, NameDomain, OutputElement};
use crate::{Dialect, Ident};

use super::super::Evaluator;

/// One thing to evaluate.
#[derive(Clone, Copy, Debug)]
enum Probe {
    /// The published column at an element index of a scope.
    Slot(ScopeId, usize),
    /// The leaf origins of a node.
    Node(NodeId),
    /// The mappings of a scope.
    Mappings(ScopeId),
}

/// What a probe evaluates to, in a form that can be compared.
fn run(ev: &mut Evaluator<'_, '_, '_>, probe: Probe) -> String {
    match probe {
        Probe::Slot(scope, index) => {
            let model = ev.scope_model(scope);
            let OutputElement::Column(slot) = &model.elements[index] else {
                return "segment".to_owned();
            };
            let (sources, transform) = ev.slot_value(slot.published());
            format!("{:?} {transform:?}", show(&sources))
        }
        Probe::Node(node) => {
            let (sources, has_back, transform) =
                collect_leaf_origins(node, ev.cx, &mut HashSet::new());
            format!("{:?} {has_back} {transform:?}", show(&sources))
        }
        Probe::Mappings(scope) => match resolve_scope_mappings(scope, ev.cx) {
            Ok(mappings) => mappings
                .iter()
                .map(|mapping| {
                    format!(
                        "{}#{:?} {:?} {:?}",
                        mapping.target.column.value,
                        mapping.ordinal,
                        show(&mapping.sources),
                        mapping.transform
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
            Err(Cut) => "cut".to_owned(),
        },
    }
}

/// Every ordering of `items`.
fn orderings<T: Copy>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut all = Vec::new();
    for index in 0..items.len() {
        let mut rest = items.to_vec();
        let first = rest.remove(index);
        for mut tail in orderings(&rest) {
            tail.insert(0, first);
            all.push(tail);
        }
    }
    all
}

/// Evaluate each probe alone, then all of them in every order in one context,
/// and require every probe to give the answer it gives alone.
fn assert_order_independent(graph: &RawGraph, probes: &[Probe]) {
    let alone: Vec<String> = probes
        .iter()
        .map(|probe| evaluate(graph, |ev| run(ev, *probe)))
        .collect();
    let indexes: Vec<usize> = (0..probes.len()).collect();
    for order in orderings(&indexes) {
        evaluate(graph, |ev| {
            for &index in &order {
                assert_eq!(
                    run(ev, probes[index]),
                    alone[index],
                    "{:?} after {:?}",
                    probes[index],
                    order
                        .iter()
                        .take_while(|&&seen| seen != index)
                        .map(|&seen| probes[seen])
                        .collect::<Vec<_>>()
                );
            }
        });
    }
}

fn pair_probes(pair: &Pair) -> Vec<Probe> {
    vec![
        Probe::Slot(pair.a, 0),
        Probe::Slot(pair.b, 0),
        Probe::Node(pair.candidates[0]),
        Probe::Node(pair.candidates[1]),
        Probe::Mappings(pair.a),
        Probe::Mappings(pair.b),
        Probe::Mappings(ScopeTree::root()),
    ]
}

#[test]
fn two_columns_that_read_each_other_are_unresolved_whichever_is_asked_first() {
    let pair = pair();
    // Asked for itself, each column finds the cycle at itself.
    evaluate(&pair.graph, |ev| {
        assert_eq!(run(ev, Probe::Slot(pair.a, 0)), "[\"?x\"] Direct");
        assert_eq!(run(ev, Probe::Slot(pair.b, 0)), "[\"?y\"] Direct");
    });
    evaluate(&pair.graph, |ev| {
        assert_eq!(run(ev, Probe::Slot(pair.b, 0)), "[\"?y\"] Direct");
        assert_eq!(run(ev, Probe::Slot(pair.a, 0)), "[\"?x\"] Direct");
    });
}

#[test]
fn no_cache_keeps_a_value_computed_under_a_cut() {
    let pair = pair();
    assert_order_independent(&pair.graph, &pair_probes(&pair));
}

#[test]
fn nothing_computed_under_a_cut_is_kept() {
    let pair = pair();
    evaluate(&pair.graph, |ev| {
        run(ev, Probe::Slot(pair.a, 0));
        run(ev, Probe::Mappings(ScopeTree::root()));
        assert!(ev.cx.slot_values.is_empty());
        for node in pair.candidates {
            assert!(ev.cx.resolved[node].is_none());
        }
        for scope in [pair.a, pair.b, ScopeTree::root()] {
            assert!(!ev.cx.mapping_cache.is_cached(scope));
        }
    });
}

#[test]
fn a_cut_is_unresolved_and_never_empty() {
    let pair = pair();
    evaluate(&pair.graph, |ev| {
        for probe in pair_probes(&pair) {
            let answer = run(ev, probe);
            assert!(!answer.contains("[]"), "{probe:?}: {answer}");
            assert!(answer.contains('?'), "{probe:?}: {answer}");
        }
    });
}

#[test]
fn a_column_outside_the_cycle_is_computed_once() {
    let mut pair = pair();
    let graph = &mut pair.graph;
    // `w` has no source, so the column is a plain, finished value.
    let (calm, _, _) = derived_reading(graph, "z", "w", None);
    evaluate(graph, |ev| {
        let id = super::harness::slot_ids(&ev.scope_model(calm))[0];
        take_evaluated_slots();
        ev.slot_value(id);
        assert_eq!(take_evaluated_slots(), [id]);
        ev.slot_value(id);
        assert!(take_evaluated_slots().is_empty());
        assert!(ev.cx.slot_values.contains_key(&id));
    });
}

// Segments

#[test]
fn a_segment_that_depends_on_itself_is_unresolved_and_not_kept() {
    let graph = parse(
        "WITH c AS (SELECT * FROM t) SELECT * FROM c",
        Dialect::Generic,
    );
    let scope = cte(&graph, "c");
    evaluate(&graph, |ev| {
        let model = ev.scope_model(scope);
        let OutputElement::Unknown(segment) = &model.elements[0] else {
            panic!("the CTE publishes an unknown segment");
        };
        let id = segment.published();
        let (a, b) = (Ident::unquoted("a"), Ident::unquoted("b"));
        // While the segment is being evaluated, it stands for unknown columns
        // and contributes an unresolved origin to a name.
        assert!(ev.cx.enter(EvalKey::Segment(id)));
        let mapping = ev
            .segment_mapping(id)
            .expect("a cut stands for unknown columns");
        assert_eq!(show(&mapping.sources), ["*?"]);
        assert_eq!(show(&ev.segment_sources(id, &a)), ["?a"]);
        ev.cx.leave(EvalKey::Segment(id));
        assert!(ev.cx.segment_mappings.is_empty());
        // Afterwards it is evaluated for real, and each name gets its own answer.
        let mapping = ev.segment_mapping(id).expect("the segment stands for t.*");
        assert_eq!(show(&mapping.sources), ["t.*"]);
        assert!(ev.cx.segment_mappings.contains_key(&id));
        for (name, expected) in [(&a, "t.*:a"), (&b, "t.*:b"), (&a, "t.*:a")] {
            assert_eq!(show(&ev.segment_sources(id, name)), [expected]);
        }
    });
}

// Mappings

const UNION_CTE: &str =
    "WITH RECURSIVE c AS (SELECT id FROM t UNION ALL SELECT id FROM u) SELECT id FROM c";

#[test]
fn mappings_that_are_being_computed_are_a_cut_and_not_an_empty_list() {
    let graph = parse(UNION_CTE, Dialect::Generic);
    let union = cte(&graph, "c");
    assert!(matches!(
        graph.scopes.output_plan(union),
        OutputPlan::SetOperation { .. }
    ));
    evaluate(&graph, |ev| {
        let model = ev.scope_model(union);
        let publication = model.publication.expect("a scope's model is published");
        let slot = slot_ids(&model)[0];
        assert!(ev.cx.enter(EvalKey::Mappings(union)));
        // Every way a set operation's mappings are read reports the cut.
        assert_eq!(resolve_scope_mappings(union, ev.cx).err(), Some(Cut));
        let (sources, _) = ev.slot_value(slot);
        assert_eq!(show(&sources), ["?id"]);
        assert_eq!(
            show(&ev.tail_column(union, publication, &Ident::unquoted("id"))),
            ["?id"]
        );
        let tail = BranchTail {
            scope: union,
            publication,
            ranges: std::rc::Rc::from([]),
            domain: NameDomain::default(),
            offset: Some(0),
        };
        let mappings = ev.tail_mappings(&tail);
        assert_eq!(mappings.len(), 1);
        assert_eq!(show(&mappings[0].sources), ["*?"]);
        ev.cx.leave(EvalKey::Mappings(union));
    });
}

#[test]
fn cycles_through_set_operations_delegates_and_tails_do_not_depend_on_the_start() {
    for sql in [
        "WITH RECURSIVE a AS (SELECT id FROM b UNION ALL SELECT id FROM t), \
         b AS (SELECT id FROM a) SELECT id FROM a",
        "WITH RECURSIVE a AS (SELECT * FROM b UNION ALL SELECT * FROM t), \
         b AS (SELECT * FROM a) SELECT * FROM a",
        "WITH RECURSIVE a AS (SELECT * FROM t UNION ALL SELECT * FROM b), \
         b AS (SELECT * FROM a) SELECT * FROM b",
        "WITH RECURSIVE a AS (SELECT * FROM b UNION ALL SELECT x FROM t), \
         b AS (SELECT * FROM a) SELECT * FROM a",
        "WITH RECURSIVE a AS (SELECT id FROM b), b AS (SELECT id FROM a) SELECT id FROM a",
        "WITH RECURSIVE a AS (SELECT x FROM b UNION ALL SELECT x FROM t), \
         b AS (SELECT x FROM a UNION ALL SELECT x FROM u) SELECT * FROM a",
        // A set operation whose operand delegates to a body that reads the
        // set operation through another CTE.
        "WITH RECURSIVE a AS ((WITH z AS (SELECT 1 AS k) SELECT id FROM b) \
         UNION ALL SELECT id FROM t), b AS (SELECT id FROM a) SELECT id FROM a",
        "WITH RECURSIVE a AS (SELECT id FROM t UNION ALL \
         (WITH z AS (SELECT 1 AS k) SELECT id FROM b)), b AS (SELECT id FROM a) SELECT id FROM a",
        "WITH RECURSIVE a AS ((WITH z AS (SELECT 1 AS k) SELECT * FROM b) \
         UNION ALL SELECT * FROM t), b AS (SELECT * FROM a) SELECT * FROM a",
    ] {
        let graph = parse(sql, Dialect::Generic);
        let mut probes = Vec::new();
        for scope in 0..graph.scopes.scope_count() {
            probes.push(Probe::Mappings(scope));
            let width = evaluate(&graph, |ev| ev.scope_model(scope).elements.len());
            probes.extend((0..width).map(|index| Probe::Slot(scope, index)));
        }
        assert_order_independent_in_rotations(&graph, &probes, sql);
    }
}

/// The probes in forward and reverse order, each rotated through every start.
fn assert_order_independent_in_rotations(graph: &RawGraph, probes: &[Probe], context: &str) {
    let alone: Vec<String> = probes
        .iter()
        .map(|probe| evaluate(graph, |ev| run(ev, *probe)))
        .collect();
    let forward: Vec<usize> = (0..probes.len()).collect();
    let reverse: Vec<usize> = forward.iter().rev().copied().collect();
    for base in [forward, reverse] {
        for start in 0..base.len() {
            let order: Vec<usize> = base[start..]
                .iter()
                .chain(&base[..start])
                .copied()
                .collect();
            evaluate(graph, |ev| {
                for &index in &order {
                    assert_eq!(
                        run(ev, probes[index]),
                        alone[index],
                        "{context}: {:?}",
                        probes[index]
                    );
                }
            });
        }
    }
}

#[test]
fn a_set_operation_segment_is_unknown_columns_while_its_mappings_are_computed() {
    let sql = "WITH c AS (SELECT * FROM t UNION ALL SELECT * FROM u) SELECT * FROM c";
    let graph = parse(sql, Dialect::Generic);
    let union = cte(&graph, "c");
    evaluate(&graph, |ev| {
        let model = ev.scope_model(union);
        let segment = model
            .elements
            .iter()
            .find_map(|element| match element {
                OutputElement::Unknown(segment) => Some(segment.published()),
                _ => None,
            })
            .expect("the set operation publishes an unknown segment");
        assert!(ev.cx.enter(EvalKey::Mappings(union)));
        let mapping = ev
            .segment_mapping(segment)
            .expect("a cut stands for unknown columns");
        assert_eq!(show(&mapping.sources), ["*?"]);
        ev.cx.leave(EvalKey::Mappings(union));
        assert!(ev.cx.segment_mappings.is_empty());
        let mapping = ev
            .segment_mapping(segment)
            .expect("the segment stands for columns");
        assert!(show(&mapping.sources).contains(&"t.*".to_owned()));
    });
}

#[test]
fn a_spent_budget_is_a_cut_and_not_an_empty_answer() {
    let pair = pair();
    evaluate_limited(&pair.graph, 0, |ev| {
        assert_eq!(
            resolve_scope_mappings(ScopeTree::root(), ev.cx).err(),
            Some(Cut)
        );
        let id = slot_ids(&ev.scope_model(pair.a))[0];
        let (sources, _) = ev.slot_value(id);
        assert_eq!(show(&sources), ["?x"]);
        assert!(ev.cx.slot_values.is_empty());
    });
}

#[test]
fn a_column_read_through_two_ranges_is_evaluated_once() {
    let sql = "WITH c AS (SELECT t.a AS x, t.b AS y FROM t) SELECT * FROM c c1, c c2";
    let graph = parse(sql, Dialect::Generic);
    let child = cte(&graph, "c");
    let OutputPlan::Delegate(body) = *graph.scopes.output_plan(ScopeTree::root()) else {
        panic!("the root delegates to its body");
    };
    evaluate(&graph, |ev| {
        let child = slot_ids(&ev.scope_model(child));
        let body = slot_ids(&ev.scope_model(body));
        take_evaluated_slots();
        for id in &body {
            ev.slot_value(*id);
        }
        let evaluated = take_evaluated_slots();
        for id in &child {
            assert_eq!(evaluated.iter().filter(|seen| *seen == id).count(), 1);
        }
    });
}
