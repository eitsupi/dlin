mod common;

use common::{analyze_one_with, expect_rendered_in};
use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, CatalogUnknownSegment, Dialect, Ident, Lineage,
    Relation, SchemaCoverage, SchemaOrder, Unresolved,
};
use std::borrow::Cow;

#[test]
fn scalar_and_array_subqueries_use_the_same_value_input_path() {
    expect_rendered_in(
        "SELECT ARRAY(SELECT x FROM t) AS arr",
        Dialect::BigQuery,
        &["arr#0 C[t.x] Expression", "Closed"],
    );
    expect_rendered_in(
        "SELECT (SELECT x FROM t) AS value",
        Dialect::Generic,
        &["value#0 C[t.x] Expression", "Closed"],
    );
}

#[test]
fn in_subquery_projection_is_a_value_input_but_exists_projection_is_not() {
    expect_rendered_in(
        "SELECT t.a IN (SELECT u.b FROM u) AS p FROM t",
        Dialect::BigQuery,
        &["p#0 C[t.a, u.b] Expression", "Closed"],
    );
    expect_rendered_in(
        "SELECT EXISTS (SELECT u.b FROM u WHERE u.b > 0) AS p",
        Dialect::PostgreSql,
        &["p#0 C[] Expression", "Closed"],
    );
}

#[test]
fn subqueries_with_non_scalar_output_shapes_are_unresolved() {
    for sql in [
        "SELECT (SELECT x, y FROM t) AS value",
        "SELECT (SELECT * FROM t) AS value",
        "SELECT ARRAY(SELECT x, y FROM t) AS arr",
    ] {
        let result = analyze_one_with(sql, Dialect::BigQuery, None);
        assert_eq!(result.output.columns.len(), 1, "{sql}");
        assert!(
            matches!(
                &result.output.columns[0].lineage,
                Lineage::Partial { known, unresolved }
                    if known.is_empty()
                        && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "subquery value output")
            ),
            "{sql}: {:?}",
            result.output.columns[0].lineage
        );
    }
}

#[test]
fn cte_and_derived_aliases_rename_outputs_by_position() {
    expect_rendered_in(
        "WITH c(b, a) AS (SELECT a, b FROM t) SELECT b, a FROM c",
        Dialect::PostgreSql,
        &["b#0 C[t.a]", "a#1 C[t.b]", "Closed"],
    );
    expect_rendered_in(
        "SELECT c.b, c.a FROM (SELECT a, b FROM t) AS c(b, a)",
        Dialect::PostgreSql,
        &["b#0 C[t.a]", "a#1 C[t.b]", "Closed"],
    );
}

#[test]
fn short_alias_lists_rename_only_the_output_prefix() {
    for dialect in [
        Dialect::Generic,
        Dialect::PostgreSql,
        Dialect::DuckDb,
        Dialect::Snowflake,
    ] {
        expect_rendered_in(
            "WITH c(x) AS (SELECT a, b FROM t) SELECT x, b FROM c",
            dialect,
            &["x#0 C[t.a]", "b#1 C[t.b]", "Closed"],
        );
        expect_rendered_in(
            "SELECT d.x, d.b FROM (SELECT a, b FROM t) AS d(x)",
            dialect,
            &["x#0 C[t.a]", "b#1 C[t.b]", "Closed"],
        );
    }
}

#[test]
fn aliases_cover_duplicate_source_names_anonymous_expressions_and_delegates() {
    expect_rendered_in(
        "WITH c(first, second) AS (SELECT t.id, u.id FROM t CROSS JOIN u) SELECT first, second FROM c",
        Dialect::Generic,
        &["first#0 C[t.id]", "second#1 C[u.id]", "Closed"],
    );
    expect_rendered_in(
        "WITH c(x) AS (SELECT 1) SELECT x FROM c",
        Dialect::Generic,
        &["x#0 C[]", "Closed"],
    );
    expect_rendered_in(
        "WITH c(y, x) AS (SELECT a, b FROM t UNION ALL SELECT c, d FROM u) SELECT y, x FROM c",
        Dialect::Generic,
        &["y#0 C[t.a, u.c]", "x#1 C[t.b, u.d]", "Closed"],
    );
    expect_rendered_in(
        "WITH c(x, y) AS (WITH inner AS (SELECT a, b FROM t) SELECT a, b FROM inner) SELECT x, y FROM c",
        Dialect::Generic,
        &["x#0 C[t.a]", "y#1 C[t.b]", "Closed"],
    );
}

