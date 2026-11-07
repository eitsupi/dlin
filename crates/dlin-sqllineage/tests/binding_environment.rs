//! What a name can see depends on where it is written: a derived table does
//! not see the other items of its FROM clause, a LATERAL one sees the items
//! before it, a subquery in an expression sees the query around it, and a
//! relation name written with a schema is never a CTE.

mod common;

use std::process::Command;

use common::{KnownTables, analyze_one_with, concrete_sources, find_mapping, known_sources};
use dlin_sqllineage::{Dialect, Ident, Lineage, StatementAnalysis, Unresolved};

/// Run a test body in a child process, on a thread with a small stack.
///
/// A stack overflow aborts the whole process, so a regression that recurses
/// without end would take every other test of the binary down with it. The
/// parent test re-runs itself alone in a child process and only reports
/// whether the child succeeded, with the child's output on failure.
const CHILD_ENV: &str = "BINDING_ENVIRONMENT_ISOLATED_TEST";

macro_rules! isolated_test {
    ($name:ident, $body:expr) => {
        #[test]
        fn $name() {
            run_isolated(stringify!($name), $body);
        }
    };
}

fn run_isolated(name: &str, body: fn()) {
    if std::env::var_os(CHILD_ENV).is_some_and(|value| value == name) {
        std::thread::Builder::new()
            .stack_size(4 * 1024 * 1024)
            .spawn(body)
            .expect("the test thread starts")
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        return;
    }
    let output = Command::new(std::env::current_exe().expect("the test binary has a path"))
        .args(["--exact", name, "--test-threads=1"])
        .env(CHILD_ENV, name)
        .output()
        .expect("the test binary can be run again");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{name} did not pass in its own process ({}):\n{stdout}\n{stderr}",
        output.status
    );
}

static T_AND_U: KnownTables = KnownTables(&[
    ("t", &["x", "y", "id"]),
    ("u", &["x", "y", "id"]),
    ("t1", &["x"]),
]);

fn analyze(sql: &str, dialect: Dialect, catalog: bool) -> StatementAnalysis {
    let catalog: Option<&dyn dlin_sqllineage::Catalog> = catalog.then_some(&T_AND_U);
    analyze_one_with(sql, dialect, catalog)
}

fn sources(result: &StatementAnalysis, column: &str) -> Vec<(String, String)> {
    concrete_sources(find_mapping(&result.output.columns, column))
}

fn read_names(result: &StatementAnalysis) -> Vec<String> {
    result
        .read
        .iter()
        .map(|relation| match &relation.schema {
            Some(schema) => format!("{}.{}", schema.value, relation.name.value),
            None => relation.name.value.clone(),
        })
        .collect()
}

fn pair(relation: &str, column: &str) -> (String, String) {
    (relation.to_owned(), column.to_owned())
}

const ALL_DIALECTS: [Dialect; 5] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::Snowflake,
    Dialect::BigQuery,
    Dialect::DuckDb,
];

const LATERAL_DIALECTS: [Dialect; 4] = [
    Dialect::Generic,
    Dialect::PostgreSql,
    Dialect::Snowflake,
    Dialect::DuckDb,
];

// A name inside a LATERAL derived table is looked up among the items before
// it only. The derived table being defined is not one of them, so a lookup
// that included it would read the derived table's own output while computing
// that output.
isolated_test!(lateral_reference_to_an_earlier_item_does_not_loop, || {
    for catalog in [false, true] {
        for dialect in LATERAL_DIALECTS {
            for sql in [
                "SELECT d.x FROM t CROSS JOIN LATERAL (SELECT x) AS d",
                "SELECT d.x FROM t, LATERAL (SELECT x) AS d",
                "SELECT d.x FROM t INNER JOIN LATERAL (SELECT x) AS d ON TRUE",
                "SELECT d.x FROM t LEFT JOIN LATERAL (SELECT x) AS d ON TRUE",
            ] {
                let result = analyze(sql, dialect, catalog);
                assert_eq!(
                    result.output.columns[0].lineage,
                    Lineage::Complete(vec![dlin_sqllineage::SourceColumn {
                        relation: common::table("t"),
                        column: Ident::unquoted("x"),
                    }]),
                    "{dialect:?} catalog={catalog}: {sql}"
                );
                assert_eq!(read_names(&result), ["t"], "{dialect:?}: {sql}");
            }
        }
    }
});

isolated_test!(apply_reference_to_an_earlier_item_does_not_loop, || {
    // The right side of an APPLY is evaluated per row of the left side.
    let result = analyze(
        "SELECT d.x FROM t CROSS APPLY (SELECT x) AS d",
        Dialect::Generic,
        false,
    );
    assert_eq!(sources(&result, "x"), [pair("t", "x")]);
    let result = analyze(
        "SELECT d.x FROM t OUTER APPLY (SELECT x) AS d",
        Dialect::Generic,
        false,
    );
    assert_eq!(sources(&result, "x"), [pair("t", "x")]);
});

isolated_test!(lateral_subquery_reading_its_own_alias_does_not_loop, || {
    // The alias of the derived table being defined is not visible inside it.
    for sql in [
        "SELECT d.x FROM t, LATERAL (SELECT d.x) AS d",
        "SELECT d.x FROM LATERAL (SELECT d.x) AS d",
        "SELECT d.x FROM t, LATERAL (SELECT (SELECT d.x)) AS d",
    ] {
        let result = analyze(sql, Dialect::PostgreSql, true);
        for column in &result.output.columns {
            assert!(
                !matches!(&column.lineage, Lineage::Complete(sources) if sources.is_empty()),
                "{sql}: {:?}",
                column.lineage
            );
        }
    }
});

