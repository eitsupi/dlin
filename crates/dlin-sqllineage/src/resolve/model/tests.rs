use std::rc::Rc;

use super::{
    InputLookup, ModelStore, OutputElement, OutputModel, OutputModels, UnknownProvider, ValueId,
    Width,
};
use crate::catalog::CatalogContext;
use crate::graph::RawGraph;
use crate::graph::node::RawNode;
use crate::graph::scope::{OutputPlan, ScopeTree};
use crate::{Dialect, Ident};

struct Tables(Vec<(&'static str, Vec<&'static str>)>);

impl crate::Catalog for Tables {
    fn lookup(&self, relation: &crate::Relation) -> Option<crate::CatalogEntry<'_>> {
        let (_, columns) = self
            .0
            .iter()
            .find(|(name, _)| *name == relation.name.value)?;
        Some(crate::CatalogEntry {
            relation: relation.clone(),
            columns: columns
                .iter()
                .map(|name| crate::CatalogColumn {
                    name: Ident::unquoted(*name),
                    data_type: None,
                    fields: None,
                })
                .collect::<Vec<_>>()
                .into(),
        })
    }
}

fn graph(sql: &str) -> RawGraph {
    let dialect = Dialect::Generic;
    let statements = sqlparser::parser::Parser::new(&*dialect.to_sqlparser_dialect())
        .try_with_sql(sql)
        .and_then(|mut parser| parser.parse_statements())
        .expect("test SQL parses");
    let (graph, _, _) = crate::build::LineageBuilder::new(dialect)
        .build(&statements[0])
        .unwrap_or_else(|_| panic!("test SQL builds"));
    graph
}

/// The model of the first star of `sql` under the given catalog tables.
fn first_star(sql: &str, tables: &[(&'static str, Vec<&'static str>)]) -> Rc<OutputModel> {
    let graph = graph(sql);
    let catalog = Tables(tables.to_vec());
    let context = CatalogContext::new(&catalog, Dialect::Generic);
    let mut store = ModelStore::default();
    let mut models = OutputModels::new(&graph, Some(&context), &mut store);
    let node = graph
        .nodes
        .iter()
        .position(|node| matches!(node, RawNode::Star { .. }))
        .expect("test SQL has a star");
    models.star(node)
}

/// Column names in output order; `?` stands for an unknown segment.
fn layout(model: &OutputModel) -> Vec<String> {
    model
        .elements
        .iter()
        .map(|element| match element {
            OutputElement::Column(slot) => slot.name.value.clone(),
            OutputElement::Unknown(_) => "?".to_owned(),
            OutputElement::Tail(_) => "~".to_owned(),
        })
        .collect()
}

fn ranges(model: &OutputModel, name: &str) -> Vec<String> {
    let [index] = model.slots_named(&Ident::unquoted(name)) else {
        panic!("expected exactly one column named {name}");
    };
    let OutputElement::Column(slot) = &model.elements[*index] else {
        unreachable!();
    };
    slot.ranges
        .iter()
        .map(|range| range.name.value.clone())
        .collect()
}

#[test]
fn using_folds_the_key_once_and_keeps_both_range_variables() {
    let model = first_star(
        "SELECT * FROM a JOIN b USING (id)",
        &[("a", vec!["x", "id"]), ("b", vec!["id", "y"])],
    );
    assert_eq!(layout(&model), ["id", "x", "y"]);
    assert_eq!(model.width, Width::Exact(3));
    assert_eq!(ranges(&model, "id"), ["a", "b"]);
    assert_eq!(ranges(&model, "x"), ["a"]);
}

#[test]
fn natural_join_folds_the_shared_names_of_listed_columns() {
    let model = first_star(
        "SELECT * FROM a NATURAL JOIN b",
        &[("a", vec!["id", "x"]), ("b", vec!["y", "id"])],
    );
    assert_eq!(layout(&model), ["id", "x", "y"]);
}

#[test]
fn qualified_exclude_matches_either_side_of_a_merged_column() {
    for qualifier in ["a", "b"] {
        let model = first_star(
            &format!("SELECT * EXCLUDE ({qualifier}.id) FROM a JOIN b USING (id)"),
            &[("a", vec!["id", "x"]), ("b", vec!["id", "y"])],
        );
        assert_eq!(layout(&model), ["x", "y"]);
    }
}

#[test]
fn rename_applies_in_list_order_to_the_current_names() {
    let model = first_star(
        "SELECT * RENAME (x AS y, y AS z) FROM t",
        &[("t", vec!["x", "y"])],
    );
    assert_eq!(layout(&model), ["z", "y"]);
}

