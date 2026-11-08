//! A reader that forwards a published column evaluates that column.

use super::harness::{cte, evaluate, parse, show, slot_ids};
use super::take_evaluated_slots;
use crate::graph::scope::{OutputPlan, ScopeTree};
use crate::resolve::mapping::{Cut, resolve_scope_mappings};
use crate::resolve::model::{OutputElement, OutputRef, ValueId};
use crate::resolve::setop_legacy::retag;
use crate::{Dialect, Ident};

const DUPLICATES: &str = "WITH c AS (SELECT t.a AS x, t.b AS x FROM t) SELECT * FROM c";

#[test]
fn star_and_delegate_readers_forward_the_column_in_the_same_position() {
    let graph = parse(DUPLICATES, Dialect::Generic);
    let child = cte(&graph, "c");
    evaluate(&graph, |ev| {
        // The root delegates to the query body, which reads the CTE through a star.
        let OutputPlan::Delegate(body) = *graph.scopes.output_plan(ScopeTree::root()) else {
            panic!("the root delegates to its body");
        };
        let root = slot_ids(&ev.scope_model(ScopeTree::root()));
        let body = slot_ids(&ev.scope_model(body));
        let child = slot_ids(&ev.scope_model(child));
        assert_eq!((root.len(), body.len(), child.len()), (2, 2, 2));
        let arena = &ev.cx.mapping_cache.models.arena;
        for index in 0..2 {
            assert!(
                matches!(arena.slot(root[index]).value, ValueId::Forward(id) if id == body[index])
            );
            assert!(
                matches!(arena.slot(body[index]).value, ValueId::Forward(id) if id == child[index])
            );
        }
        // Each reader column evaluates the CTE column it forwards, though the
        // two share a name.
        for (index, expected) in [(0, "t.a"), (1, "t.b")] {
            take_evaluated_slots();
            let (sources, _) = ev.slot_value(root[index]);
            assert_eq!(show(&sources), [expected]);
            assert_eq!(
                take_evaluated_slots(),
                [root[index], body[index], child[index]]
            );
        }
    });
}

#[test]
fn a_delegating_scope_records_which_output_it_forwards() {
    let graph = parse(DUPLICATES, Dialect::Generic);
    evaluate(&graph, |ev| {
        let OutputPlan::Delegate(body) = *graph.scopes.output_plan(ScopeTree::root()) else {
            panic!("the root delegates to its body");
        };
        let root = ev.scope_model(ScopeTree::root());
        let body = slot_ids(&ev.scope_model(body));
        let pairs: Vec<_> = root
            .forwarded
            .iter()
            .map(|pair| (pair.from, pair.to))
            .collect();
        let expected: Vec<_> = slot_ids(&root)
            .into_iter()
            .zip(&body)
            .map(|(own, body)| (OutputRef::Slot(*body), OutputRef::Slot(own)))
            .collect();
        assert_eq!(pairs, expected);
    });
}

#[test]
fn a_projection_alias_is_resolved_by_name_once_and_then_followed_by_id() {
    let sql = "WITH c AS (SELECT t.a AS x, t.b AS y FROM t) SELECT y AS a, x AS b FROM c";
    let graph = parse(sql, Dialect::Generic);
    let child = cte(&graph, "c");
    evaluate(&graph, |ev| {
        let OutputPlan::Delegate(body) = *graph.scopes.output_plan(ScopeTree::root()) else {
            panic!("the root delegates to its body");
        };
        let body = ev.scope_model(body);
        let child = ev.scope_model(child);
        let [child_x, child_y] = slot_ids(&child)[..] else {
            panic!("the CTE lists two columns");
        };
        // The aliases are expressions of the body, not forwards.
        for element in &body.elements {
            let OutputElement::Column(slot) = element else {
                panic!("expected columns");
            };
            assert!(matches!(slot.value, ValueId::Expr(_)), "{:?}", slot.value);
        }
        let [a, b] = slot_ids(&body)[..] else {
            panic!("the body lists two columns");
        };
        take_evaluated_slots();
        let (sources, _) = ev.slot_value(a);
        assert_eq!(show(&sources), ["t.b"]);
        let evaluated = take_evaluated_slots();
        assert!(evaluated.contains(&child_y), "{evaluated:?}");
        assert!(!evaluated.contains(&child_x), "{evaluated:?}");
        let (sources, _) = ev.slot_value(b);
        assert_eq!(show(&sources), ["t.a"]);
        assert!(take_evaluated_slots().contains(&child_x));
    });
}