isolated_test!(lateral_derived_table_sees_the_items_before_it, || {
    let result = analyze(
        "SELECT d.v FROM t AS a, u AS b, LATERAL (SELECT a.x AS v) AS d",
        Dialect::PostgreSql,
        false,
    );
    assert_eq!(sources(&result, "v"), [pair("t", "x")]);
    let result = analyze(
        "SELECT d.v FROM t AS a, u AS b, LATERAL (SELECT b.x AS v) AS d",
        Dialect::PostgreSql,
        false,
    );
    assert_eq!(sources(&result, "v"), [pair("u", "x")]);
});

#[test]
fn ordinary_derived_table_does_not_see_its_sibling_items() {
    for dialect in ALL_DIALECTS {
        for sql in [
            "SELECT d.v FROM t, (SELECT x AS v) AS d",
            "SELECT d.v FROM t AS a, (SELECT a.x AS v) AS d",
        ] {
            let result = analyze(sql, dialect, false);
            let lineage = &result.output.columns[0].lineage;
            assert!(
                known_sources(&result.output.columns[0]).is_empty(),
                "{dialect:?} {sql}: a sibling's column must not become a source: {lineage:?}"
            );
            assert!(
                matches!(lineage, Lineage::Partial { .. }),
                "{dialect:?} {sql}: {lineage:?}"
            );
            assert_eq!(read_names(&result), ["t"], "{dialect:?} {sql}");
        }
    }
}

#[test]
fn ordinary_derived_table_still_sees_the_query_around_it() {
    // An outer reference from a FROM subquery refers to an enclosing query,
    // not to a sibling.
    for dialect in ALL_DIALECTS {
        let result = analyze(
            "SELECT (SELECT d.v FROM (SELECT o.x AS v) AS d) FROM o",
            dialect,
            false,
        );
        assert_eq!(
            concrete_sources_of(&result.output.columns[0]),
            [pair("o", "x")],
            "{dialect:?}"
        );
    }
}

fn concrete_sources_of(column: &dlin_sqllineage::OutputColumn) -> Vec<(String, String)> {
    concrete_sources(column)
}

#[test]
fn subquery_in_an_expression_sees_the_query_around_it() {
    for dialect in ALL_DIALECTS {
        let result = analyze("SELECT (SELECT o.x) AS v FROM o", dialect, false);
        assert_eq!(sources(&result, "v"), [pair("o", "x")], "{dialect:?}");
        let result = analyze("SELECT (SELECT x) AS v FROM o", dialect, false);
        assert_eq!(sources(&result, "v"), [pair("o", "x")], "{dialect:?}");
    }
}

// A schema-qualified relation name is a physical relation. CTEs have no
// schema, so `public.c` cannot be the CTE `c`.
#[test]
fn schema_qualified_relation_is_not_a_cte() {
    let sql = "WITH c AS (SELECT x FROM t1) SELECT p.x FROM public.c AS p";
    let result = analyze(sql, Dialect::PostgreSql, false);
    let relation = dlin_sqllineage::Relation {
        catalog: None,
        schema: Some(Ident::unquoted("public")),
        name: Ident::unquoted("c"),
    };
    assert_eq!(
        result.output.columns[0].lineage,
        Lineage::Complete(vec![dlin_sqllineage::SourceColumn {
            relation: relation.clone(),
            column: Ident::unquoted("x"),
        }])
    );
    assert!(result.read.contains(&relation), "{:?}", result.read);
}

#[test]
fn schema_qualified_relation_is_not_a_cte_in_any_dialect() {
    for dialect in ALL_DIALECTS {
        for qualified in ["public.c", "db.public.c"] {
            let sql = format!("WITH c AS (SELECT x FROM t1) SELECT p.x FROM {qualified} AS p");
            let result = analyze(&sql, dialect, false);
            let lineage = &result.output.columns[0].lineage;
            let Lineage::Complete(found) = lineage else {
                panic!("{dialect:?} {sql}: {lineage:?}");
            };
            assert_eq!(found.len(), 1, "{dialect:?} {sql}");
            assert_eq!(found[0].relation.name, Ident::unquoted("c"));
            assert!(found[0].relation.schema.is_some(), "{dialect:?} {sql}");
        }
    }
}

#[test]
fn unqualified_relation_name_still_reads_the_cte() {
    for dialect in ALL_DIALECTS {
        let result = analyze(
            "WITH c AS (SELECT x FROM t1) SELECT p.x FROM c AS p",
            dialect,
            false,
        );
        assert_eq!(sources(&result, "x"), [pair("t1", "x")], "{dialect:?}");
        assert_eq!(read_names(&result), ["t1"], "{dialect:?}");
    }
}

#[test]
fn table_query_binds_relation_names_like_a_from_item() {
    let result = analyze(
        "WITH c AS (SELECT x FROM t1) TABLE c",
        Dialect::PostgreSql,
        false,
    );
    assert_eq!(read_names(&result), ["t1"]);
    let result = analyze(
        "WITH c AS (SELECT x FROM t1) TABLE public.c",
        Dialect::PostgreSql,
        false,
    );
    let mut read = read_names(&result);
    read.sort();
    assert_eq!(read, ["public.c", "t1"]);
}

