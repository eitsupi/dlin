mod common;

use common::{analyze_one_with, analyze_rendered};
use dlin_sqllineage::{Dialect, Lineage, Relation, Shape, Unresolved};

const BY_NAME: &str = "BY NAME";
const ALL_BY_NAME: &str = "ALL BY NAME";
const DISTINCT_BY_NAME: &str = "DISTINCT BY NAME";

fn contains_by_name_unsupported(lineage: &Lineage) -> bool {
    matches!(
        lineage,
        Lineage::Partial { unresolved, .. }
            if unresolved.iter().any(|reason| matches!(
                reason,
                Unresolved::Unsupported { what: "BY NAME set operation" }
            ))
    )
}

fn assert_by_name_is_unsupported(sql: &str, dialect: Dialect) {
    let rendered = analyze_rendered(sql, dialect, None);
    assert!(
        rendered
            .last()
            .is_some_and(|line| line.starts_with("Open[")),
        "{sql}: {rendered:#?}"
    );

    let analysis = analyze_one_with(sql, dialect, None);
    let has_unsupported = analysis
        .output
        .columns
        .iter()
        .any(|column| contains_by_name_unsupported(&column.lineage))
        || matches!(
            &analysis.output.shape,
            Shape::Open { unexpanded }
                if unexpanded.iter().any(|star| contains_by_name_unsupported(&star.lineage))
        );
    assert!(has_unsupported, "{sql}: {:#?}", analysis.output);
    let read: Vec<_> = analysis
        .read
        .iter()
        .map(|relation| relation.name.value.as_str())
        .collect();
    assert!(read.contains(&"left_t"), "{sql}: {read:?}");
    assert!(read.contains(&"right_t"), "{sql}: {read:?}");
}

#[test]
fn by_name_quantifiers_keep_the_left_value_and_report_unsupported_shape() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        for quantifier in [BY_NAME, ALL_BY_NAME, DISTINCT_BY_NAME] {
            let sql = format!("SELECT a FROM left_t UNION {quantifier} SELECT b, c FROM right_t");
            assert_by_name_is_unsupported(&sql, dialect);
            let rendered = analyze_rendered(&sql, dialect, None);
            assert!(
                rendered[0].starts_with("a#0 P[left_t.a | "),
                "{dialect:?} {quantifier}: {rendered:#?}"
            );
        }
    }
}

#[test]
fn by_name_stars_on_both_sides_remain_open_and_keep_both_reads() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        let sql = "SELECT * FROM left_t UNION ALL BY NAME SELECT * FROM right_t";
        assert_by_name_is_unsupported(sql, dialect);
    }
}

#[test]
fn outer_lookup_of_a_right_only_name_is_unsupported_through_a_derived_table() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        let sql = "SELECT s.b FROM (SELECT a FROM left_t UNION BY NAME SELECT b FROM right_t) AS s";
        let rendered = analyze_rendered(sql, dialect, None);
        assert!(
            rendered[0].contains("Unsupported"),
            "{dialect:?}: {rendered:#?}"
        );
        let analysis = analyze_one_with(sql, dialect, None);
        let reads: Vec<_> = analysis
            .read
            .iter()
            .map(|Relation { name, .. }| name.value.as_str())
            .collect();
        assert!(reads.contains(&"left_t"), "{reads:?}");
        assert!(reads.contains(&"right_t"), "{reads:?}");
    }
}

#[test]
fn by_name_shape_and_reason_survive_a_cte_delegate() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        let sql = "WITH c AS (SELECT a FROM left_t UNION BY NAME SELECT b, c FROM right_t) SELECT * FROM c";
        assert_by_name_is_unsupported(sql, dialect);
    }
}

#[test]
fn by_name_reason_survives_when_nested_on_either_side_of_a_positional_union() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        for sql in [
            "SELECT a FROM (SELECT x AS a FROM x_t UNION BY NAME SELECT y AS b FROM y_t) AS s UNION ALL SELECT z AS a FROM z_t",
            "SELECT z AS a FROM z_t UNION ALL SELECT a FROM (SELECT x AS a FROM x_t UNION BY NAME SELECT y AS b FROM y_t) AS s",
        ] {
            let rendered = analyze_rendered(sql, dialect, None);
            assert!(
                rendered.iter().any(|line| line.contains("Unsupported")),
                "{dialect:?}: {rendered:#?}"
            );
        }
    }
}

