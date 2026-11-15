mod common;

use common::{analyze_one_with, expect_rendered_in};
use dlin_sqllineage::{
    Analyzer, Catalog, CatalogColumn, CatalogEntry, Dialect, Ident, Lineage, Relation,
    SchemaCoverage, SchemaOrder, Unresolved,
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

struct StarCatalog {
    order: SchemaOrder,
    columns: Vec<CatalogColumn>,
}

impl StarCatalog {
    fn new(order: SchemaOrder) -> Self {
        Self {
            order,
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
        }
    }
}

impl Catalog for StarCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name.value == "t").then(|| CatalogEntry {
            relation: relation.clone(),
            columns: Cow::Borrowed(&self.columns),
            coverage: SchemaCoverage::Exhaustive,
            order: self.order,
            unknown_segments: Cow::Borrowed(&[]),
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
                    && matches!(&unresolved[..], [Unresolved::Unsupported { what }] if *what == "column alias order unknown")
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