#[test]
fn range_variables_do_not_hide_or_replace_cte_names() {
    for dialect in ALL_DIALECTS {
        // `c` the range variable of t2 is not the CTE `c` that `c AS e` reads.
        let result = analyze(
            "WITH c AS (SELECT x FROM t1) SELECT e.x FROM t2 AS c, c AS e",
            dialect,
            false,
        );
        assert_eq!(sources(&result, "x"), [pair("t1", "x")], "{dialect:?}");
        // A derived table's alias is not a relation name either.
        let result = analyze(
            "SELECT e.y FROM (SELECT 1 AS x) AS c, c AS e",
            dialect,
            false,
        );
        assert_eq!(sources(&result, "y"), [pair("c", "y")], "{dialect:?}");
        assert_eq!(read_names(&result), ["c"], "{dialect:?}");
    }
}

// Appending an item to a FROM clause cannot change what a name written
// before it means, in a scope that cannot see the new item.
#[test]
fn appending_an_unrelated_from_item_keeps_an_ordinary_derived_table_meaning() {
    for dialect in ALL_DIALECTS {
        for catalog in [false, true] {
            let base = analyze("SELECT d.v FROM t, (SELECT x AS v) AS d", dialect, catalog);
            for appended in [", u", ", u AS w", " CROSS JOIN u", ", (SELECT 1 AS z) AS z"] {
                let sql = format!("SELECT d.v FROM t, (SELECT x AS v) AS d{appended}");
                let longer = analyze(&sql, dialect, catalog);
                assert_eq!(
                    base.output.columns[0].lineage, longer.output.columns[0].lineage,
                    "{dialect:?} catalog={catalog}: {sql}"
                );
            }
            // `x` is not a column of a sibling, whatever the siblings are.
            assert!(known_sources(&base.output.columns[0]).is_empty());
        }
    }
}

isolated_test!(
    appending_an_unrelated_from_item_keeps_a_lateral_derived_table_meaning,
    || {
        for dialect in LATERAL_DIALECTS {
            for catalog in [false, true] {
                let base = analyze(
                    "SELECT d.v FROM t, LATERAL (SELECT x AS v) AS d",
                    dialect,
                    catalog,
                );
                assert_eq!(
                    concrete_sources_of(&base.output.columns[0]),
                    [pair("t", "x")],
                    "{dialect:?} catalog={catalog}"
                );
                for appended in [", u", ", u AS w", " CROSS JOIN u", ", t AS t2"] {
                    let sql = format!("SELECT d.v FROM t, LATERAL (SELECT x AS v) AS d{appended}");
                    let longer = analyze(&sql, dialect, catalog);
                    assert_eq!(
                        base.output.columns[0].lineage, longer.output.columns[0].lineage,
                        "{dialect:?} catalog={catalog}: {sql}"
                    );
                }
            }
        }
    }
);

#[test]
fn appending_an_unrelated_from_item_keeps_an_unnest_argument_meaning() {
    for dialect in [Dialect::Generic, Dialect::PostgreSql, Dialect::BigQuery] {
        let base = analyze(
            "SELECT item FROM base, UNNEST(items) AS item",
            dialect,
            false,
        );
        for appended in [", other", ", other AS o"] {
            let sql = format!("SELECT item FROM base, UNNEST(items) AS item{appended}");
            let longer = analyze(&sql, dialect, false);
            assert_eq!(
                base.output.columns[0].lineage, longer.output.columns[0].lineage,
                "{dialect:?}: {sql}"
            );
        }
        assert_eq!(
            concrete_sources_of(&base.output.columns[0]),
            [pair("base", "items")],
            "{dialect:?}"
        );
    }
}

#[test]
fn a_correlated_name_after_the_reference_does_not_change_it() {
    // The subquery is written before `u`, but a subquery in an expression
    // sees the whole FROM clause of its query: this pair documents the
    // difference from FROM items, which see only what comes before them.
    let narrow = analyze("SELECT (SELECT x) AS v FROM t", Dialect::PostgreSql, false);
    assert_eq!(sources(&narrow, "v"), [pair("t", "x")]);
    let wide = analyze(
        "SELECT (SELECT x) AS v FROM t, u",
        Dialect::PostgreSql,
        false,
    );
    assert!(matches!(
        wide.output.columns[0].lineage,
        Lineage::Partial { .. }
    ));
}

/// How an alias is written.
#[derive(Clone, Copy, Debug)]
enum Spelling {
    Bare,
    Quoted,
}

fn spell(name: &str, spelling: Spelling, dialect: Dialect) -> String {
    match (spelling, dialect) {
        (Spelling::Bare, _) => name.to_owned(),
        (Spelling::Quoted, Dialect::BigQuery) => format!("`{name}`"),
        (Spelling::Quoted, _) => format!("\"{name}\""),
    }
}

struct Template {
    sql: &'static str,
    lateral: bool,
}

