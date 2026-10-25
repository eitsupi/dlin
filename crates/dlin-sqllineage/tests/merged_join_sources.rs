//! A column merged by `USING` or `NATURAL` takes its value from the join's
//! contributing side: both sides for an inner or full join, and the
//! preserved side for a left or right join. The lineage of an unqualified
//! reference lists those relations in FROM order, whether they are physical
//! tables, CTEs or derived tables.

use dlin_sqllineage::{
    AnalyzeOptions, AnalyzeResult, CatalogProvider, ColumnMapping, ColumnOrigin, Dialect, TableRef,
    TransformKind, analyze,
};

const RUNS: usize = 50;

/// Columns of known tables; a table not listed is unknown.
struct Columns(&'static [(&'static str, &'static [&'static str])]);

impl CatalogProvider for Columns {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        self.0
            .iter()
            .find(|(name, _)| *name == table.table)
            .map(|(_, columns)| columns.iter().map(|column| (*column).to_string()).collect())
    }

    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        let owners: Vec<&TableRef> = candidates
            .iter()
            .filter(|candidate| {
                self.list_columns(candidate)
                    .is_some_and(|columns| columns.iter().any(|known| known == column))
            })
            .collect();
        (owners.len() == 1).then(|| owners[0].clone())
    }
}

fn run_with_dialect(
    sql: &str,
    catalog: Option<&'static [(&'static str, &'static [&'static str])]>,
    dialect: Dialect,
) -> Vec<AnalyzeResult> {
    analyze(
        sql,
        AnalyzeOptions {
            dialect,
            catalog: catalog.map(|columns| Box::new(Columns(columns)) as Box<dyn CatalogProvider>),
            ..AnalyzeOptions::default()
        },
    )
    .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
}

/// Analyze `sql` repeatedly, require identical output every time, and return
/// the first mapping.
fn mapping(
    sql: &str,
    catalog: Option<&'static [(&'static str, &'static [&'static str])]>,
) -> ColumnMapping {
    mapping_with_dialect(sql, catalog, Dialect::Generic)
}

fn mapping_with_dialect(
    sql: &str,
    catalog: Option<&'static [(&'static str, &'static [&'static str])]>,
    dialect: Dialect,
) -> ColumnMapping {
    let first = run_with_dialect(sql, catalog, dialect);
    let expected = format!("{first:#?}");
    for attempt in 1..RUNS {
        let again = format!("{:#?}", run_with_dialect(sql, catalog, dialect));
        assert_eq!(
            expected, again,
            "run {attempt} differs from run 0 for: {sql}"
        );
    }
    first
        .into_iter()
        .next()
        .expect("one statement")
        .columns
        .mappings
        .into_iter()
        .next()
        .expect("one column")
}

fn show(origin: &ColumnOrigin) -> String {
    match origin {
        ColumnOrigin::Concrete { table, column } => format!("{}.{column}", table.table),
        ColumnOrigin::Ambiguous { column, candidates } => {
            let names: Vec<&str> = candidates
                .iter()
                .map(|table| table.table.as_str())
                .collect();
            format!("?{column}({})", names.join(","))
        }
        other => panic!("unexpected origin {other:?}"),
    }
}

fn sources(
    sql: &str,
    catalog: Option<&'static [(&'static str, &'static [&'static str])]>,
) -> Vec<String> {
    mapping(sql, catalog).sources.iter().map(show).collect()
}

fn plain(sql: &str) -> Vec<String> {
    sources(sql, None)
}

fn plain_with_dialect(sql: &str, dialect: Dialect) -> Vec<String> {
    mapping_with_dialect(sql, None, dialect)
        .sources
        .iter()
        .map(show)
        .collect()
}

#[test]
fn using_join_of_two_tables_lists_both() {
    assert_eq!(
        plain("SELECT x FROM t1 JOIN t2 USING (x)"),
        ["t1.x", "t2.x"]
    );
}