#[test]
fn a_left_name_selected_from_outside_keeps_only_the_left_value() {
    use common::known_sources;

    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        let sql = "SELECT s.a FROM (SELECT a FROM left_t UNION BY NAME SELECT b FROM right_t) AS s";
        let rendered = analyze_rendered(sql, dialect, None);
        assert!(
            rendered[0].starts_with("a#0 P[left_t.a | "),
            "{dialect:?}: {rendered:#?}"
        );
        let analysis = analyze_one_with(sql, dialect, None);
        assert!(contains_by_name_unsupported(
            &analysis.output.columns[0].lineage
        ));
        let sources: Vec<_> = known_sources(&analysis.output.columns[0])
            .iter()
            .map(|source| {
                (
                    source.relation.name.value.as_str(),
                    source.column.value.as_str(),
                )
            })
            .collect();
        assert_eq!(sources, [("left_t", "a")], "{dialect:?}: {sources:?}");
    }
}

#[test]
fn by_name_with_only_the_right_or_only_the_left_star_stays_open() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        assert_by_name_is_unsupported(
            "SELECT a FROM left_t UNION BY NAME SELECT * FROM right_t",
            dialect,
        );
        assert_by_name_is_unsupported(
            "SELECT * FROM left_t UNION BY NAME SELECT b FROM right_t",
            dialect,
        );
    }
}

fn assert_positional_wrapper_keeps_only_by_name_left_sources(sql: &str, dialect: Dialect) {
    use common::known_sources;

    let rendered = analyze_rendered(sql, dialect, None);
    assert!(
        rendered.iter().any(|line| line.contains("Unsupported")),
        "{dialect:?}: {rendered:#?}"
    );
    let analysis = analyze_one_with(sql, dialect, None);
    let output = analysis
        .output
        .columns
        .first()
        .unwrap_or_else(|| panic!("{dialect:?}: {:#?}", analysis.output));
    assert!(contains_by_name_unsupported(&output.lineage), "{output:?}");
    let sources: Vec<_> = known_sources(output)
        .iter()
        .map(|source| {
            (
                source.relation.name.value.as_str(),
                source.column.value.as_str(),
            )
        })
        .collect();
    assert!(sources.contains(&("x_t", "x")), "{dialect:?}: {sources:?}");
    assert!(!sources.contains(&("y_t", "y")), "{dialect:?}: {sources:?}");
}

#[test]
fn by_name_as_the_left_operand_of_a_positional_union_keeps_its_reason() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        assert_positional_wrapper_keeps_only_by_name_left_sources(
            "SELECT x AS a FROM x_t UNION BY NAME SELECT y AS b FROM y_t UNION ALL SELECT z AS a FROM z_t",
            dialect,
        );
    }
}

#[test]
fn by_name_as_the_right_operand_of_a_positional_union_keeps_its_reason() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        assert_positional_wrapper_keeps_only_by_name_left_sources(
            "SELECT z AS a FROM z_t UNION ALL (SELECT x AS a FROM x_t UNION BY NAME SELECT y AS b FROM y_t)",
            dialect,
        );
    }
}

#[test]
fn by_name_does_not_hide_a_known_width_mismatch_in_an_inner_positional_union() {
    use dlin_sqllineage::DiagnosticKind;

    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        let sql =
            "SELECT a FROM left_t UNION BY NAME (SELECT x FROM x_t UNION ALL SELECT y, z FROM y_t)";
        let analysis = analyze_one_with(sql, dialect, None);
        assert!(
            analysis.diagnostics.iter().any(|diagnostic| matches!(
                diagnostic.kind,
                DiagnosticKind::SetOperationArityMismatch { left: 1, right: 2 }
            )),
            "{dialect:?}: {:#?}",
            analysis.diagnostics
        );
    }
}