const TEMPLATES: [Template; 11] = [
    Template {
        sql: "SELECT {a}.x FROM t AS {a}",
        lateral: false,
    },
    Template {
        sql: "SELECT {a}.x, {b}.y FROM t AS {a} JOIN u AS {b} ON {a}.id = {b}.id",
        lateral: false,
    },
    Template {
        sql: "SELECT {a}.v FROM (SELECT x AS v FROM t) AS {a}",
        lateral: false,
    },
    Template {
        sql: "SELECT {a}.v FROM (SELECT {b}.x AS v FROM t AS {b}) AS {a}",
        lateral: false,
    },
    Template {
        // The CTE reads a relation no rename produces: a CTE that is named
        // like the relation it reads would read itself in some dialects.
        sql: "WITH {a} AS (SELECT x FROM src) SELECT {a}.x FROM {a}",
        lateral: false,
    },
    Template {
        sql: "WITH c AS (SELECT x FROM t) SELECT {a}.x FROM c AS {a}",
        lateral: false,
    },
    Template {
        sql: "SELECT (SELECT {a}.x) AS v FROM t AS {a}",
        lateral: false,
    },
    Template {
        sql: "SELECT {a}.id FROM t AS {a} JOIN u AS {b} USING (id)",
        lateral: false,
    },
    Template {
        sql: "SELECT {a}.* FROM t AS {a}",
        lateral: false,
    },
    Template {
        sql: "SELECT {a}.v FROM t AS {b}, LATERAL (SELECT {b}.x AS v) AS {a}",
        lateral: true,
    },
    Template {
        sql: "SELECT {a}.v FROM t AS {b}, LATERAL (SELECT x AS v) AS {a}",
        lateral: true,
    },
];

// Renaming a range variable together with every reference to it is not a
// change to the query: the physical lineage must not move, whatever the new
// name is (the name of a column, the name of another relation, a quoted name
// with spaces) and however it is written.
isolated_test!(
    renaming_an_alias_with_its_references_keeps_the_lineage,
    || {
        let names = [
            ("a", "b"),
            ("p", "q"),
            ("x", "y"),
            ("t", "u"),
            ("u", "t"),
            ("id", "id2"),
            ("Mixed Case", "other name"),
        ];
        let mut checked = 0;
        for dialect in ALL_DIALECTS {
            for catalog in [false, true] {
                for template in &TEMPLATES {
                    if template.lateral && dialect == Dialect::BigQuery {
                        continue;
                    }
                    let render = |a: &str, b: &str, spelling: Spelling| {
                        template
                            .sql
                            .replace("{a}", &spell(a, spelling, dialect))
                            .replace("{b}", &spell(b, spelling, dialect))
                    };
                    let baseline_sql = render("a", "b", Spelling::Bare);
                    let baseline = analyze(&baseline_sql, dialect, catalog);
                    for (a, b) in names {
                        for spelling in [Spelling::Bare, Spelling::Quoted] {
                            if matches!(spelling, Spelling::Bare)
                                && (a.contains(' ') || b.contains(' '))
                            {
                                continue;
                            }
                            let sql = render(a, b, spelling);
                            let renamed = analyze(&sql, dialect, catalog);
                            assert_eq!(
                                lineage_summary(&baseline),
                                lineage_summary(&renamed),
                                "{dialect:?} catalog={catalog} {spelling:?}\n  baseline: {baseline_sql}\n  renamed:  {sql}"
                            );
                            checked += 1;
                        }
                    }
                }
            }
        }
        assert!(checked > 1000, "{checked}");
    }
);

/// What a rename must not change: the relations read and, per output column,
/// its name and lineage.
fn lineage_summary(result: &StatementAnalysis) -> String {
    let columns: Vec<String> = result
        .output
        .columns
        .iter()
        .map(|column| format!("{:?}: {:?}", column.name, column.lineage))
        .collect();
    format!(
        "read={:?} shape={:?} unresolved_stars={}\n{}",
        result.read,
        result.output.shape,
        result.has_unresolved_stars,
        columns.join("\n")
    )
}

#[test]
fn renamed_baselines_keep_their_expected_sources() {
    // Pin the baselines the rename test compares, so that comparing equal
    // cannot hide every variant being wrong in the same way.
    let cases: [(&str, &[(&str, &str)]); 4] = [
        ("SELECT {a}.x FROM t AS {a}", &[("x", "t.x")]),
        (
            "SELECT {a}.x, {b}.y FROM t AS {a} JOIN u AS {b} ON {a}.id = {b}.id",
            &[("x", "t.x"), ("y", "u.y")],
        ),
        (
            "WITH {a} AS (SELECT x FROM t) SELECT {a}.x FROM {a}",
            &[("x", "t.x")],
        ),
        (
            "SELECT {a}.v FROM t AS {b}, LATERAL (SELECT {b}.x AS v) AS {a}",
            &[("v", "t.x")],
        ),
    ];
    for (sql, expected) in cases {
        let sql = sql.replace("{a}", "\"u\"").replace("{b}", "\"t\"");
        let result = analyze(&sql, Dialect::PostgreSql, true);
        for (column, source) in expected {
            let (relation, name) = source.split_once('.').unwrap();
            assert_eq!(sources(&result, column), [pair(relation, name)], "{sql}");
        }
    }
}

#[test]
fn unresolved_markers_do_not_leak_a_hidden_alias() {
    // A name that no visible item defines stays unresolved instead of being
    // attributed to a later item.
    let result = analyze(
        "SELECT d.v FROM t, (SELECT z AS v) AS d, u",
        Dialect::PostgreSql,
        true,
    );
    let lineage = &result.output.columns[0].lineage;
    assert!(
        matches!(lineage, Lineage::Partial { known, unresolved }
            if known.is_empty()
                && matches!(&unresolved[..], [Unresolved::Unbound { column }] if column.value == "z")),
        "{lineage:?}"
    );
}