#[test]
fn two_ranges_over_one_cte_forward_the_same_columns() {
    let sql = "WITH c AS (SELECT t.a AS x, t.b AS y FROM t) SELECT * FROM c c1, c c2";
    let graph = parse(sql, Dialect::Generic);
    let child = cte(&graph, "c");
    evaluate(&graph, |ev| {
        let OutputPlan::Delegate(body) = *graph.scopes.output_plan(ScopeTree::root()) else {
            panic!("the root delegates to its body");
        };
        let body = slot_ids(&ev.scope_model(body));
        let child = slot_ids(&ev.scope_model(child));
        assert_eq!(body.len(), 4);
        let arena = &ev.cx.mapping_cache.models.arena;
        for (index, id) in body.iter().enumerate() {
            assert!(matches!(
                arena.slot(*id).value,
                ValueId::Forward(target) if target == child[index % 2]
            ));
        }
        let rendered: Vec<_> = body
            .iter()
            .map(|id| show(&ev.slot_value(*id).0).concat())
            .collect();
        assert_eq!(rendered, ["t.a", "t.b", "t.a", "t.b"]);
    });
}

#[test]
fn a_renamed_column_keeps_the_column_it_was_renamed_from() {
    let sql =
        "WITH c AS (SELECT t.a AS a, t.b AS b FROM t) SELECT * RENAME (b AS a, a AS b) FROM c";
    let graph = parse(sql, Dialect::Snowflake);
    let child = cte(&graph, "c");
    evaluate(&graph, |ev| {
        let OutputPlan::Delegate(body) = *graph.scopes.output_plan(ScopeTree::root()) else {
            panic!("the root delegates to its body");
        };
        let body = ev.scope_model(body);
        let child = slot_ids(&ev.scope_model(child));
        let names: Vec<_> = body
            .elements
            .iter()
            .map(|element| match element {
                OutputElement::Column(slot) => (slot.name.clone(), slot.published()),
                _ => panic!("expected columns"),
            })
            .collect();
        // The names swapped; each column still forwards the one it was.
        assert_eq!(names[0].0, Ident::unquoted("b"));
        assert_eq!(names[1].0, Ident::unquoted("a"));
        let arena = &ev.cx.mapping_cache.models.arena;
        assert!(matches!(arena.slot(names[0].1).value, ValueId::Forward(id) if id == child[0]));
        assert!(matches!(arena.slot(names[1].1).value, ValueId::Forward(id) if id == child[1]));
        // The column now called `a` is the CTE's `b`.
        assert_eq!(show(&ev.slot_value(names[1].1).0), ["t.b"]);
    });
}

#[test]
fn a_set_operation_forwards_its_left_operand_under_its_own_scope() {
    let sql = "WITH s AS (SELECT t.a AS x FROM t UNION ALL SELECT u.b FROM u) SELECT * FROM s";
    let graph = parse(sql, Dialect::Generic);
    let op = cte(&graph, "s");
    let OutputPlan::SetOperation { left, .. } = *graph.scopes.output_plan(op) else {
        panic!("the CTE is a set operation");
    };
    evaluate(&graph, |ev| {
        let merged = ev.scope_model(op);
        let left_slot = slot_ids(&ev.scope_model(left))[0];
        let merged_slot = slot_ids(&merged)[0];
        let arena = &ev.cx.mapping_cache.models.arena;
        // The operation, not the operand it forwards, is the merge's scope.
        let def = arena.slot(merged_slot);
        assert_eq!(def.owner, op);
        assert!(matches!(
            def.value,
            ValueId::SetMerged { op: scope, left: id } if scope == op && id == left_slot
        ));
        assert_eq!(
            merged
                .forwarded
                .iter()
                .map(|pair| (pair.from, pair.to))
                .collect::<Vec<_>>(),
            [(OutputRef::Slot(left_slot), OutputRef::Slot(merged_slot))]
        );
        // What the merge stands for is the left column itself; the right
        // operand's contribution is the operation's column.
        assert_eq!(show(&ev.value(&def.value).0), ["t.a", "u.b"]);
        assert_eq!(show(&ev.slot_value(merged_slot).0), ["t.a", "u.b"]);
    });
}