#[test]
fn ambiguity_keeps_the_correspondence_reason_through_projection_wrappers() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        for sql in [
            "SELECT s.a FROM (SELECT * FROM (SELECT a, a FROM left_t UNION BY NAME SELECT b, b FROM right_t) u) s",
            "SELECT a FROM (SELECT a FROM left_t UNION BY NAME SELECT b FROM right_t) s JOIN another_t q ON TRUE",
        ] {
            let analysis = analyze_one_with(sql, dialect, None);
            assert!(
                common::known_sources(&analysis.output.columns[0]).is_empty(),
                "{dialect:?} {sql}: {:#?}",
                analysis.output
            );
            assert!(
                contains_by_name_unsupported(&analysis.output.columns[0].lineage),
                "{dialect:?} {sql}: {:#?}",
                analysis.output
            );
        }
    }
}

#[test]
fn an_unmodelled_left_source_does_not_make_by_name_exhaustive() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        let analysis = analyze_one_with(
            "SELECT * FROM (VALUES (1)) v UNION BY NAME SELECT b FROM right_t",
            dialect,
            None,
        );
        let Shape::Open { unexpanded } = &analysis.output.shape else {
            panic!("{dialect:?}: {:#?}", analysis.output);
        };
        assert!(
            unexpanded
                .iter()
                .any(|star| contains_by_name_unsupported(&star.lineage))
        );
    }
}

#[test]
fn unknown_only_ambiguity_keeps_the_correspondence_reason() {
    for dialect in [Dialect::Generic, Dialect::DuckDb] {
        for sql in [
            "SELECT x FROM (SELECT * FROM left_t UNION BY NAME SELECT * FROM right_t) s CROSS JOIN other_t o",
            "SELECT x FROM (SELECT a FROM left_t UNION BY NAME SELECT b FROM right_t) s CROSS JOIN other_t o",
        ] {
            let analysis = analyze_one_with(sql, dialect, None);
            assert!(
                common::known_sources(&analysis.output.columns[0]).is_empty(),
                "{dialect:?} {sql}: {:#?}",
                analysis.output
            );
            assert!(
                contains_by_name_unsupported(&analysis.output.columns[0].lineage),
                "{dialect:?} {sql}: {:#?}",
                analysis.output
            );
        }
    }
}

/// `x(a, b)` and `y(b, a)`, for the cases that read operands through stars.
const XY: common::KnownTables = common::KnownTables(&[("x", &["a", "b"]), ("y", &["b", "a"])]);

fn expect(dialect: Dialect, sql: &str, expected: &[&str]) {
    assert_eq!(
        analyze_rendered(sql, dialect, None),
        expected,
        "{dialect:?} {sql}"
    );
}

/// The strict forms correspond outputs by name: the output follows the left
/// operand, and each output takes the right column of the same name (S4).
#[test]
fn strict_by_name_pairs_columns_by_name() {
    for (dialect, operation) in [
        (Dialect::BigQuery, "UNION ALL BY NAME"),
        (Dialect::BigQuery, "UNION DISTINCT BY NAME"),
        (Dialect::BigQuery, "INTERSECT DISTINCT BY NAME"),
        (Dialect::DuckDb, "UNION BY NAME"),
        (Dialect::DuckDb, "UNION ALL BY NAME"),
        (Dialect::Snowflake, "UNION ALL BY NAME"),
    ] {
        for right in ["SELECT b, a FROM y", "SELECT a, b FROM y"] {
            // Reordering the right operand does not change what corresponds.
            expect(
                dialect,
                &format!("SELECT a, b FROM x {operation} {right}"),
                &["a#0 C[x.a, y.a]", "b#1 C[x.b, y.b]", "Closed"],
            );
        }
    }
    // Positional correspondence does change.
    expect(
        Dialect::BigQuery,
        "SELECT a, b FROM x UNION ALL SELECT b, a FROM y",
        &["a#0 C[x.a, y.b]", "b#1 C[x.b, y.a]", "Closed"],
    );
    // The right operand of EXCEPT supplies no values.
    expect(
        Dialect::BigQuery,
        "SELECT a, b FROM x EXCEPT DISTINCT BY NAME SELECT b, a FROM y",
        &["a#0 C[x.a]", "b#1 C[x.b]", "Closed"],
    );
}