/// The candidates of the single ambiguity an output column reports, as names.
fn ambiguity_candidates(result: &StatementAnalysis) -> Vec<String> {
    let Lineage::Partial { unresolved, .. } = &result.output.columns[0].lineage else {
        panic!(
            "expected an ambiguity: {:?}",
            result.output.columns[0].lineage
        );
    };
    let [Unresolved::Ambiguous { candidates, .. }] = &unresolved[..] else {
        panic!("expected one ambiguity: {unresolved:?}");
    };
    let mut names: Vec<String> = candidates
        .iter()
        .map(|candidate| match candidate {
            dlin_sqllineage::Candidate::Relation(relation) => relation.name.value.clone(),
            dlin_sqllineage::Candidate::Derived(Some(name)) => format!("derived {}", name.value),
            other => format!("{other:?}"),
        })
        .collect();
    names.sort();
    names
}

// Two relations sharing a range variable leave a merged name without an owner.
// The candidates are the relations that can carry the name, not everything in
// the FROM clause.
#[test]
fn duplicate_range_variables_list_only_the_relations_that_can_carry_the_name() {
    for dialect in [Dialect::Generic, Dialect::PostgreSql] {
        // A derived table that does not publish `a` cannot be what `a` means.
        let result = analyze(
            "SELECT a FROM t1 AS x JOIN t2 AS x USING (a), (SELECT 1 AS q) AS d",
            dialect,
            false,
        );
        assert_eq!(ambiguity_candidates(&result), ["t1", "t2"], "{dialect:?}");

        // A relation whose columns are unknown may publish it, so it stays.
        let result = analyze(
            "SELECT a FROM t1 AS x JOIN t2 AS x USING (a), u",
            dialect,
            false,
        );
        assert_eq!(
            ambiguity_candidates(&result),
            ["t1", "t2", "u"],
            "{dialect:?}"
        );

        // So does a derived table that publishes the name.
        let result = analyze(
            "SELECT a FROM t1 AS x JOIN t2 AS x USING (a), (SELECT 1 AS a) AS d",
            dialect,
            false,
        );
        assert_eq!(
            ambiguity_candidates(&result),
            ["derived d", "t1", "t2"],
            "{dialect:?}"
        );
    }
}

// In a WITH RECURSIVE list every CTE is visible to every other, itself and the
// later ones included; without RECURSIVE a CTE sees only the ones before it,
// and a later name is a physical relation.
#[test]
fn recursive_with_list_lets_a_cte_read_a_later_one() {
    for dialect in [Dialect::Generic, Dialect::PostgreSql] {
        let result = analyze(
            "WITH RECURSIVE x AS (SELECT id FROM y), y AS (SELECT id FROM t) SELECT id FROM x",
            dialect,
            false,
        );
        assert_eq!(sources(&result, "id"), [pair("t", "id")], "{dialect:?}");
        assert_eq!(read_names(&result), ["t"], "{dialect:?}");
    }
}

#[test]
fn non_recursive_with_list_binds_a_later_name_to_the_physical_relation() {
    for dialect in ALL_DIALECTS {
        let result = analyze(
            "WITH x AS (SELECT id FROM y), y AS (SELECT id FROM t) SELECT id FROM x",
            dialect,
            false,
        );
        assert_eq!(sources(&result, "id"), [pair("y", "id")], "{dialect:?}");
        let mut read = read_names(&result);
        read.sort();
        assert_eq!(read, ["t", "y"], "{dialect:?}");
    }
}

isolated_test!(recursive_ctes_that_read_each_other_terminate, || {
    for sql in [
        "WITH RECURSIVE a AS (SELECT id FROM b), b AS (SELECT id FROM a) SELECT id FROM a",
        "WITH RECURSIVE a AS (SELECT id FROM b), b AS (SELECT id FROM a) SELECT id FROM b",
        "WITH RECURSIVE a AS (SELECT id FROM b UNION ALL SELECT id FROM t), \
         b AS (SELECT id FROM a) SELECT id FROM a",
        "WITH RECURSIVE a AS (SELECT id FROM b), b AS (SELECT id FROM c), \
         c AS (SELECT id FROM a) SELECT * FROM a",
    ] {
        for dialect in [Dialect::Generic, Dialect::PostgreSql] {
            let result = analyze(sql, dialect, false);
            // The cycle is reported as not fully known, never as a source-free
            // or complete answer.
            for column in &result.output.columns {
                assert!(
                    matches!(&column.lineage, Lineage::Partial { unresolved, .. }
                        if !unresolved.is_empty()),
                    "{dialect:?} {sql}: {:?}",
                    column.lineage
                );
            }
        }
    }
});

// A CTE inside a LATERAL subquery sees the FROM items before the subquery.
#[test]
fn cte_inside_a_lateral_subquery_sees_the_outer_from_items() {
    for dialect in LATERAL_DIALECTS {
        for sql in [
            "SELECT d.v FROM t, LATERAL (WITH c AS (SELECT t.x AS v) SELECT v FROM c) AS d",
            "SELECT d.v FROM t, LATERAL (WITH c AS (SELECT x AS v) SELECT v FROM c) AS d",
        ] {
            let result = analyze(sql, dialect, false);
            assert_eq!(sources(&result, "v"), [pair("t", "x")], "{dialect:?} {sql}");
        }
    }
}

// The same CTE in an ordinary derived table does not.
#[test]
fn cte_inside_an_ordinary_derived_table_does_not_see_sibling_items() {
    let result = analyze(
        "SELECT d.v FROM t, (WITH c AS (SELECT t.x AS v) SELECT v FROM c) AS d",
        Dialect::PostgreSql,
        false,
    );
    assert!(known_sources(&result.output.columns[0]).is_empty());
}