#[test]
fn alias_count_mismatch_does_not_guess_a_column_mapping() {
    let result = analyze_one_with(
        "WITH c(x, y) AS (SELECT a FROM t) SELECT x FROM c",
        Dialect::Generic,
        None,
    );
    assert_eq!(result.output.columns.len(), 1);
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { known, unresolved }
            if known.is_empty()
                && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "column alias count mismatch")
    ));
}

#[test]
fn unknown_width_aliases_keep_confirmed_prefix_and_suffix_names() {
    let sql =
        "WITH c(a, alias_y) AS (SELECT a, * EXCLUDE(alias_y) FROM t) SELECT a, alias_y FROM c";
    let result = Analyzer::new(Dialect::Generic)
        .with_catalog(&StarCatalog::partial_proven())
        .analyze(sql)
        .expect("query parses")
        .into_single()
        .expect("one statement");
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Complete(sources)
            if sources.len() == 1 && sources[0].column.value == "a"
    ));
    assert!(
        matches!(
            &result.output.columns[1].lineage,
            Lineage::Complete(sources)
                if sources.len() == 1 && sources[0].column.value == "a"
        ),
        "{:?}",
        result.output.columns[1].lineage
    );

    let result = analyze_one_with(
        "WITH c(x, y) AS (SELECT a, * FROM t) SELECT x, y FROM c",
        Dialect::Generic,
        None,
    );
    assert_eq!(result.output.columns.len(), 2);
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { known, unresolved }
            if known.is_empty() && !unresolved.is_empty()
    ));
    assert!(matches!(
        &result.output.columns[1].lineage,
        Lineage::Partial { known, unresolved }
            if known.is_empty() && !unresolved.is_empty()
    ));

    expect_rendered_in(
        "WITH c(alias_x, alias_y) AS (SELECT t.a, t.* EXCLUDE(alias_x, alias_y) FROM t) SELECT alias_x, alias_y FROM c",
        Dialect::Generic,
        &[
            "alias_x#0 C[t.a]",
            "alias_y#1 P[ | Unsupported { what: \"column alias position unknown\" }]",
            "Closed",
        ],
    );
    expect_rendered_in(
        "WITH c(alias_x, alias_y) AS (SELECT t.a, t.* EXCLUDE(alias_x, alias_y) FROM t) SELECT * EXCLUDE(alias_y) FROM c",
        Dialect::Generic,
        &["alias_x#0 C[t.a]", "Open[? after 0; ? after -]"],
    );
    let filtered_alias = analyze_one_with(
        "WITH c(alias_x, alias_y) AS (SELECT t.a, t.* EXCLUDE(alias_x, alias_y) FROM t) SELECT d.alias_y FROM (SELECT * EXCLUDE(alias_y) FROM c) AS d",
        Dialect::Generic,
        None,
    );
    assert!(
        matches!(
            &filtered_alias.output.columns[0].lineage,
            Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && unresolved.iter().any(|reason| matches!(reason, Unresolved::Unbound { column } if column.value == "alias_y"))
                    && unresolved.iter().all(|reason| !matches!(reason, Unresolved::Unsupported { what: "column alias position unknown" }))
        ),
        "{:?}",
        filtered_alias.output.columns[0].lineage
    );
}

#[test]
fn aliases_over_an_unknown_interval_keep_alias_and_original_names_unresolved() {
    let result = analyze_one_with(
        "WITH c(x) AS (SELECT * FROM t) SELECT x, a FROM c",
        Dialect::Generic,
        None,
    );
    assert_eq!(result.output.columns.len(), 2);
    for column in &result.output.columns {
        assert!(
            matches!(
                &column.lineage,
                Lineage::Partial { known, unresolved }
                    if known.is_empty()
                        && !unresolved.is_empty()
                        && unresolved.iter().all(|reason| matches!(reason,
                            Unresolved::Unverified { .. }
                                | Unresolved::Unsupported { what: "column alias position unknown" }
                        ))
            ),
            "{:?}",
            column.lineage
        );
    }
}

