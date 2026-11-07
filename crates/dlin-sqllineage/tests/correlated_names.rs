//! An unqualified column name is settled scope by scope: the innermost query
//! that provides the column answers for it, and only a query that cannot be
//! shown to provide it leaves the name to the queries around. A subquery in
//! an expression never adds its range variables or its outputs to the query
//! it sits in.

mod common;

use common::{KnownTables, analyze_one_with, concrete_sources, find_mapping};
use dlin_sqllineage::{
    Candidate, Catalog, Dialect, Lineage, OutputColumn, StatementAnalysis, Unresolved,
};

const ALL_DIALECTS: [Dialect; 5] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::Snowflake,
    Dialect::BigQuery,
    Dialect::DuckDb,
];

static T_X_U_Y: KnownTables = KnownTables(&[("t", &["x"]), ("u", &["y"])]);
static T_X_U_X: KnownTables = KnownTables(&[("t", &["x"]), ("u", &["x"])]);
static T_Y_ONLY: KnownTables = KnownTables(&[("t", &["y"])]);
static U_Y_ONLY: KnownTables = KnownTables(&[("u", &["y"])]);
static U_X_ONLY: KnownTables = KnownTables(&[("u", &["x"])]);
static T_XY: KnownTables = KnownTables(&[("t", &["x", "y"])]);

fn analyze(sql: &str, dialect: Dialect, catalog: Option<&dyn Catalog>) -> StatementAnalysis {
    analyze_one_with(sql, dialect, catalog)
}

fn pair(relation: &str, column: &str) -> (String, String) {
    (relation.to_owned(), column.to_owned())
}

fn read_names(result: &StatementAnalysis) -> Vec<String> {
    let mut names: Vec<String> = result
        .read
        .iter()
        .map(|relation| relation.name.value.clone())
        .collect();
    names.sort();
    names
}

/// The candidates of a column whose only content is one ambiguous name.
fn ambiguous_candidates(column: &OutputColumn) -> Vec<String> {
    let Lineage::Partial { known, unresolved } = &column.lineage else {
        panic!("expected an ambiguous column, got {:?}", column.lineage);
    };
    assert!(known.is_empty(), "{:?}", column.lineage);
    let [Unresolved::Ambiguous { candidates, .. }] = unresolved.as_slice() else {
        panic!("expected one ambiguity, got {:?}", column.lineage);
    };
    let mut names: Vec<String> = candidates
        .iter()
        .map(|candidate| match candidate {
            Candidate::Relation(relation) => relation.name.value.clone(),
            other => panic!("unexpected candidate {other:?}"),
        })
        .collect();
    names.sort();
    names
}

fn check_complete(
    sql: &str,
    catalog: Option<&dyn Catalog>,
    column: &str,
    expected: &[(&str, &str)],
    reads: &[&str],
) {
    for dialect in ALL_DIALECTS {
        let result = analyze(sql, dialect, catalog);
        let expected: Vec<_> = expected.iter().map(|(r, c)| pair(r, c)).collect();
        assert_eq!(
            concrete_sources(find_mapping(&result.output.columns, column)),
            expected,
            "{dialect:?}: {sql}"
        );
        assert_eq!(read_names(&result), reads, "{dialect:?}: {sql}");
    }
}

fn check_ambiguous(sql: &str, catalog: Option<&dyn Catalog>, column: &str, expected: &[&str]) {
    for dialect in ALL_DIALECTS {
        let result = analyze(sql, dialect, catalog);
        assert_eq!(
            ambiguous_candidates(find_mapping(&result.output.columns, column)),
            expected,
            "{dialect:?}: {sql}"
        );
    }
}

// The subquery's own relation is known to lack the column, so the name is the
// enclosing query's.
#[test]
fn a_scope_that_lacks_the_column_leaves_it_to_the_scope_around() {
    check_complete(
        "SELECT (SELECT x FROM u) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &[("t", "x")],
        &["t", "u"],
    );
    // Through an intermediate scope that lacks it as well.
    check_complete(
        "SELECT (SELECT (SELECT x FROM u) FROM u) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &[("t", "x")],
        &["t", "u"],
    );
}

#[test]
fn a_scope_that_provides_the_column_answers_for_it() {
    check_complete(
        "SELECT (SELECT x FROM u) AS v FROM t",
        Some(&T_X_U_X),
        "v",
        &[("u", "x")],
        &["t", "u"],
    );
}