#[test]
fn replace_keeps_position_and_name_but_not_excluded_columns() {
    let model = first_star(
        "SELECT * EXCLUDE (y) REPLACE (1 AS x, 2 AS y) FROM t",
        &[("t", vec!["x", "y", "z"])],
    );
    assert_eq!(layout(&model), ["x", "z"]);
    let OutputElement::Column(slot) = &model.elements[0] else {
        unreachable!();
    };
    assert!(matches!(slot.value, ValueId::Expr(_)));
}

#[test]
fn a_relation_without_columns_is_an_unknown_segment() {
    let model = first_star("SELECT * FROM t", &[]);
    assert_eq!(layout(&model), ["?"]);
    assert_eq!(model.width, Width::Unknown);
    assert!(model.has_unknown_segment());
    let [OutputElement::Unknown(segment)] = model.elements.as_slice() else {
        unreachable!();
    };
    assert!(matches!(segment.provider, UnknownProvider::Relation(_)));
}

#[test]
fn exclude_restricts_the_names_an_unknown_segment_may_publish() {
    let model = first_star("SELECT * EXCLUDE (t.id) FROM t", &[]);
    let [OutputElement::Unknown(segment)] = model.elements.as_slice() else {
        unreachable!();
    };
    assert!(
        !segment
            .domain
            .admits(&Ident::unquoted("id"), Dialect::Generic)
    );
    assert!(
        segment
            .domain
            .admits(&Ident::unquoted("other"), Dialect::Generic)
    );
    let other = first_star("SELECT * EXCLUDE (u.id) FROM t JOIN u ON TRUE", &[]);
    assert!(other.has_unknown_segment());
}

#[test]
fn using_with_one_known_side_binds_the_key_to_the_listed_column() {
    let model = first_star(
        "SELECT * FROM a JOIN b USING (id)",
        &[("b", vec!["id", "y"])],
    );
    // The key leads. An inner join takes it from both sides, and the
    // unknown relation supplies it as a column.
    assert_eq!(layout(&model), ["id", "?", "y"]);
    assert_eq!(ranges(&model, "id"), ["a", "b"]);
    let OutputElement::Column(slot) = &model.elements[0] else {
        unreachable!();
    };
    assert!(matches!(slot.value, ValueId::Merged(..)));
    let OutputElement::Unknown(segment) = &model.elements[1] else {
        unreachable!();
    };
    assert!(
        !segment
            .domain
            .admits(&Ident::unquoted("id"), Dialect::Generic)
    );
    assert!(
        segment
            .domain
            .admits(&Ident::unquoted("name"), Dialect::Generic)
    );
}

#[test]
fn using_value_follows_the_join_kind_when_one_side_is_unknown() {
    // The unknown `a` supplies `id` as a column of its own; `b` lists it.
    for (join, expected) in [
        ("JOIN", "merged"),
        ("LEFT JOIN", "a"),
        ("RIGHT JOIN", "b"),
        ("FULL JOIN", "merged"),
    ] {
        let model = first_star(
            &format!("SELECT * FROM a {join} b USING (id)"),
            &[("b", vec!["id", "y"])],
        );
        assert_eq!(layout(&model), ["id", "?", "y"], "{join}");
        let OutputElement::Column(slot) = &model.elements[0] else {
            unreachable!();
        };
        match (&slot.value, expected) {
            (ValueId::Merged(..), "merged") => {}
            (ValueId::Physical { table, .. }, owner) => {
                assert_eq!(table.table.value, owner, "{join}");
            }
            (value, _) => panic!("{join}: unexpected {value:?}"),
        }
    }
}

#[test]
fn set_operation_keeps_the_columns_its_right_operand_adds_as_a_tail() {
    let graph = graph("SELECT * FROM t1 UNION ALL SELECT a, b FROM t2");
    let mut store = ModelStore::default();
    let mut models = OutputModels::new(&graph, None, &mut store);
    let model = models.scope(ScopeTree::root());
    assert_eq!(layout(&model), ["?", "~"]);
    assert_eq!(model.listed_len(), 1);
    assert_eq!(model.width, Width::Unknown);
}

#[test]
fn using_with_two_unknown_sides_lists_the_key_first() {
    let model = first_star("SELECT * FROM a JOIN b USING (id)", &[]);
    assert_eq!(layout(&model), ["id", "?", "?"]);
    assert_eq!(ranges(&model, "id"), ["a", "b"]);

    let excluded = first_star("SELECT * EXCLUDE (b.id) FROM a JOIN b USING (id)", &[]);
    assert_eq!(layout(&excluded), ["?", "?"]);
}