#[test]
fn using_join_variants_list_only_the_contributing_sides() {
    for (join, expected) in [
        ("JOIN", vec!["t1.x", "t2.x"]),
        ("INNER JOIN", vec!["t1.x", "t2.x"]),
        ("LEFT JOIN", vec!["t1.x"]),
        ("LEFT OUTER JOIN", vec!["t1.x"]),
        ("RIGHT JOIN", vec!["t2.x"]),
        ("RIGHT OUTER JOIN", vec!["t2.x"]),
        ("FULL JOIN", vec!["t1.x", "t2.x"]),
        ("FULL OUTER JOIN", vec!["t1.x", "t2.x"]),
    ] {
        assert_eq!(
            plain(&format!("SELECT x FROM t1 {join} t2 USING (x)")),
            expected,
            "{join}"
        );
    }
}

#[test]
fn natural_join_variants_list_only_the_contributing_sides() {
    const BOTH_HAVE_X: &[(&str, &[&str])] = &[("t1", &["x"]), ("t2", &["x"])];
    for (join, expected) in [
        ("NATURAL JOIN", vec!["t1.x", "t2.x"]),
        ("NATURAL INNER JOIN", vec!["t1.x", "t2.x"]),
        ("NATURAL LEFT JOIN", vec!["t1.x"]),
        ("NATURAL RIGHT JOIN", vec!["t2.x"]),
        ("NATURAL FULL JOIN", vec!["t1.x", "t2.x"]),
    ] {
        assert_eq!(
            sources(&format!("SELECT x FROM t1 {join} t2"), Some(BOTH_HAVE_X)),
            expected,
            "{join}"
        );
    }
}

#[test]
fn natural_join_chains_fold_the_contributing_sources_left_to_right() {
    const ALL_HAVE_X: &[(&str, &[&str])] = &[("t1", &["x"]), ("t2", &["x"]), ("t3", &["x"])];
    assert_eq!(
        sources(
            "SELECT x FROM t1 NATURAL LEFT JOIN t2 NATURAL FULL JOIN t3",
            Some(ALL_HAVE_X)
        ),
        ["t1.x", "t3.x"]
    );
    assert_eq!(
        sources(
            "SELECT x FROM t1 NATURAL RIGHT JOIN t2 NATURAL INNER JOIN t3",
            Some(ALL_HAVE_X)
        ),
        ["t2.x", "t3.x"]
    );
}

#[test]
fn special_join_variants_use_their_contributing_side() {
    for (join, expected) in [
        ("SEMI JOIN", vec!["t1.x"]),
        ("LEFT SEMI JOIN", vec!["t1.x"]),
        ("ANTI JOIN", vec!["t1.x"]),
        ("LEFT ANTI JOIN", vec!["t1.x"]),
        ("RIGHT SEMI JOIN", vec!["t2.x"]),
        ("RIGHT ANTI JOIN", vec!["t2.x"]),
    ] {
        assert_eq!(
            plain(&format!("SELECT x FROM t1 {join} t2 USING (x)")),
            expected,
            "{join}"
        );
    }

    assert_eq!(
        plain_with_dialect("SELECT x FROM t1 CROSS JOIN t2 USING (x)", Dialect::MySql),
        ["t1.x", "t2.x"]
    );
    assert_eq!(
        plain_with_dialect(
            "SELECT x FROM t1 STRAIGHT_JOIN t2 USING (x)",
            Dialect::MySql
        ),
        ["t1.x", "t2.x"]
    );
    assert_eq!(
        plain(
            "SELECT x FROM t1 ASOF JOIN t2 \
             MATCH_CONDITION (t1.ts >= t2.ts) USING (x)"
        ),
        ["t1.x"]
    );
}