const FORWARD_REFERENCE: &str =
    "WITH RECURSIVE x AS (SELECT id FROM y), y AS (SELECT id FROM t) SELECT id FROM x";

/// Dialects whose CTE visibility rule is not modelled.
const UNMODELLED_DIALECTS: [Dialect; 6] = [
    Dialect::MySql,
    Dialect::Hive,
    Dialect::Databricks,
    Dialect::Redshift,
    Dialect::Trino,
    Dialect::Spark,
];

/// Candidates of every ambiguity in an output column's unresolved part.
fn all_ambiguity_candidates(column: &dlin_sqllineage::OutputColumn) -> Vec<Vec<String>> {
    let Lineage::Partial { unresolved, .. } = &column.lineage else {
        return Vec::new();
    };
    unresolved
        .iter()
        .filter_map(|item| match item {
            Unresolved::Ambiguous { candidates, .. } => {
                let mut names: Vec<String> = candidates
                    .iter()
                    .map(|candidate| match candidate {
                        dlin_sqllineage::Candidate::Relation(relation) => {
                            format!("relation {}", relation.name.value)
                        }
                        dlin_sqllineage::Candidate::Cte(name) => format!("cte {}", name.value),
                        other => format!("{other:?}"),
                    })
                    .collect();
                names.sort();
                Some(names)
            }
            _ => None,
        })
        .collect()
}

// Whether a CTE can read one defined after it depends on the dialect.
#[test]
fn forward_cte_reference_follows_the_dialect() {
    for dialect in [
        Dialect::PostgreSql,
        Dialect::BigQuery,
        Dialect::Generic,
        Dialect::Ansi,
        Dialect::SQLite,
    ] {
        let result = analyze(FORWARD_REFERENCE, dialect, false);
        assert_eq!(sources(&result, "id"), [pair("t", "id")], "{dialect:?}");
        assert_eq!(read_names(&result), ["t"], "{dialect:?}");
    }
    // DuckDB, Snowflake and SQL Server see only earlier CTEs, even with
    // RECURSIVE: `y` is the physical relation.
    for dialect in [Dialect::DuckDb, Dialect::Snowflake, Dialect::MsSql] {
        let result = analyze(FORWARD_REFERENCE, dialect, false);
        assert_eq!(sources(&result, "id"), [pair("y", "id")], "{dialect:?}");
    }
}

#[test]
fn forward_cte_reference_without_the_keyword_follows_the_dialect() {
    let sql = FORWARD_REFERENCE.replace("WITH RECURSIVE", "WITH");
    // SQLite sees every CTE of the list without RECURSIVE too.
    let result = analyze(&sql, Dialect::SQLite, false);
    assert_eq!(sources(&result, "id"), [pair("t", "id")]);
    assert_eq!(read_names(&result), ["t"]);
    // SQL Server has no forward references.
    let result = analyze(&sql, Dialect::MsSql, false);
    assert_eq!(sources(&result, "id"), [pair("y", "id")]);
    // The standard dialects need the keyword.
    for dialect in [Dialect::Generic, Dialect::Ansi, Dialect::PostgreSql] {
        let result = analyze(&sql, dialect, false);
        assert_eq!(sources(&result, "id"), [pair("y", "id")], "{dialect:?}");
    }
}

#[test]
fn forward_cte_reference_in_an_unmodelled_dialect_is_not_assumed_physical() {
    for dialect in UNMODELLED_DIALECTS {
        for sql in [
            FORWARD_REFERENCE,
            "WITH x AS (SELECT id FROM y), y AS (SELECT id FROM t) SELECT id FROM x",
        ] {
            let result = analyze(sql, dialect, false);
            let column = &result.output.columns[0];
            assert!(known_sources(column).is_empty(), "{dialect:?} {sql}");
            assert_eq!(
                all_ambiguity_candidates(column),
                [["cte y", "relation y"]],
                "{dialect:?} {sql}"
            );
        }
    }
}

#[test]
fn earlier_ctes_are_visible_in_every_dialect() {
    for dialect in Dialect::ALL {
        let result = analyze(
            "WITH y AS (SELECT id FROM t), x AS (SELECT id FROM y) SELECT id FROM x",
            dialect,
            false,
        );
        assert_eq!(sources(&result, "id"), [pair("t", "id")], "{dialect:?}");
    }
}

const KEYWORD_LESS_SELF_REFERENCE: &str = "WITH c(n) AS (SELECT t.n FROM t UNION ALL \
     SELECT c.n + 1 FROM c WHERE c.n < 3) SELECT n FROM c";

// Snowflake lets a CTE read itself without RECURSIVE. It is the same
// recursive CTE as one written with the keyword, never a physical `c`.
#[test]
fn snowflake_cte_reads_itself_without_the_keyword() {
    let result = analyze(KEYWORD_LESS_SELF_REFERENCE, Dialect::Snowflake, false);
    let keyword = analyze(
        &KEYWORD_LESS_SELF_REFERENCE.replace("WITH", "WITH RECURSIVE"),
        Dialect::Snowflake,
        false,
    );
    assert_eq!(result.output.columns, keyword.output.columns);
    assert_eq!(read_names(&result), ["t"]);
    let column = &result.output.columns[0];
    assert_eq!(known_sources(column), [source("t", "n")]);
    assert!(
        matches!(&column.lineage, Lineage::Partial { unresolved, .. }
            if unresolved.iter().any(|item| matches!(item, Unresolved::Recursive))),
        "{:?}",
        column.lineage
    );
}