#[test]
fn several_using_keys_lead_in_the_order_they_were_written() {
    let model = first_star(
        "SELECT * FROM a JOIN b USING (k2, k1)",
        &[("a", vec!["k1", "x", "k2"])],
    );
    assert_eq!(layout(&model), ["k2", "k1", "x", "?"]);
}

#[test]
fn replace_and_rename_of_unlisted_columns_become_explicit_columns() {
    let model = first_star("SELECT * REPLACE (1 AS a) RENAME (b AS c) FROM t", &[]);
    assert_eq!(layout(&model), ["?", "a", "c"]);
    let OutputElement::Column(renamed) = &model.elements[2] else {
        unreachable!();
    };
    assert!(matches!(renamed.value, ValueId::Named { .. }));
    let OutputElement::Unknown(segment) = &model.elements[0] else {
        unreachable!();
    };
    assert!(
        !segment
            .domain
            .admits(&Ident::unquoted("b"), Dialect::Generic)
    );
}

#[test]
fn replace_of_an_excluded_unlisted_column_adds_nothing() {
    let model = first_star("SELECT * EXCLUDE (a) REPLACE (1 AS a) FROM t", &[]);
    assert_eq!(layout(&model), ["?"]);
}

#[test]
fn replace_and_rename_take_over_the_name_of_a_set_operation_tail() {
    let set_operation = "SELECT * FROM t1 UNION ALL SELECT a, b FROM t2";
    for (modifier, new_name) in [("REPLACE (1 AS b)", "b"), ("RENAME (b AS kept)", "kept")] {
        let graph = graph(&format!("SELECT * {modifier} FROM ({set_operation}) s"));
        let node = graph
            .nodes
            .iter()
            .position(
                |node| matches!(node, RawNode::Star { env, .. } if env.scope == ScopeTree::root()),
            )
            .expect("the root projection has a star");
        let mut store = ModelStore::default();
        let model = OutputModels::new(&graph, None, &mut store).star(node);
        assert_eq!(layout(&model), ["?", "~", new_name], "{modifier}");
        let OutputElement::Tail(tail) = &model.elements[1] else {
            unreachable!();
        };
        // The tail no longer publishes `b`; the new column stands for it.
        assert!(!tail.domain.admits(&Ident::unquoted("b"), Dialect::Generic));
        assert!(tail.domain.admits(&Ident::unquoted("a"), Dialect::Generic));
    }
}

#[test]
fn scope_models_are_built_once_and_forward_child_slots() {
    let graph = graph("WITH c AS (SELECT x, y FROM t) SELECT * FROM c");
    let mut store = ModelStore::default();
    let mut models = OutputModels::new(&graph, None, &mut store);
    let root = models.scope(ScopeTree::root());
    assert!(Rc::ptr_eq(&root, &models.scope(ScopeTree::root())));
    assert_eq!(layout(&root), ["x", "y"]);
    let OutputElement::Column(slot) = &root.elements[0] else {
        unreachable!();
    };
    assert!(matches!(slot.value, ValueId::ScopeSlot { index: 0, .. }));
}

#[test]
fn a_cycle_is_cut_to_an_unknown_segment() {
    let graph = graph("WITH RECURSIVE t AS (SELECT * FROM t UNION ALL SELECT 1) SELECT * FROM t");
    let mut store = ModelStore::default();
    let mut models = OutputModels::new(&graph, None, &mut store);
    assert!(models.scope(ScopeTree::root()).has_unknown_segment());
}

/// The scope that holds the FROM clause of the root query: the root itself, or
/// the body below its WITH clause.
fn query_scope(graph: &RawGraph) -> usize {
    let mut scope = ScopeTree::root();
    while let OutputPlan::Delegate(child) = graph.scopes.output_plan(scope) {
        scope = *child;
    }
    scope
}

/// What an unqualified `name` denotes in the FROM clause of the root query.
fn lookup(sql: &str, tables: &[(&'static str, Vec<&'static str>)], name: &str) -> InputLookup {
    let graph = graph(sql);
    let catalog = Tables(tables.to_vec());
    let context = CatalogContext::new(&catalog, Dialect::Generic);
    let mut store = ModelStore::default();
    let env = graph.scopes.env(query_scope(&graph));
    OutputModels::new(&graph, Some(&context), &mut store).lookup_input(env, &Ident::unquoted(name))
}