#[test]
fn dialects_apply_unsupported_and_overlong_alias_lists_conservatively() {
    for (sql, dialect) in [Dialect::PostgreSql, Dialect::Snowflake]
        .into_iter()
        .flat_map(|dialect| {
            [
        "WITH c(x, y) AS (SELECT 1 AS a) SELECT x FROM c",
        "SELECT d.x FROM (SELECT 1 AS a) AS d(x, y)",
        "WITH c(x, y) AS (SELECT a FROM t UNION ALL SELECT b FROM u) SELECT x FROM c",
        "WITH c(x, y) AS (WITH nested AS (SELECT a FROM t) SELECT * FROM nested) SELECT x FROM c",
    ].map(|sql| (sql, dialect))
        })
    {
        let result = analyze_one_with(sql, dialect, None);
        assert!(
            matches!(
                &result.output.columns[0].lineage,
                Lineage::Partial { known, unresolved }
                    if known.is_empty()
                        && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "column alias count mismatch")
            ),
            "{sql}: {:?}",
            result.output.columns[0].lineage
        );
    }

    let duckdb_cte = analyze_one_with(
        "WITH c(x, ignored) AS (SELECT 1 AS a) SELECT * FROM c",
        Dialect::DuckDb,
        None,
    );
    assert_eq!(duckdb_cte.output.columns.len(), 1);
    assert!(matches!(
        &duckdb_cte.output.columns[0].name,
        dlin_sqllineage::OutputName::Explicit(name) if name.value == "x"
    ));

    let duckdb_derived = analyze_one_with(
        "SELECT d.x FROM (SELECT 1 AS a) AS d(x, ignored)",
        Dialect::DuckDb,
        None,
    );
    assert!(matches!(
        &duckdb_derived.output.columns[0].lineage,
        Lineage::Partial { known, unresolved }
            if known.is_empty()
                && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "column alias count mismatch")
    ));

    let bq =
        Analyzer::new(Dialect::BigQuery).analyze("WITH c(x) AS (SELECT 1 AS a) SELECT x FROM c");
    if let Ok(result) = bq {
        let result = result.into_single().expect("one statement");
        assert!(matches!(
            &result.output.columns[0].lineage,
            Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "column aliases unsupported by dialect")
        ));
    }

    let bq_derived =
        Analyzer::new(Dialect::BigQuery).analyze("SELECT d.x FROM (SELECT 1 AS a) AS d(x)");
    if let Ok(result) = bq_derived {
        let result = result.into_single().expect("one statement");
        assert!(matches!(
            &result.output.columns[0].lineage,
            Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "column aliases unsupported by dialect")
        ));
    }
}

struct StarCatalog {
    order: SchemaOrder,
    coverage: SchemaCoverage,
    columns: Vec<CatalogColumn>,
    unknown_segments: Vec<CatalogUnknownSegment>,
}

impl StarCatalog {
    fn new(order: SchemaOrder) -> Self {
        Self {
            order,
            coverage: SchemaCoverage::Exhaustive,
            columns: ["x", "y"]
                .into_iter()
                .enumerate()
                .map(|(ordinal, name)| CatalogColumn {
                    name: Ident::unquoted(name),
                    ordinal: (order == SchemaOrder::Proven).then_some(ordinal),
                    data_type: None,
                    fields: None,
                })
                .collect(),
            unknown_segments: Vec::new(),
        }
    }

    fn partial_proven() -> Self {
        Self {
            order: SchemaOrder::Proven,
            coverage: SchemaCoverage::Partial,
            columns: ["a", "b"]
                .into_iter()
                .enumerate()
                .map(|(ordinal, name)| CatalogColumn {
                    name: Ident::unquoted(name),
                    ordinal: Some(ordinal),
                    data_type: None,
                    fields: None,
                })
                .collect(),
            unknown_segments: vec![CatalogUnknownSegment {
                after_ordinal: Some(1),
            }],
        }
    }
}