#[test]
fn using_join_chains_fold_sources_left_to_right() {
    assert_eq!(
        plain("SELECT x FROM t1 JOIN t2 USING (x) JOIN t3 USING (x)"),
        ["t1.x", "t2.x", "t3.x"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 LEFT JOIN t2 USING (x) FULL JOIN t3 USING (x)"),
        ["t1.x", "t3.x"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 JOIN t2 USING (x) RIGHT JOIN t3 USING (x)"),
        ["t3.x"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 FULL JOIN t2 USING (x) LEFT JOIN t3 USING (x)"),
        ["t1.x", "t2.x"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 LEFT JOIN (t2 RIGHT JOIN t3 USING (x)) USING (x)"),
        ["t1.x"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 FULL JOIN (t2 RIGHT JOIN t3 USING (x)) USING (x)"),
        ["t1.x", "t3.x"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 JOIN (t2 JOIN t3 USING (x)) USING (x)"),
        ["t1.x", "t2.x", "t3.x"]
    );
}

/// `NATURAL` merges only the columns both sides have. Without a catalog the
/// engine does not know that a table has the column, so it does not claim a
/// merge: that would report a source that may not exist.
#[test]
fn natural_join_of_tables_of_unknown_shape_stays_ambiguous() {
    assert_eq!(plain("SELECT x FROM t1 NATURAL JOIN t2"), ["?x(t1,t2)"]);
    assert_eq!(
        plain("SELECT x FROM t1 NATURAL LEFT JOIN t2"),
        ["?x(t1,t2)"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 NATURAL JOIN t2 NATURAL JOIN t3"),
        ["?x(t1,t2,t3)"]
    );
}

#[test]
fn only_the_named_columns_are_merged() {
    assert_eq!(plain("SELECT y FROM t1 JOIN t2 USING (x)"), ["?y(t1,t2)"]);
    assert_eq!(
        plain("SELECT x FROM t1 JOIN t2 ON t1.id = t2.id"),
        ["?x(t1,t2)"]
    );
}

#[test]
fn qualified_reference_is_not_merged() {
    assert_eq!(plain("SELECT t2.x FROM t1 JOIN t2 USING (x)"), ["t2.x"]);
}

#[test]
fn merged_column_inside_an_expression_lists_both() {
    let mapping = mapping("SELECT x + 1 AS y FROM t1 JOIN t2 USING (x)", None);
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["t1.x", "t2.x"]
    );
    assert_eq!(mapping.transform, TransformKind::Expression);
}

#[test]
fn self_join_lists_the_table_once() {
    assert_eq!(plain("SELECT x FROM t AS p JOIN t AS q USING (x)"), ["t.x"]);
}

/// A comma-joined table is outside the merge, and nothing says it lacks the
/// column, so the name stays a real choice among the tables.
#[test]
fn comma_joined_table_keeps_the_name_ambiguous() {
    assert_eq!(
        plain("SELECT x FROM t1 JOIN t2 USING (x), t3"),
        ["?x(t1,t2,t3)"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 NATURAL JOIN t2 CROSS JOIN t3"),
        ["?x(t1,t2,t3)"]
    );
    assert_eq!(
        plain("SELECT x FROM t1 JOIN t2 USING (x) JOIN t3 ON t1.id = t3.id"),
        ["?x(t1,t2,t3)"]
    );
}

const KNOWN: &[(&str, &[&str])] = &[("t1", &["x", "y"]), ("t2", &["x", "z"]), ("t3", &["w"])];

#[test]
fn catalog_ruling_out_the_extra_table_resolves_the_merge() {
    assert_eq!(
        sources("SELECT x FROM t1 JOIN t2 USING (x), t3", Some(KNOWN)),
        ["t1.x", "t2.x"]
    );
    assert_eq!(
        sources(
            "SELECT x FROM t1 NATURAL JOIN t2 CROSS JOIN t3",
            Some(KNOWN)
        ),
        ["t1.x", "t2.x"]
    );
}

#[test]
fn catalog_that_only_rules_out_the_extra_table_is_enough() {
    const ONLY_T3: &[(&str, &[&str])] = &[("t3", &["w"])];
    assert_eq!(
        sources("SELECT x FROM t1 JOIN t2 USING (x), t3", Some(ONLY_T3)),
        ["t1.x", "t2.x"]
    );
}

#[test]
fn catalog_that_gives_the_extra_table_the_column_keeps_it_ambiguous() {
    const ALL_HAVE_X: &[(&str, &[&str])] = &[("t1", &["x"]), ("t2", &["x"]), ("t3", &["x"])];
    assert_eq!(
        sources("SELECT x FROM t1 JOIN t2 USING (x), t3", Some(ALL_HAVE_X)),
        ["?x(t1,t2,t3)"]
    );
}

#[test]
fn catalog_does_not_change_a_using_merge_of_known_tables() {
    assert_eq!(
        sources("SELECT x FROM t1 JOIN t2 USING (x)", Some(KNOWN)),
        ["t1.x", "t2.x"]
    );
    assert_eq!(
        sources(
            "SELECT x FROM t1 LEFT JOIN t2 USING (x) FULL JOIN t3 USING (x)",
            Some(&[("t1", &["x"]), ("t2", &["x"]), ("t3", &["x"])])
        ),
        ["t1.x", "t3.x"]
    );
}

/// `NATURAL` merges only the columns both sides have, so a table the catalog
/// knows to lack the name is not part of the merge.
#[test]
fn natural_join_follows_the_catalog_for_which_tables_hold_the_column() {
    // x is shared by t1 and t2, y belongs to t1 alone and z to t2 alone.
    assert_eq!(
        sources("SELECT x FROM t1 NATURAL JOIN t2", Some(KNOWN)),
        ["t1.x", "t2.x"]
    );
    assert_eq!(
        sources("SELECT y FROM t1 NATURAL JOIN t2", Some(KNOWN)),
        ["t1.y"]
    );
    assert_eq!(
        sources("SELECT z FROM t1 NATURAL JOIN t2", Some(KNOWN)),
        ["t2.z"]
    );
    assert_eq!(
        sources(
            "SELECT x FROM t1 NATURAL JOIN t2 NATURAL JOIN t3",
            Some(KNOWN)
        ),
        ["t1.x", "t2.x"]
    );
}

/// A table the catalog does not describe is not confirmed to share the column,
/// so it is not merged; the catalog's single confirmed owner is then picked as
/// for any ambiguous column.
#[test]
fn natural_join_does_not_merge_a_table_the_catalog_does_not_confirm() {
    assert_eq!(
        sources("SELECT x FROM t1 NATURAL JOIN t2", Some(&[("t1", &["x"])])),
        ["t1.x"]
    );
    assert_eq!(
        sources(
            "WITH a AS (SELECT x FROM s1) SELECT x FROM a NATURAL JOIN t",
            Some(&[("other", &["x"])])
        ),
        ["s1.x"]
    );
}

#[test]
fn ordinary_join_still_uses_the_catalog_to_pick_an_owner() {
    assert_eq!(
        sources("SELECT y FROM t1 JOIN t2 ON t1.x = t2.x", Some(KNOWN)),
        ["t1.y"]
    );
    assert_eq!(
        sources("SELECT x FROM t1 JOIN t2 ON t1.x = t2.x", Some(KNOWN)),
        ["?x(t1,t2)"]
    );
}

#[test]
fn ctes_joined_by_using_list_both_origins() {
    assert_eq!(
        plain(
            "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
             SELECT x FROM a JOIN b USING (x)"
        ),
        ["s1.x", "s2.x"]
    );
    assert_eq!(
        plain(
            "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
             SELECT x FROM a FULL JOIN b USING (x)"
        ),
        ["s1.x", "s2.x"]
    );
}

#[test]
fn ctes_reaching_the_same_column_list_it_once() {
    assert_eq!(
        plain(
            "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s1) \
             SELECT x FROM a JOIN b USING (x)"
        ),
        ["s1.x"]
    );
}

#[test]
fn table_and_cte_sources_follow_join_kind() {
    assert_eq!(
        plain("WITH a AS (SELECT x FROM s1) SELECT x FROM t LEFT JOIN a USING (x)"),
        ["t.x"]
    );
    assert_eq!(
        plain("WITH a AS (SELECT x FROM s1) SELECT x FROM a FULL JOIN t USING (x)"),
        ["s1.x", "t.x"]
    );
    assert_eq!(
        sources(
            "WITH a AS (SELECT x FROM s1) SELECT x FROM a NATURAL JOIN t",
            Some(&[("t", &["x"])])
        ),
        ["s1.x", "t.x"]
    );
    assert_eq!(
        plain(
            "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
             SELECT x FROM t1 JOIN a USING (x) JOIN t2 USING (x) JOIN b USING (x)"
        ),
        ["t1.x", "s1.x", "t2.x", "s2.x"]
    );
}

/// A CTE that publishes the name outranks physical tables of unknown shape,
/// so a table that is not merged with it does not compete.
#[test]
fn unmerged_table_does_not_compete_with_a_cte_provider() {
    assert_eq!(
        plain("WITH a AS (SELECT x FROM s1) SELECT x FROM a JOIN t USING (x), t3"),
        ["s1.x", "t.x"]
    );
    assert_eq!(
        plain("WITH a AS (SELECT x FROM s1) SELECT x FROM a JOIN t ON a.id = t.id"),
        ["s1.x"]
    );
}

#[test]
fn second_cte_outside_the_merge_keeps_the_name_ambiguous() {
    assert_eq!(
        plain(
            "WITH a AS (SELECT x FROM s1), b AS (SELECT x FROM s2) \
             SELECT x FROM t JOIN a USING (x), b"
        ),
        ["?x(a,b)"]
    );
}

#[test]
fn derived_tables_and_tables_merge_like_ctes() {
    assert_eq!(
        plain("SELECT x FROM (SELECT x FROM s1) d JOIN t USING (x)"),
        ["s1.x", "t.x"]
    );
    assert_eq!(
        plain(
            "SELECT x FROM (SELECT x FROM s1) d1 JOIN (SELECT x FROM s2) d2 USING (x) \
             JOIN t USING (x)"
        ),
        ["s1.x", "s2.x", "t.x"]
    );
}

#[test]
fn aggregation_of_one_side_marks_the_merged_column() {
    let mapping = mapping(
        "WITH a AS (SELECT SUM(v) AS x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a JOIN b USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s1.v", "s2.x"]
    );
    assert_eq!(mapping.transform, TransformKind::Aggregation);

    let mapping = self::mapping(
        "WITH a AS (SELECT SUM(v) AS x FROM s1) SELECT x FROM t FULL JOIN a USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["t.x", "s1.v"]
    );
    assert_eq!(mapping.transform, TransformKind::Aggregation);
}

#[test]
fn aggregation_propagates_only_from_a_contributing_side() {
    let mapping = self::mapping(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT SUM(v) AS x FROM s2) \
         SELECT x FROM a LEFT JOIN b USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s1.x"]
    );
    assert_eq!(mapping.transform, TransformKind::Direct);

    let mapping = self::mapping(
        "WITH a AS (SELECT SUM(v) AS x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a LEFT JOIN b USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s1.v"]
    );
    assert_eq!(mapping.transform, TransformKind::Aggregation);

    let mapping = self::mapping(
        "WITH a AS (SELECT SUM(v) AS x FROM s1), b AS (SELECT x FROM s2) \
         SELECT x FROM a RIGHT JOIN b USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s2.x"]
    );
    assert_eq!(mapping.transform, TransformKind::Direct);

    let mapping = self::mapping(
        "WITH a AS (SELECT x FROM s1), b AS (SELECT SUM(v) AS x FROM s2) \
         SELECT x FROM a RIGHT JOIN b USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s2.v"]
    );
    assert_eq!(mapping.transform, TransformKind::Aggregation);
}

#[test]
fn excluded_aggregation_does_not_return_when_a_later_full_join_adds_sources() {
    let mapping = mapping(
        "WITH a AS (SELECT SUM(v) AS x FROM s1), b AS (SELECT x FROM s2), \
         c AS (SELECT x FROM s3), d AS (SELECT x FROM s4) \
         SELECT x FROM a JOIN b USING (x) RIGHT JOIN c USING (x) FULL JOIN d USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s3.x", "s4.x"]
    );
    assert_eq!(mapping.transform, TransformKind::Direct);
}

#[test]
fn transform_of_a_nested_cte_is_followed_through_the_merge() {
    let mapping = mapping(
        "WITH base AS (SELECT SUM(v) AS x FROM s1), a AS (SELECT x FROM base), \
         b AS (SELECT x FROM s2) \
         SELECT x FROM a JOIN b USING (x)",
        None,
    );
    assert_eq!(
        mapping.sources.iter().map(show).collect::<Vec<_>>(),
        ["s1.v", "s2.x"]
    );
    assert_eq!(mapping.transform, TransformKind::Aggregation);
}

#[test]
fn direct_columns_stay_direct() {
    let mapping = mapping("SELECT x FROM t1 JOIN t2 USING (x)", None);
    assert_eq!(mapping.transform, TransformKind::Direct);
}