fn contributing(lookup: &InputLookup) -> Vec<String> {
    let InputLookup::Column { contributing, .. } = lookup else {
        panic!("expected one column, got {lookup:?}");
    };
    contributing
        .iter()
        .map(|range| range.name.value.clone())
        .collect()
}

#[test]
fn the_input_model_is_built_once_per_scope() {
    let graph = graph("SELECT * FROM a JOIN b USING (id)");
    let mut store = ModelStore::default();
    let mut models = OutputModels::new(&graph, None, &mut store);
    let env = graph.scopes.env(ScopeTree::root());
    let first = models.input(env);
    assert!(Rc::ptr_eq(&first, &models.input(env)));
}

#[test]
fn a_single_relation_owns_every_name() {
    assert!(matches!(
        lookup("SELECT x FROM t", &[], "x"),
        InputLookup::Physical(_)
    ));
    assert!(matches!(
        lookup("WITH c AS (SELECT y FROM t) SELECT x FROM c", &[], "x"),
        InputLookup::Scope { .. }
    ));
}

#[test]
fn a_using_key_is_one_column_of_the_relations_that_supply_it() {
    let tables: &[(&str, Vec<&str>)] = &[];
    for (join, expected) in [
        ("JOIN", vec!["a", "b"]),
        ("LEFT JOIN", vec!["a"]),
        ("RIGHT JOIN", vec!["b"]),
        ("FULL JOIN", vec!["a", "b"]),
    ] {
        let found = lookup(
            &format!("SELECT id FROM a {join} b USING (id)"),
            tables,
            "id",
        );
        assert_eq!(contributing(&found), expected, "{join}");
    }
}

#[test]
fn a_using_key_competes_with_an_unknown_relation_outside_the_join() {
    let found = lookup("SELECT id FROM a JOIN b USING (id) CROSS JOIN c", &[], "id");
    let InputLookup::Ambiguous {
        candidates, typed, ..
    } = found
    else {
        panic!("expected an ambiguity, got {found:?}");
    };
    assert_eq!(candidates.len(), 3);
    assert!(typed);
}

#[test]
fn unknown_derived_tables_are_possible_owners() {
    let found = lookup(
        "SELECT id FROM a JOIN (SELECT * FROM b) d ON TRUE",
        &[],
        "id",
    );
    let InputLookup::Ambiguous {
        candidates, typed, ..
    } = found
    else {
        panic!("expected an ambiguity, got {found:?}");
    };
    assert_eq!(candidates.len(), 2);
    assert!(
        typed,
        "a derived table is not a relation a catalog can name"
    );
}

#[test]
fn a_listed_column_competes_with_an_unknown_relation() {
    // `b` may have an `x` of its own, so the listed `a.x` is not the only
    // possible owner.
    let found = lookup("SELECT x FROM a JOIN b ON TRUE", &[("a", vec!["x"])], "x");
    let InputLookup::Ambiguous {
        candidates, typed, ..
    } = found
    else {
        panic!("expected an ambiguity, got {found:?}");
    };
    assert_eq!(candidates.len(), 2);
    assert!(typed, "a catalog that lacks `b` cannot settle it");
}

#[test]
fn a_relation_the_catalog_knows_to_lack_the_name_is_not_a_provider() {
    let found = lookup(
        "SELECT x FROM a JOIN b ON TRUE",
        &[("a", vec!["x"]), ("b", vec!["y"])],
        "x",
    );
    assert_eq!(contributing(&found), ["a"]);
}

#[test]
fn range_names_that_repeat_leave_merged_names_ambiguous() {
    let found = lookup("SELECT id FROM a AS t JOIN b AS t USING (id)", &[], "id");
    let InputLookup::Ambiguous { candidates, .. } = found else {
        panic!("expected an ambiguity, got {found:?}");
    };
    assert_eq!(candidates.len(), 2);
    // A name no join merges is still decided by what the ranges list.
    let found = lookup(
        "SELECT x FROM a AS t JOIN b AS t USING (id)",
        &[("a", vec!["x"]), ("b", vec!["id"])],
        "x",
    );
    assert_eq!(contributing(&found), ["t"]);
}

#[test]
fn a_key_taken_from_several_unknown_relations_is_uncertain() {
    let model = first_star("SELECT * FROM a NATURAL JOIN b JOIN c USING (id)", &[]);
    let OutputElement::Column(key) = &model.elements[0] else {
        panic!("the key leads");
    };
    assert!(matches!(key.value, ValueId::Uncertain { .. }));
    let names: Vec<_> = key
        .candidate_ranges
        .iter()
        .map(|range| range.name.value.as_str())
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
}