impl Catalog for StarCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name.value == "t").then(|| CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Borrowed(&self.columns),
            coverage: self.coverage,
            order: self.order,
            unknown_segments: Cow::Borrowed(&self.unknown_segments),
            evidence: Cow::Borrowed(&[]),
        })
    }
}

#[test]
fn star_aliases_require_proven_catalog_order() {
    let sql = "WITH c(second, first) AS (SELECT * FROM t) SELECT second, first FROM c";
    let proven = Analyzer::new(Dialect::Generic)
        .with_catalog(&StarCatalog::new(SchemaOrder::Proven))
        .analyze(sql)
        .expect("query parses")
        .into_single()
        .expect("one statement");
    assert!(
        matches!(
            &proven.output.columns[0].lineage,
            Lineage::Complete(sources)
                if sources.len() == 1 && sources[0].column.value == "x"
        ),
        "{:?}",
        proven.output.columns[0].lineage
    );
    assert!(
        matches!(
            &proven.output.columns[1].lineage,
            Lineage::Complete(sources)
                if sources.len() == 1 && sources[0].column.value == "y"
        ),
        "{:?}",
        proven.output.columns[1].lineage
    );

    let unknown = Analyzer::new(Dialect::Generic)
        .with_catalog(&StarCatalog::new(SchemaOrder::Unknown))
        .analyze(sql)
        .expect("query parses")
        .into_single()
        .expect("one statement");
    assert!(
        matches!(
            &unknown.output.columns[0].lineage,
            Lineage::Partial { known, unresolved }
                if known.is_empty()
                    && !unresolved.is_empty()
                    && unresolved.iter().all(|reason| matches!(reason,
                        Unresolved::Unverified { .. }
                            | Unresolved::Unsupported { what: "column alias position unknown" }
                    ))
        ),
        "{:?}",
        unknown.output.columns[0].lineage
    );
}

#[test]
fn correlated_array_and_in_values_keep_the_captured_outer_binding() {
    expect_rendered_in(
        "SELECT ARRAY(SELECT t.x FROM u WHERE u.y = t.y) AS xs FROM t",
        Dialect::BigQuery,
        &["xs#0 C[t.x] Expression", "Closed"],
    );
    expect_rendered_in(
        "SELECT t.x IN (SELECT t.y FROM u WHERE u.z = t.z) AS p FROM t",
        Dialect::BigQuery,
        &["p#0 C[t.x, t.y] Expression", "Closed"],
    );
}

#[test]
fn aliases_keep_proven_values_when_an_unknown_delegate_output_is_replaced() {
    let result = analyze_one_with(
        "WITH c(x, y) AS (WITH nested AS (SELECT t.a, t.* EXCLUDE(x, y) FROM t) SELECT * FROM nested) SELECT x, y FROM c",
        Dialect::Generic,
        None,
    );
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Complete(sources)
            if sources.len() == 1 && sources[0].relation.name.value == "t"
                && sources[0].column.value == "a"
    ));
    assert!(matches!(
        &result.output.columns[1].lineage,
        Lineage::Partial { known, unresolved }
            if known.is_empty() && matches!(unresolved.as_slice(),
                [Unresolved::Unsupported { what: "column alias position unknown" }])
    ));
}

#[test]
fn every_in_target_is_a_value_input() {
    expect_rendered_in(
        r#"SELECT t.a IN (1, t.b) AS p FROM t"#,
        Dialect::Generic,
        &["p#0 C[t.a, t.b] Expression", "Closed"],
    );
    expect_rendered_in(
        r#"SELECT t.a IN UNNEST(t.b) AS p FROM t"#,
        Dialect::BigQuery,
        &["p#0 C[t.a, t.b] Expression", "Closed"],
    );
}

