//! CTEs that read each other through a star or a set operation.
//!
//! The cycle cannot be evaluated, and the answer says so: what it hides is an
//! unresolved part of the output, never a missing column or a closed shape.

mod common;

use common::analyze_one_with;
use dlin_sqllineage::{Dialect, Lineage, OutputName, Shape, Unresolved};

const DIALECTS: [Dialect; 2] = [Dialect::Generic, Dialect::PostgreSql];

/// The kinds of unresolved parts, in order.
fn kinds(unresolved: &[Unresolved]) -> Vec<String> {
    unresolved
        .iter()
        .map(|part| match part {
            Unresolved::Unsupported { what } => format!("Unsupported({what})"),
            Unresolved::BehindStar { relation } => format!("BehindStar({})", relation.name.value),
            Unresolved::NamedBehindStar { relation, column } => {
                format!("NamedBehindStar({}.{})", relation.name.value, column.value)
            }
            Unresolved::Unbound { column } => format!("Unbound({})", column.value),
            other => format!("{other:?}"),
        })
        .collect()
}

/// For each unexpanded star: the relation it reads and what is unresolved.
fn open_stars(sql: &str, dialect: Dialect) -> Vec<(Option<String>, Vec<String>)> {
    let analysis = analyze_one_with(sql, dialect, None);
    assert!(analysis.output.columns.is_empty(), "{sql}");
    assert!(analysis.has_unresolved_stars, "{sql}");
    let Shape::Open { unexpanded } = &analysis.output.shape else {
        panic!("{dialect:?} {sql}: the shape is closed");
    };
    unexpanded
        .iter()
        .map(|star| {
            let Lineage::Partial { known, unresolved } = &star.lineage else {
                panic!("{dialect:?} {sql}: a star over a cycle is not complete");
            };
            assert!(known.is_empty(), "{dialect:?} {sql}");
            (
                star.relation
                    .as_ref()
                    .map(|relation| relation.name.value.clone()),
                kinds(unresolved),
            )
        })
        .collect()
}

/// A star over a CTE that reads itself through another CTE has no columns the
/// analysis can list; the cut is reported as an unexpanded star, not dropped
/// to an empty, closed output.
#[test]
fn a_star_over_a_cycle_through_a_union_is_an_unexpanded_star() {
    for dialect in DIALECTS {
        assert_eq!(
            open_stars(
                "WITH RECURSIVE a AS (SELECT * FROM b UNION ALL SELECT x FROM t), \
                 b AS (SELECT * FROM a) SELECT * FROM a",
                dialect
            ),
            [(None, vec!["Unsupported(star expansion)".to_owned()])],
            "{dialect:?}"
        );
    }
}

/// The right operand of a union whose left operand is cut stays a star over `t`;
/// the cut is reported as further unexpanded stars instead of being dropped.
#[test]
fn a_star_over_the_right_operand_of_a_cut_union_stays_open() {
    for dialect in DIALECTS {
        let behind = "BehindStar(t)".to_owned();
        let unsupported = "Unsupported(star expansion)".to_owned();
        assert_eq!(
            open_stars(
                "WITH RECURSIVE a AS (SELECT * FROM t UNION ALL SELECT * FROM b), \
                 b AS (SELECT * FROM a) SELECT * FROM b",
                dialect
            ),
            [
                (
                    Some("t".to_owned()),
                    vec![behind.clone(), unsupported.clone()]
                ),
                (
                    Some("t".to_owned()),
                    vec![unsupported.clone(), behind.clone()]
                ),
                (Some("t".to_owned()), vec![unsupported, behind]),
            ],
            "{dialect:?}"
        );
    }
}

/// A name read through the cut union is unbound next to what the star may
/// hide; the cut is reported as `Unbound(y)` instead of being dropped.
#[test]
fn a_name_read_through_a_cut_union_is_unbound_and_behind_the_star() {
    for dialect in DIALECTS {
        let analysis = analyze_one_with(
            "WITH RECURSIVE a AS (SELECT * FROM t UNION ALL SELECT * FROM b), \
             b AS (SELECT * FROM a) SELECT y FROM b",
            dialect,
            None,
        );
        assert!(
            matches!(analysis.output.shape, Shape::Closed),
            "{dialect:?}"
        );
        let [column] = &analysis.output.columns[..] else {
            panic!("{dialect:?}: expected one column");
        };
        assert!(
            matches!(&column.name, OutputName::Explicit(name) | OutputName::Inferred(name)
                if name.value == "y"),
            "{dialect:?}"
        );
        assert_eq!(column.ordinal, Some(0));
        let Lineage::Partial { known, unresolved } = &column.lineage else {
            panic!("{dialect:?}: a cut is not complete");
        };
        assert!(known.is_empty());
        assert_eq!(
            kinds(unresolved),
            ["NamedBehindStar(t.y)", "Unbound(y)"],
            "{dialect:?}"
        );
    }
}
