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
        let sql = "SELECT x FROM (SELECT * FROM left_t UNION BY NAME SELECT * FROM right_t) s CROSS JOIN other_t o";
        let analysis = analyze_one_with(sql, dialect, None);
        assert!(
            contains_by_name_unsupported(&analysis.output.columns[0].lineage),
            "{dialect:?}: {:#?}",
            analysis.output
        );
    }
}