// The innermost query has no FROM clause and provides nothing; the next one
// out does, and the search stops there.
#[test]
fn a_scope_without_a_from_clause_provides_nothing() {
    check_complete(
        "SELECT (SELECT (SELECT x) FROM u) AS v FROM t",
        Some(&T_X_U_X),
        "v",
        &[("u", "x")],
        &["t", "u"],
    );
    check_complete(
        "SELECT (SELECT (SELECT x) FROM u) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &[("t", "x")],
        &["t", "u"],
    );
}

// Without a catalog, `u` may or may not have `x`, and if it does not the name
// is `t.x`. Neither can be preferred.
#[test]
fn a_scope_that_may_provide_the_column_competes_with_the_scope_around() {
    check_ambiguous(
        "SELECT (SELECT x FROM u) AS v FROM t",
        None,
        "v",
        &["t", "u"],
    );
    // The enclosing query knows `x`; the subquery's relation may have it too.
    check_ambiguous(
        "SELECT (SELECT x FROM w) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &["t", "w"],
    );
    // A competition that spans more than two scopes.
    check_ambiguous(
        "SELECT (SELECT (SELECT x FROM w) FROM u) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &["t", "w"],
    );
}

#[test]
fn a_possible_provider_stands_when_nothing_around_can_provide_the_column() {
    // No query around the subquery.
    check_complete(
        "SELECT (SELECT x FROM u) AS v",
        None,
        "v",
        &[("u", "x")],
        &["u"],
    );
    // The enclosing relation is known to lack the column.
    check_complete(
        "SELECT (SELECT x FROM w) AS v FROM t",
        Some(&T_Y_ONLY),
        "v",
        &[("w", "x")],
        &["t", "w"],
    );
    // The same relation at both levels supplies the same column either way.
    check_complete(
        "SELECT (SELECT x FROM t) AS v FROM t",
        None,
        "v",
        &[("t", "x")],
        &["t"],
    );
}

#[test]
fn an_unknown_enclosing_relation_gets_the_column_a_scope_lacks() {
    check_complete(
        "SELECT (SELECT x FROM u) AS v FROM t",
        Some(&U_Y_ONLY),
        "v",
        &[("t", "x")],
        &["t", "u"],
    );
    check_complete(
        "SELECT (SELECT x FROM u) AS v FROM t",
        Some(&U_X_ONLY),
        "v",
        &[("u", "x")],
        &["t", "u"],
    );
}

// A name that nothing provides is still the innermost relation's: a clause
// with a single relation answers for any name.
#[test]
fn a_name_nobody_provides_stays_with_the_innermost_relation() {
    check_complete(
        "SELECT (SELECT z FROM u) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &[("u", "z")],
        &["t", "u"],
    );
}

#[test]
fn a_name_that_two_relations_of_one_scope_share_does_not_go_outward() {
    check_ambiguous(
        "SELECT (SELECT x FROM u AS a, u AS a) AS v FROM t",
        Some(&T_X_U_X),
        "v",
        &["u"],
    );
}

#[test]
fn a_qualified_name_uses_the_range_variable_whatever_the_scopes_provide() {
    check_complete(
        "SELECT (SELECT t.x FROM u) AS v FROM t",
        Some(&T_X_U_X),
        "v",
        &[("t", "x")],
        &["t", "u"],
    );
    check_complete(
        "SELECT (SELECT u.x FROM u) AS v FROM t",
        Some(&T_X_U_X),
        "v",
        &[("u", "x")],
        &["t", "u"],
    );
}

#[test]
fn using_joins_inside_a_subquery_keep_their_own_clause() {
    let catalog: &dyn Catalog = &KnownTables(&[("t", &["x"]), ("u", &["x"]), ("w", &["x"])]);
    check_complete(
        "SELECT (SELECT x FROM u JOIN w USING (x)) AS v FROM t",
        Some(catalog),
        "v",
        &[("u", "x"), ("w", "x")],
        &["t", "u", "w"],
    );
}

// The reference in the subquery's WHERE or SELECT list is correlated with the
// query around it when its own relation lacks the name.
#[test]
fn a_correlated_reference_binds_to_the_enclosing_query() {
    check_complete(
        "SELECT (SELECT CASE WHEN x = 1 THEN y END FROM u) AS v FROM t",
        Some(&T_X_U_Y),
        "v",
        &[("t", "x"), ("u", "y")],
        &["t", "u"],
    );
    for dialect in ALL_DIALECTS {
        let result = analyze(
            "SELECT t.x FROM t WHERE EXISTS (SELECT 1 FROM u WHERE x = 1)",
            dialect,
            Some(&T_X_U_Y),
        );
        assert_eq!(result.output.columns.len(), 1, "{dialect:?}");
        assert_eq!(
            concrete_sources(&result.output.columns[0]),
            [pair("t", "x")],
            "{dialect:?}"
        );
        assert_eq!(read_names(&result), ["t", "u"], "{dialect:?}");
    }
}