#[test]
fn retagging_keeps_unique_published_refs_and_marks_only_tails_unpublished() {
    for sql in [
        "WITH a AS (SELECT t.a AS x, t.b AS x FROM t UNION ALL SELECT u.a, u.b FROM u), \
         b AS (SELECT * FROM a) SELECT * FROM b",
        "WITH s AS (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2), \
         d AS (SELECT * FROM s) SELECT * FROM d",
    ] {
        let graph = parse(sql, Dialect::Generic);
        let op = cte(&graph, if sql.contains("WITH a AS") { "a" } else { "s" });
        evaluate(&graph, |ev| {
            let model = ev.scope_model(op);
            let mappings = resolve_scope_mappings(op, ev.cx).expect("mappings resolve");
            let published: Vec<_> = mappings.iter().filter_map(|mapping| mapping.slot).collect();
            for (index, output) in published.iter().enumerate() {
                assert!(
                    published[..index].iter().all(|earlier| earlier != output),
                    "duplicate published identity: {output:?}"
                );
                assert_eq!(ev.cx.mapping_cache.models.arena.owner(*output), op);
            }
            for mapping in mappings.iter() {
                if mapping.slot.is_none() {
                    assert!(mapping.unpublished_tail);
                }
            }
            for pair in &model.forwarded {
                if matches!(pair.to, OutputRef::Slot(_)) {
                    assert_eq!(
                        mappings
                            .iter()
                            .filter(|mapping| mapping.slot == Some(pair.to))
                            .count(),
                        1,
                        "each published column has one mapping"
                    );
                }
            }

            let mut invalid = mappings.to_vec();
            if let Some(mapping) = invalid.first_mut() {
                let published = mapping.slot.expect("the scope publishes this mapping");
                mapping.slot = None;
                mapping.unpublished_tail = false;
                assert_eq!(retag(&mut invalid, &model).err(), Some(Cut));

                let mut missing = mappings.to_vec();
                missing[0].slot = Some(published);
                assert_eq!(retag(&mut missing, &model).err(), Some(Cut));

                let mut duplicate = vec![mappings[0].clone(), mappings[0].clone()];
                assert_eq!(retag(&mut duplicate, &model).err(), Some(Cut));
            }

            let root_model = ev.scope_model(ScopeTree::root());
            let root_mappings = resolve_scope_mappings(ScopeTree::root(), ev.cx)
                .expect("delegated root mappings resolve");
            let root_ids: Vec<_> = root_mappings
                .iter()
                .filter_map(|mapping| mapping.slot)
                .collect();
            for (index, output) in root_ids.iter().enumerate() {
                assert!(root_ids[..index].iter().all(|earlier| earlier != output));
                assert_eq!(
                    ev.cx.mapping_cache.models.arena.owner(*output),
                    ScopeTree::root()
                );
            }
            for pair in &root_model.forwarded {
                if matches!(pair.to, OutputRef::Slot(_)) {
                    assert_eq!(
                        root_mappings
                            .iter()
                            .filter(|mapping| mapping.slot == Some(pair.to))
                            .count(),
                        1
                    );
                }
            }
        });
    }
}

#[test]
fn set_operation_provider_evaluation_uses_the_merged_segment_on_both_paths() {
    let graph = parse(
        "WITH s AS (SELECT * FROM t1 UNION ALL SELECT * FROM t2) SELECT * FROM s",
        Dialect::Generic,
    );
    let op = cte(&graph, "s");
    evaluate(&graph, |ev| {
        let model = ev.scope_model(op);
        let OutputElement::Unknown(segment) = &model.elements[0] else {
            panic!("the set operation forwards the unknown segment");
        };
        let provider = segment.provider.clone();
        let position = segment.position;
        let mapping = ev
            .provider_mapping(&provider, position)
            .expect("the merged segment has a mapping");
        assert_eq!(show(&mapping.sources), ["t1.*", "t2.*"]);
        assert_eq!(
            show(&ev.provider_sources(&provider, &Ident::unquoted("id"))),
            ["t1.*:id", "t2.*:id"]
        );
    });
}
