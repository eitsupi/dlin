mod common;

use common::{
    KnownTables, analyze_one_with, analyze_with_catalog, concrete_sources, find_mapping, table,
};
use dlin_sqllineage::{Candidate, Dialect, Ident, Lineage, Shape, Unresolved};

#[test]
fn a_field_of_an_unnest_element_keeps_its_parent_array_dependency() {
    let result = analyze_one_with(
        "SELECT item.x AS field FROM base, UNNEST(base.items) AS item",
        Dialect::DuckDb,
        None,
    );

    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "field")),
        vec![("base".into(), "items".into())]
    );
}

#[test]
fn unnest_is_not_a_physical_read_or_an_unexpanded_star_relation() {
    let result = analyze_one_with(
        "SELECT * FROM t, UNNEST(t.arr) AS u(x)",
        Dialect::DuckDb,
        None,
    );

    assert_eq!(result.read, [table("t")]);
    let Shape::Open { unexpanded } = result.output.shape else {
        panic!("expected an open star output");
    };
    assert_eq!(
        unexpanded
            .iter()
            .filter_map(|star| star.relation.as_ref())
            .cloned()
            .collect::<Vec<_>>(),
        [table("t")]
    );
    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("t".into(), "arr".into())]
    );
}

#[test]
fn unaliased_duckdb_unnest_publishes_its_default_output() {
    let result = analyze_one_with("SELECT * FROM UNNEST([1, 2])", Dialect::DuckDb, None);

    assert!(result.read.is_empty());
    assert_eq!(result.output.shape, Shape::Closed);
    assert_eq!(result.output.columns.len(), 1);
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Complete(sources) if sources.is_empty()
    ));
}

#[test]
fn duckdb_unnest_ordinality_uses_the_alias_at_its_position() {
    let result = analyze_one_with(
        "SELECT n FROM UNNEST([1, 2]) WITH ORDINALITY AS u(x, n)",
        Dialect::DuckDb,
        None,
    );

    let mapping = find_mapping(&result.output.columns, "n");
    assert!(matches!(
        &mapping.lineage,
        Lineage::Complete(sources) if sources.is_empty()
    ));
}

#[test]
fn postgres_unnest_with_too_many_aliases_keeps_all_outputs_unresolved() {
    for (sql, dialect, names) in [
        (
            "SELECT x, n FROM UNNEST(ARRAY[1]) WITH ORDINALITY AS u(x, n, extra)",
            Dialect::PostgreSql,
            vec!["x", "n"],
        ),
        (
            "SELECT x FROM UNNEST([1]) AS u(x, extra)",
            Dialect::Generic,
            vec!["x"],
        ),
    ] {
        let result = analyze_one_with(sql, dialect, None);
        for name in names {
            let mapping = find_mapping(&result.output.columns, name);
            assert!(
                matches!(
                    &mapping.lineage,
                    Lineage::Partial { unresolved, .. }
                        if matches!(unresolved.as_slice(), [Unresolved::Unsupported { what }] if *what == "UNNEST alias count mismatch")
                ),
                "{name}: {:?}",
                mapping.lineage
            );
        }
    }
}

#[test]
fn duckdb_unnest_ignores_surplus_aliases() {
    let result = analyze_one_with(
        "SELECT x FROM UNNEST([1]) AS u(x, extra)",
        Dialect::DuckDb,
        None,
    );

    let mapping = find_mapping(&result.output.columns, "x");
    assert!(matches!(
        &mapping.lineage,
        Lineage::Complete(sources) if sources.is_empty()
    ));
}

#[test]
fn a_local_physical_column_shadows_an_outer_virtual_column() {
    let catalog = KnownTables(&[("u", &["item"])]);
    let result = analyze_with_catalog(
        "SELECT (SELECT item FROM u) AS nested FROM t, UNNEST(t.items) AS item",
        Dialect::DuckDb,
        &catalog,
    );

    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "nested")),
        vec![("u".into(), "item".into())]
    );
}

#[test]
fn a_correlated_virtual_column_is_used_when_the_inner_scope_has_no_source() {
    let result = analyze_with_catalog(
        "SELECT (SELECT item) AS nested FROM t, UNNEST(t.items) AS item",
        Dialect::DuckDb,
        &KnownTables(&[("t", &["items"])]),
    );

    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "nested")),
        vec![("t".into(), "items".into())]
    );
}

#[test]
fn same_scope_virtual_columns_with_the_same_name_are_ambiguous() {
    let result = analyze_with_catalog(
        "SELECT x FROM t, UNNEST(t.first) AS u(x), UNNEST(t.second) AS v(x)",
        Dialect::DuckDb,
        &KnownTables(&[("t", &["first", "second"])]),
    );
    let mapping = find_mapping(&result.output.columns, "x");
    assert!(matches!(
        &mapping.lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(unresolved.as_slice(), [Unresolved::Ambiguous { column, candidates }] if column.value == "x" && candidates.is_empty())
    ));
}

#[test]
fn virtual_column_competes_with_an_unknown_physical_provider() {
    let result = analyze_one_with(
        "SELECT x FROM t, UNNEST(t.first) AS u(x)",
        Dialect::DuckDb,
        None,
    );
    let mapping = find_mapping(&result.output.columns, "x");
    assert_eq!(
        mapping.lineage,
        Lineage::Partial {
            known: vec![],
            unresolved: vec![Unresolved::Ambiguous {
                column: Ident::unquoted("x"),
                candidates: vec![Candidate::Relation(table("t"))],
            }],
        }
    );
}

#[test]
fn a_missing_field_on_an_explicit_unnest_column_stays_unresolved() {
    let result = analyze_one_with(
        "SELECT u.missing FROM t, UNNEST(t.arr) AS u(x)",
        Dialect::DuckDb,
        None,
    );
    let mapping = find_mapping(&result.output.columns, "missing");
    assert!(
        matches!(
            &mapping.lineage,
            dlin_sqllineage::Lineage::Partial { unresolved, .. }
                if matches!(unresolved.as_slice(), [dlin_sqllineage::Unresolved::Unbound { column }] if column.value == "missing")
        ),
        "{:?}",
        mapping.lineage
    );
}

#[test]
fn a_field_of_an_unnest_element_keeps_its_parent_with_bigquery_offset() {
    let result = analyze_one_with(
        "SELECT item.x AS field FROM base, UNNEST(base.items) AS item WITH OFFSET AS off",
        Dialect::BigQuery,
        None,
    );

    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "field")),
        vec![("base".into(), "items".into())]
    );
}