// Contract case B3: the binding and the output of a subquery do not leak into
// the query it sits in, whichever walk reaches the subquery.
#[test]
fn contract_case_b3_a_subquery_in_a_function_argument_does_not_leak() {
    let sql = "SELECT a FROM t WHERE ARRAY_LENGTH(ARRAY(SELECT b FROM u)) > 0";
    for dialect in ALL_DIALECTS {
        let result = analyze(sql, dialect, None);
        assert_eq!(result.output.columns.len(), 1, "{dialect:?}");
        assert_eq!(
            result.output.columns[0].lineage,
            Lineage::Complete(vec![dlin_sqllineage::SourceColumn {
                relation: common::table("t"),
                column: dlin_sqllineage::Ident::unquoted("a"),
            }]),
            "{dialect:?}"
        );
        // The subquery is still read.
        assert_eq!(read_names(&result), ["t", "u"], "{dialect:?}");
    }
}

#[test]
fn subqueries_in_a_filter_never_add_outputs() {
    for dialect in ALL_DIALECTS {
        for sql in [
            "SELECT a FROM t WHERE a IN (SELECT b FROM u)",
            "SELECT a FROM t WHERE EXISTS (SELECT b FROM u)",
            "SELECT a FROM t WHERE (SELECT b FROM u) > 0",
            "SELECT a FROM t WHERE ARRAY_LENGTH(ARRAY(SELECT b FROM u), 1) > 0",
        ] {
            let result = analyze(sql, dialect, None);
            assert_eq!(result.output.columns.len(), 1, "{dialect:?}: {sql}");
            assert_eq!(
                concrete_sources(&result.output.columns[0]),
                [pair("t", "a")],
                "{dialect:?}: {sql}"
            );
            assert_eq!(read_names(&result), ["t", "u"], "{dialect:?}: {sql}");
        }
    }
}

#[test]
fn a_star_in_a_subquery_does_not_widen_the_query_around() {
    for dialect in ALL_DIALECTS {
        let result = analyze(
            "SELECT * FROM t WHERE EXISTS (SELECT * FROM u)",
            dialect,
            Some(&T_X_U_Y),
        );
        assert_eq!(result.output.columns.len(), 1, "{dialect:?}");
        assert_eq!(
            concrete_sources(&result.output.columns[0]),
            [pair("t", "x")],
            "{dialect:?}"
        );
    }
}

// Wrapping an expression in a scalar subquery that has no FROM clause changes
// nothing about which columns it depends on.
#[test]
fn wrapping_an_expression_in_a_fromless_scalar_subquery_keeps_its_lineage() {
    let expressions = [
        "x",
        "x + y",
        "COALESCE(x, y)",
        "CASE WHEN x > 1 THEN y ELSE x END",
        "UPPER(x)",
        "t.x",
        "z",
    ];
    for dialect in ALL_DIALECTS {
        for catalog in [None, Some(&T_XY as &dyn Catalog)] {
            for expression in expressions {
                let plain = analyze(
                    &format!("SELECT {expression} AS v FROM t"),
                    dialect,
                    catalog,
                );
                let wrapped = analyze(
                    &format!("SELECT (SELECT {expression}) AS v FROM t"),
                    dialect,
                    catalog,
                );
                assert_eq!(
                    plain.output.columns[0].lineage,
                    wrapped.output.columns[0].lineage,
                    "{dialect:?} catalog={}: {expression}",
                    catalog.is_some()
                );
            }
        }
    }
}

// DuckDB lets a plain derived table read the FROM items before it (implicit
// LATERAL). The engine does not model that yet, so the reference is left
// unresolved rather than bound to a wrong relation. This documents a
// limitation to be lifted once visibility is decided per dialect.
#[test]
fn duckdb_implicit_lateral_reference_is_unresolved_for_now() {
    let result = analyze(
        "SELECT d.y FROM t, (SELECT t.x AS y) AS d",
        Dialect::DuckDb,
        None,
    );
    let column = &result.output.columns[0];
    assert!(
        matches!(&column.lineage, Lineage::Partial { known, .. } if known.is_empty()),
        "{:?}",
        column.lineage
    );
    assert_eq!(read_names(&result), ["t"]);
    // With the keyword it is resolved.
    let result = analyze(
        "SELECT d.y FROM t, LATERAL (SELECT t.x AS y) AS d",
        Dialect::DuckDb,
        None,
    );
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        [pair("t", "x")]
    );
}