#[test]
fn strict_by_name_matches_names_by_the_dialect_rule() {
    // BigQuery compares column names case-insensitively; the left spelling is
    // the output's.
    expect(
        Dialect::BigQuery,
        "SELECT A, b FROM x UNION ALL BY NAME SELECT b, a FROM y",
        &["A#0 C[x.A, y.a]", "b#1 C[x.b, y.b]", "Closed"],
    );
    // In Snowflake a quoted lower case name differs from an unquoted one, so
    // the operands do not list the same names.
    assert_by_name_is_unsupported(
        "SELECT a, b FROM left_t UNION ALL BY NAME SELECT \"b\", a FROM right_t",
        Dialect::Snowflake,
    );
}

#[test]
fn strict_by_name_reads_stars_the_catalog_expands() {
    assert_eq!(
        analyze_rendered(
            "SELECT * FROM x UNION ALL BY NAME SELECT * FROM y",
            Dialect::BigQuery,
            Some(&XY)
        ),
        ["a#0 C[x.a, y.a]", "b#1 C[x.b, y.b]", "Closed"]
    );
}

#[test]
fn strict_by_name_survives_wrappers() {
    expect(
        Dialect::BigQuery,
        "WITH c AS (SELECT a, b FROM x UNION ALL BY NAME SELECT b, a FROM y) SELECT b FROM c",
        &["b#0 C[x.b, y.b]", "Closed"],
    );
    expect(
        Dialect::BigQuery,
        "SELECT * FROM (SELECT a, b FROM x UNION ALL BY NAME SELECT b, a FROM y) \
         UNION ALL SELECT b, a FROM x",
        &["a#0 C[x.a, x.b, y.a]", "b#1 C[x.a, x.b, y.b]", "Closed"],
    );
}

/// Only the forms each dialect documents with the strict meaning are
/// modelled, and only when both operands list the same names once. Anything
/// else keeps the left values and reports the correspondence as unsupported.
#[test]
fn by_name_forms_without_a_proven_correspondence_stay_unsupported() {
    for (dialect, sql) in [
        // The quantifier is required in BigQuery.
        (
            Dialect::BigQuery,
            "SELECT a, b FROM left_t UNION BY NAME SELECT b, a FROM right_t",
        ),
        (
            Dialect::BigQuery,
            "SELECT a, b FROM left_t INTERSECT ALL BY NAME SELECT b, a FROM right_t",
        ),
        // DuckDB and Snowflake accept BY NAME only with UNION.
        (
            Dialect::DuckDb,
            "SELECT a, b FROM left_t EXCEPT BY NAME SELECT b, a FROM right_t",
        ),
        (
            Dialect::Snowflake,
            "SELECT a, b FROM left_t INTERSECT BY NAME SELECT b, a FROM right_t",
        ),
        (
            Dialect::Snowflake,
            "SELECT a, b FROM left_t MINUS BY NAME SELECT b, a FROM right_t",
        ),
        // Dialects that do not document BY NAME.
        (
            Dialect::Generic,
            "SELECT a, b FROM left_t UNION ALL BY NAME SELECT b, a FROM right_t",
        ),
        (
            Dialect::PostgreSql,
            "SELECT a, b FROM left_t UNION ALL BY NAME SELECT b, a FROM right_t",
        ),
        // Different names: an error in BigQuery, NULL padding in DuckDB.
        (
            Dialect::BigQuery,
            "SELECT a FROM left_t UNION ALL BY NAME SELECT b FROM right_t",
        ),
        (
            Dialect::DuckDb,
            "SELECT a FROM left_t UNION ALL BY NAME SELECT a, b FROM right_t",
        ),
        // A name listed twice.
        (
            Dialect::BigQuery,
            "SELECT a, a FROM left_t UNION ALL BY NAME SELECT a, a FROM right_t",
        ),
        // A star the catalog cannot expand.
        (
            Dialect::BigQuery,
            "SELECT a FROM left_t UNION ALL BY NAME SELECT * FROM right_t",
        ),
    ] {
        assert_by_name_is_unsupported(sql, dialect);
    }
}
