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
        sql: "WITH {a} AS (SELECT x FROM t) SELECT {a}.x FROM {a}",
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
