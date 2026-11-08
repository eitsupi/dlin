//! A projection's mappings are its published model, element by element.

use super::super::projection_mappings;
use super::harness::{cte, evaluate, parse};
use crate::graph::scope::{OutputPlan, ScopeId, ScopeTree};
use crate::resolve::model::OutputElement;
use crate::types::ColumnOrigin;
use crate::{Dialect, Ident};

/// The projection a statement's root reads through its delegating scopes.
fn body(graph: &crate::graph::RawGraph) -> ScopeId {
    let mut scope = ScopeTree::root();
    while let OutputPlan::Delegate(child) = graph.scopes.output_plan(scope) {
        scope = *child;
    }
    scope
}

const STATEMENTS: &[&str] = &[
    "SELECT 1 AS p, 2 AS q",
    "SELECT * FROM t",
    "SELECT a, *, b FROM t",
    "SELECT 1 AS p, s.* FROM (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) s",
    "SELECT * FROM (SELECT 1 AS p, s.* FROM (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) s) o",
    "SELECT 1 AS p, s.*, 2 AS q FROM (SELECT * FROM t1 UNION ALL SELECT * FROM t2) s",
    "SELECT * FROM (VALUES (1, 2)) AS v",
    "SELECT 1 AS p, v.* FROM (VALUES (1, 2)) AS v",
    "SELECT x.*, s.* FROM t x, (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) s",
    "WITH c AS (SELECT t.a AS x, t.b AS x FROM t) SELECT *, * FROM c",
    "SELECT * REPLACE (a + 1 AS a) FROM (SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) s",
];

#[test]
fn a_projection_has_one_mapping_per_element_that_stands_for_columns() {
    for sql in STATEMENTS {
        let graph = parse(sql, Dialect::Generic);
        let scope = body(&graph);
        assert!(
            matches!(graph.scopes.output_plan(scope), OutputPlan::Projection),
            "{sql}"
        );
        evaluate(&graph, |ev| {
            let model = ev.scope_model(scope);
            let mappings = projection_mappings(scope, &model, ev.cx);
            let mut expected = 0;
            let mut next = mappings.iter();
            for element in &model.elements {
                let produced = match element {
                    OutputElement::Column(slot) => {
                        let mapping = next.clone().next().expect("a column has a mapping");
                        assert_eq!(mapping.target.column, slot.name, "{sql}");
                        assert_eq!(mapping.ordinal, slot.ordinal, "{sql}");
                        1
                    }
                    OutputElement::Unknown(segment) => {
                        usize::from(ev.segment_mapping(segment.published()).is_some())
                    }
                    OutputElement::Tail(tail) => ev.tail_mappings(tail).len(),
                };
                expected += produced;
                for _ in 0..produced {
                    next.next();
                }
            }
            assert_eq!(mappings.len(), expected, "{sql}");
            assert!(next.next().is_none(), "{sql}");
        });
    }
}

#[test]
fn a_tail_remembers_the_columns_before_the_star_that_carries_it() {
    let union = "(SELECT * FROM t1 UNION ALL SELECT a, b FROM t2) s";
    let offset = |sql: &str, scope_of: fn(&crate::graph::RawGraph) -> ScopeId| {
        let graph = parse(sql, Dialect::Generic);
        evaluate(&graph, |ev| {
            let model = ev.scope_model(scope_of(&graph));
            let offsets: Vec<_> = model
                .elements
                .iter()
                .filter_map(|element| match element {
                    OutputElement::Tail(tail) => Some(tail.offset),
                    _ => None,
                })
                .collect();
            assert_eq!(offsets.len(), 1, "{sql}");
            offsets[0]
        })
    };
    assert_eq!(
        offset(&format!("SELECT * FROM {union}"), body),
        Some(0),
        "no column precedes the star"
    );
    assert_eq!(
        offset(&format!("SELECT 1 AS p, s.* FROM {union}"), body),
        Some(1)
    );
    assert_eq!(
        offset(&format!("SELECT 1 AS p, 2 AS q, s.* FROM {union}"), body),
        Some(2)
    );
    // A star of unknown width before it leaves the offset unknown.
    assert_eq!(
        offset(&format!("SELECT u.*, s.* FROM u, {union}"), body),
        None
    );
    // A reader that only re-expresses the projection starts counting again.
    assert_eq!(
        offset(
            &format!("SELECT * FROM (SELECT 1 AS p, s.* FROM {union}) o"),
            body
        ),
        Some(0)
    );
}

#[test]
fn a_recursive_marker_is_in_the_mappings_but_not_in_what_a_reader_sees() {
    let sql = "WITH RECURSIVE c AS (SELECT 1 AS n UNION ALL SELECT n FROM c), \
               d AS (SELECT n + 1 AS m FROM c) SELECT m FROM d";
    let graph = parse(sql, Dialect::Generic);
    let projection = cte(&graph, "d");
    evaluate(&graph, |ev| {
        let model = ev.scope_model(projection);
        let mappings = projection_mappings(projection, &model, ev.cx);
        assert_eq!(mappings.len(), 1);
        assert!(matches!(
            mappings[0].sources.as_slice(),
            [ColumnOrigin::Recursive { .. }]
        ));
        let model = ev.scope_model(projection);
        let OutputElement::Column(slot) = &model.elements[0] else {
            panic!("the projection lists m");
        };
        assert_eq!(slot.name, Ident::unquoted("m"));
        let (sources, has_back, _) = ev.slot_read(slot.published());
        assert!(!has_back);
        assert!(
            sources
                .iter()
                .all(|source| !matches!(source, ColumnOrigin::Recursive { .. }))
        );
    });
}