fn source(relation: &str, column: &str) -> dlin_sqllineage::SourceColumn {
    dlin_sqllineage::SourceColumn {
        relation: common::table(relation),
        column: Ident::unquoted(column),
    }
}

// SQLite and SQL Server also let a CTE read itself without the keyword.
#[test]
fn sqlite_and_sql_server_cte_read_themselves_without_the_keyword() {
    for dialect in [Dialect::SQLite, Dialect::MsSql] {
        let result = analyze(KEYWORD_LESS_SELF_REFERENCE, dialect, false);
        assert_eq!(read_names(&result), ["t"], "{dialect:?}");
        let column = &result.output.columns[0];
        assert_eq!(known_sources(column), [source("t", "n")], "{dialect:?}");
        assert!(
            matches!(&column.lineage, Lineage::Partial { unresolved, .. }
                if unresolved.iter().any(|item| matches!(item, Unresolved::Recursive))),
            "{dialect:?}: {:?}",
            column.lineage
        );
        assert!(all_ambiguity_candidates(column).is_empty(), "{dialect:?}");
    }
}

// DuckDB needs RECURSIVE for a CTE to read itself; without it the query is
// invalid and the name is reported as undecidable between the CTE and a
// physical relation, not silently bound to either.
#[test]
fn duckdb_cte_reading_itself_without_the_keyword_is_unresolved() {
    let result = analyze(KEYWORD_LESS_SELF_REFERENCE, Dialect::DuckDb, false);
    let column = &result.output.columns[0];
    assert_eq!(
        all_ambiguity_candidates(column),
        [["cte c", "relation c"]],
        "{:?}",
        column.lineage
    );
    assert!(!known_sources(column).contains(&source("c", "n")));

    let result = analyze(
        &KEYWORD_LESS_SELF_REFERENCE.replace("WITH", "WITH RECURSIVE"),
        Dialect::DuckDb,
        false,
    );
    assert_eq!(read_names(&result), ["t"]);
    assert!(all_ambiguity_candidates(&result.output.columns[0]).is_empty());
}

#[test]
fn self_reference_without_the_keyword_follows_the_dialect() {
    // PostgreSQL: without RECURSIVE the name is the physical relation `c`.
    let result = analyze(KEYWORD_LESS_SELF_REFERENCE, Dialect::PostgreSql, false);
    assert_eq!(read_names(&result), ["c", "t"]);
    assert_eq!(
        known_sources(&result.output.columns[0]),
        [source("c", "n"), source("t", "n")]
    );
    // An unmodelled dialect: undecidable.
    for dialect in UNMODELLED_DIALECTS {
        let result = analyze(KEYWORD_LESS_SELF_REFERENCE, dialect, false);
        assert_eq!(
            all_ambiguity_candidates(&result.output.columns[0]),
            [["cte c", "relation c"]],
            "{dialect:?}"
        );
        // With the keyword it is an ordinary recursive CTE.
        let result = analyze(
            &KEYWORD_LESS_SELF_REFERENCE.replace("WITH", "WITH RECURSIVE"),
            dialect,
            false,
        );
        assert_eq!(read_names(&result), ["t"], "{dialect:?}");
        assert!(
            all_ambiguity_candidates(&result.output.columns[0]).is_empty(),
            "{dialect:?}"
        );
    }
}

fn has_recursive_marker(column: &dlin_sqllineage::OutputColumn) -> bool {
    matches!(&column.lineage, Lineage::Partial { unresolved, .. }
        if unresolved.iter().any(|item| matches!(item, Unresolved::Recursive)))
}

const UNION_CTE: &str = "WITH RECURSIVE c AS (SELECT x FROM t UNION ALL SELECT y FROM u) \
     SELECT x FROM c";

// Being allowed to read itself does not make a CTE recursive: only a CTE that
// does read itself is. A UNION inside a CTE that never mentions its own name
// keeps both operands.
#[test]
fn a_union_cte_that_does_not_read_itself_keeps_both_operands() {
    let both = [pair("t", "x"), pair("u", "y")];
    // Dialects where the keyword allows a CTE to read itself.
    for dialect in [
        Dialect::PostgreSql,
        Dialect::BigQuery,
        Dialect::Generic,
        Dialect::Ansi,
        Dialect::DuckDb,
    ] {
        let result = analyze(UNION_CTE, dialect, false);
        assert_eq!(sources(&result, "x"), both, "{dialect:?}");
        assert_eq!(read_names(&result), ["t", "u"], "{dialect:?}");
    }
    // Dialects where a CTE may read itself with or without the keyword.
    for dialect in [Dialect::Snowflake, Dialect::SQLite, Dialect::MsSql] {
        for sql in [
            UNION_CTE.to_owned(),
            UNION_CTE.replace("WITH RECURSIVE", "WITH"),
        ] {
            let result = analyze(&sql, dialect, false);
            assert_eq!(sources(&result, "x"), both, "{dialect:?} {sql}");
            assert_eq!(read_names(&result), ["t", "u"], "{dialect:?} {sql}");
        }
    }
    // Without the keyword the other dialects never allowed self-reference.
    for dialect in [Dialect::PostgreSql, Dialect::Generic, Dialect::DuckDb] {
        let result = analyze(&UNION_CTE.replace("WITH RECURSIVE", "WITH"), dialect, false);
        assert_eq!(sources(&result, "x"), both, "{dialect:?}");
    }
}