/// SQLite reads `x IN name` and `x IN name(args)` as
/// `x IN (SELECT * FROM name...)`, so both forms read the same relations
/// and give the same value.
#[test]
fn sqlite_in_table_is_in_select_star() {
    for (sql, read) in [
        (r#"SELECT t.a FROM t WHERE t.a IN u"#, &["t", "u"][..]),
        // A CTE name reads the relations of the CTE, not a table of its name.
        (
            r#"WITH c AS (SELECT a FROM v) SELECT t.a FROM t WHERE t.a IN c"#,
            &["t", "v"],
        ),
        // A table-valued function reads the relations of its arguments, not
        // a table of its name.
        (
            r#"SELECT t.a FROM t WHERE t.a IN json_each((SELECT v FROM w))"#,
            &["t", "w"],
        ),
    ] {
        let analysis = analyze_one_with(sql, Dialect::SQLite, None);
        assert_eq!(
            analysis
                .read
                .iter()
                .map(|relation| relation.name.value.as_str())
                .collect::<Vec<_>>(),
            read,
            "{sql}"
        );
    }
    for (in_table, explicit) in [
        (
            r#"SELECT t.a IN u AS p FROM t"#,
            r#"SELECT t.a IN (SELECT * FROM u) AS p FROM t"#,
        ),
        (
            r#"WITH c AS (SELECT a FROM v) SELECT t.a IN c AS p FROM t"#,
            r#"WITH c AS (SELECT a FROM v) SELECT t.a IN (SELECT * FROM c) AS p FROM t"#,
        ),
        (
            r#"SELECT t.a IN json_each(t.b) AS p FROM t"#,
            r#"SELECT t.a IN (SELECT * FROM json_each(t.b)) AS p FROM t"#,
        ),
    ] {
        let actual = analyze_one_with(in_table, Dialect::SQLite, None);
        let expected = analyze_one_with(explicit, Dialect::SQLite, None);
        assert_eq!(actual.output, expected.output, "{in_table}");
        assert_eq!(actual.read, expected.read, "{in_table}");
        assert_eq!(actual.diagnostics, expected.diagnostics, "{in_table}");
    }
}

/// A wildcard argument reads the values of every column it expands to, so
/// it cannot be reported as an argument without inputs. Explicit column
/// arguments keep their exact inputs.
#[test]
fn wildcard_function_arguments_are_not_inputless() {
    let catalog = common::KnownTables(&[("t", &["a", "b"])]);
    expect_rendered_with_catalog(
        "SELECT OBJECT_CONSTRUCT('a', a, 'b', b) AS o FROM t",
        &catalog,
        &["o#0 C[t.a, t.b] Expression", "Closed"],
    );
    for sql in [
        "SELECT OBJECT_CONSTRUCT(*) AS o FROM t",
        "SELECT OBJECT_CONSTRUCT(t.*) AS o FROM t",
        "SELECT HASH(* EXCLUDE (b)) AS o FROM t",
        "SELECT OBJECT_CONSTRUCT('k', a, *) AS o FROM t",
        "SELECT COUNT(t.*) AS o FROM t",
        "SELECT COUNT(* ILIKE 'a%') AS o FROM t",
        "SELECT APPROX_COUNT_DISTINCT(*) AS o FROM t",
    ] {
        let result = analyze_one_with(sql, Dialect::Snowflake, Some(&catalog));
        let Lineage::Partial { unresolved, .. } = &result.output.columns[0].lineage else {
            panic!("{sql}: {:?}", result.output.columns[0].lineage);
        };
        assert!(
            unresolved.iter().any(|reason| matches!(
                reason,
                Unresolved::Unsupported { what } if *what == "wildcard function argument"
            )),
            "{sql}: {unresolved:?}"
        );
    }
}

/// `COUNT(*)` counts rows and reads no column value.
#[test]
fn count_star_reads_no_column_value() {
    let catalog = common::KnownTables(&[("t", &["a", "b"])]);
    expect_rendered_with_catalog(
        "SELECT COUNT(*) AS n, count(*) AS m FROM t",
        &catalog,
        &["n#0 C[] Aggregation", "m#1 C[] Aggregation", "Closed"],
    );
}

fn expect_rendered_with_catalog(sql: &str, catalog: &dyn Catalog, expected: &[&str]) {
    let result = analyze_one_with(sql, Dialect::Snowflake, Some(catalog));
    assert_eq!(common::render_output(&result.output), expected, "{sql}");
}