#[test]
fn a_union_cte_that_does_not_read_itself_keeps_both_operands_with_a_catalog() {
    for dialect in [Dialect::PostgreSql, Dialect::Snowflake] {
        let result = analyze(UNION_CTE, dialect, true);
        assert_eq!(
            sources(&result, "x"),
            [pair("t", "x"), pair("u", "y")],
            "{dialect:?}"
        );
    }
}

// A set operation nested in a recursive CTE's body that does not contain the
// self-reference is an ordinary set operation.
#[test]
fn a_nested_union_in_a_recursive_cte_is_not_recursive() {
    let sql = "WITH RECURSIVE c(n) AS (SELECT n FROM t UNION ALL \
         SELECT d.n + 1 FROM c JOIN (SELECT n FROM t UNION ALL SELECT x FROM u) AS d ON TRUE) \
         SELECT n FROM c";
    let result = analyze(sql, Dialect::PostgreSql, false);
    let column = &result.output.columns[0];
    assert_eq!(known_sources(column), [source("t", "n")]);
    assert!(has_recursive_marker(column), "{:?}", column.lineage);
    // The derived table's own union is still read in full.
    assert_eq!(read_names(&result), ["t", "u"]);
}

#[test]
fn a_cte_that_reads_itself_is_recursive() {
    for dialect in [
        Dialect::PostgreSql,
        Dialect::BigQuery,
        Dialect::Generic,
        Dialect::Ansi,
        Dialect::DuckDb,
        Dialect::Snowflake,
        Dialect::SQLite,
        Dialect::MsSql,
    ] {
        let result = analyze(
            "WITH RECURSIVE c(n) AS (SELECT n FROM t UNION ALL SELECT n + 1 FROM c WHERE n < 3) \
             SELECT n FROM c",
            dialect,
            false,
        );
        let column = &result.output.columns[0];
        assert_eq!(known_sources(column), [source("t", "n")], "{dialect:?}");
        assert!(
            has_recursive_marker(column),
            "{dialect:?}: {:?}",
            column.lineage
        );
        assert_eq!(read_names(&result), ["t"], "{dialect:?}");
    }
}

// A WITH nested inside a CTE body shadows the CTE being defined, like any
// nearer CTE of the same name.
#[test]
fn a_nested_with_shadows_the_cte_being_defined() {
    let recursive = "WITH RECURSIVE c AS (SELECT v FROM \
         (WITH c AS (SELECT x AS v FROM t) SELECT v FROM c) AS d) SELECT v FROM c";
    let plain = recursive.replace("WITH RECURSIVE", "WITH");
    for (sql, dialect) in [
        (recursive, Dialect::PostgreSql),
        (recursive, Dialect::Generic),
        (recursive, Dialect::DuckDb),
        (&plain[..], Dialect::Snowflake),
        (recursive, Dialect::Snowflake),
        (&plain[..], Dialect::SQLite),
        (&plain[..], Dialect::MsSql),
    ] {
        let result = analyze(sql, dialect, false);
        assert_eq!(sources(&result, "v"), [pair("t", "x")], "{dialect:?} {sql}");
        assert_eq!(read_names(&result), ["t"], "{dialect:?} {sql}");
    }
}

// Without a nearer CTE of that name, a reference from a nested query is the
// CTE being defined, and so recursive only when it really is.
#[test]
fn a_nested_with_reading_the_cte_being_defined_is_recursive() {
    let sql = "WITH RECURSIVE c AS (SELECT x AS v FROM t UNION ALL \
         SELECT v FROM (WITH d AS (SELECT v FROM c) SELECT v FROM d) AS e) SELECT v FROM c";
    for dialect in [Dialect::PostgreSql, Dialect::Generic, Dialect::DuckDb] {
        let result = analyze(sql, dialect, false);
        let column = &result.output.columns[0];
        assert_eq!(known_sources(column), [source("t", "x")], "{dialect:?}");
        assert!(
            has_recursive_marker(column),
            "{dialect:?}: {:?}",
            column.lineage
        );
        assert_eq!(read_names(&result), ["t"], "{dialect:?}");
    }
}

#[test]
fn a_nested_with_reading_a_finished_cte_is_an_ordinary_reference() {
    let sql = "WITH c AS (SELECT x AS v FROM t UNION ALL SELECT y FROM u) \
         SELECT v FROM (WITH d AS (SELECT v FROM c) SELECT v FROM d) AS e";
    for dialect in [Dialect::PostgreSql, Dialect::Snowflake, Dialect::DuckDb] {
        let result = analyze(sql, dialect, false);
        assert_eq!(
            sources(&result, "v"),
            [pair("t", "x"), pair("u", "y")],
            "{dialect:?}"
        );
    }
}

// A nested query that reads the outer CTE by the name an inner CTE of the
// same name is still being defined is not the outer one: the inner is a
// different CTE, hidden from its own body in PostgreSQL.
#[test]
fn an_inner_cte_reading_its_own_name_does_not_read_the_outer_cte() {
    let sql = "WITH c AS (SELECT x AS v FROM t) \
         SELECT v FROM (WITH c AS (SELECT v FROM c) SELECT v FROM c) AS e";
    let result = analyze(sql, Dialect::PostgreSql, false);
    assert_eq!(sources(&result, "v"), [pair("t", "x")]);
    assert_eq!(read_names(&result), ["t"]);
}
