//! Output columns keep their own identity through every query wrapper.
//!
//! An output that is duplicated, anonymous, listed after an unexpanded star,
//! renamed, read through a CTE, a derived table or a set operation must carry
//! the sources of exactly the column it is, never those of a column that shares
//! its name, its position or its width. Each case is read directly and through
//! wrappers, and the answer must not change.

mod common;

use common::expect_rendered_in;
use dlin_sqllineage::Dialect;

fn expect(sql: &str, expected: &[&str]) {
    expect_in(sql, Dialect::Generic, expected);
}

fn expect_in(sql: &str, dialect: Dialect, expected: &[&str]) {
    expect_rendered_in(sql, dialect, expected);
}

/// The query `inner` read in turn directly, through a CTE, through a derived
/// table, through a CTE whose body has a CTE of its own, and through two
/// chained CTEs. Every reading must produce `expected`.
fn expect_through_wrappers(inner: &str, expected: &[&str]) {
    expect_through_wrappers_in(inner, Dialect::Generic, expected);
}

fn expect_through_wrappers_in(inner: &str, dialect: Dialect, expected: &[&str]) {
    let readings = [
        inner.to_string(),
        format!("WITH c AS ({inner}) SELECT * FROM c"),
        format!("SELECT * FROM ({inner}) d"),
        format!("WITH c AS (WITH z AS (SELECT 1 AS one FROM v) {inner}) SELECT * FROM c"),
        format!("WITH c AS ({inner}), w AS (SELECT * FROM c) SELECT * FROM w"),
        format!("WITH z AS (SELECT 1 AS one FROM v) {inner}"),
    ];
    for sql in readings {
        expect_in(&sql, dialect, expected);
    }
}

// Duplicate output names

/// Two outputs named `x` keep their own sources and ordinals.
#[test]
fn duplicate_names_keep_their_own_sources() {
    expect_through_wrappers(
        "SELECT t.a AS x, t.b AS x FROM t",
        &["x#0 C[t.a]", "x#1 C[t.b]", "Closed"],
    );
}

/// Reading a duplicated name is ambiguous. It is never the first of the two.
#[test]
fn reading_a_duplicate_name_is_ambiguous() {
    expect(
        "WITH c AS (SELECT t.a AS x, t.b AS x FROM t) SELECT x FROM c",
        &["x#0 P[ | Ambiguous(x: cte c)]", "Closed"],
    );
    expect(
        "SELECT x FROM (SELECT t.a AS x, t.b AS x FROM t) d",
        &["x#0 P[ | Ambiguous(x: derived d)]", "Closed"],
    );
    expect(
        "WITH c AS (SELECT t.a AS x, t.b AS x FROM t), w AS (SELECT * FROM c) \
         SELECT w.x FROM w",
        &["x#0 P[ | Ambiguous(x: cte w)]", "Closed"],
    );
}

/// A duplicate name read next to unique ones does not disturb the others.
#[test]
fn duplicate_names_do_not_disturb_their_neighbours() {
    expect_through_wrappers(
        "SELECT t.a AS p, t.b AS x, t.c AS x, t.d AS q FROM t",
        &[
            "p#0 C[t.a]",
            "x#1 C[t.b]",
            "x#2 C[t.c]",
            "q#3 C[t.d]",
            "Closed",
        ],
    );
    expect(
        "WITH c AS (SELECT t.a AS p, t.b AS x, t.c AS x, t.d AS q FROM t) \
         SELECT q, p FROM c",
        &["q#0 C[t.d]", "p#1 C[t.a]", "Closed"],
    );
}

// Anonymous outputs

/// Two anonymous outputs are two columns, not one.
#[test]
fn anonymous_outputs_keep_their_own_sources() {
    expect_through_wrappers(
        "SELECT t.a + 1, t.b + 1 FROM t",
        &["_#0 C[t.a] Expression", "_#1 C[t.b] Expression", "Closed"],
    );
}

/// Anonymous outputs next to named ones stay at their positions.
#[test]
fn anonymous_outputs_between_named_ones_keep_position() {
    expect_through_wrappers(
        "SELECT t.a AS p, t.b + 1, t.c AS q, t.d + 1 FROM t",
        &[
            "p#0 C[t.a]",
            "_#1 C[t.b] Expression",
            "q#2 C[t.c]",
            "_#3 C[t.d] Expression",
            "Closed",
        ],
    );
}

// Outputs after an unexpanded star

/// A column listed after an unexpanded star has no known position, but it
/// keeps its own sources through every wrapper.
#[test]
fn columns_after_an_unknown_segment_keep_their_sources() {
    expect_through_wrappers(
        "SELECT *, t.x AS y, t.z AS w FROM t",
        &["y#- C[t.x]", "w#- C[t.z]", "Open[t after -]"],
    );
}

/// The same names read one by one. A name that the unexpanded star may also
/// publish is ambiguous between the listed column and the star, so it does not
/// resolve to the listed column.
#[test]
fn reading_names_after_an_unknown_segment_competes_with_the_star() {
    for from in ["c", "(SELECT * FROM c) d"] {
        expect(
            &format!("WITH c AS (SELECT *, t.x AS y, t.z AS w FROM t) SELECT w, y FROM {from}"),
            &[
                "w#0 P[ | Ambiguous(w: t)]",
                "y#1 P[ | Ambiguous(y: t)]",
                "Closed",
            ],
        );
    }
}

/// Columns listed before and after a star keep their own positions where the
/// position is known.
#[test]
fn positions_before_an_unknown_segment_survive_wrappers() {
    expect_through_wrappers(
        "SELECT t.x AS y, t.z AS w, * FROM t",
        &["y#0 C[t.x]", "w#1 C[t.z]", "Open[t after 1]"],
    );
    expect_through_wrappers(
        "SELECT t.x AS y, *, t.z AS w FROM t",
        &["y#0 C[t.x]", "w#- C[t.z]", "Open[t after 0]"],
    );
    expect_through_wrappers(
        "SELECT t.k + 1, t.m + 1, * FROM t",
        &[
            "_#0 C[t.k] Expression",
            "_#1 C[t.m] Expression",
            "Open[t after 1]",
        ],
    );
}

// Projection aliases over a CTE

/// An alias in an outer projection takes the value of the column it names in
/// the CTE, not the column at the aliased position.
#[test]
fn projection_aliases_over_a_cte_follow_the_named_column() {
    expect(
        "WITH c AS (SELECT t.a AS x, t.b AS y FROM t) SELECT y AS a, x AS b FROM c",
        &["a#0 C[t.b]", "b#1 C[t.a]", "Closed"],
    );
    expect(
        "SELECT y AS a, x AS b FROM (SELECT t.a AS x, t.b AS y FROM t) d",
        &["a#0 C[t.b]", "b#1 C[t.a]", "Closed"],
    );
    expect(
        "WITH c AS (SELECT t.a AS x, t.b AS y FROM t), w AS (SELECT y AS x, x AS y FROM c) \
         SELECT y, x FROM w",
        &["y#0 C[t.a]", "x#1 C[t.b]", "Closed"],
    );
}

// Chains of stars

/// A chain of `SELECT *` over CTEs keeps every column's own sources.
#[test]
fn nested_star_chains_forward_every_column() {
    let with = "WITH a AS (SELECT t.p AS x, t.q AS y FROM t), \
                b AS (SELECT * FROM a), c AS (SELECT * FROM b) ";
    expect(
        &format!("{with}SELECT * FROM c"),
        &["x#0 C[t.p]", "y#1 C[t.q]", "Closed"],
    );
    expect(
        &format!("{with}SELECT y, x FROM c"),
        &["y#0 C[t.q]", "x#1 C[t.p]", "Closed"],
    );
    expect(
        "SELECT * FROM (SELECT * FROM (SELECT * FROM (SELECT t.p AS x, t.q AS y FROM t) a) b) c",
        &["x#0 C[t.p]", "y#1 C[t.q]", "Closed"],
    );
}

// RENAME

/// A renamed column keeps the value it had, even when the new name equals the
/// name of another column.
#[test]
fn rename_changes_the_name_and_keeps_the_value() {
    let with = "WITH c AS (SELECT t.a AS a, t.b AS b FROM t) ";
    for (rename, expected) in [
        ("a AS b2", ["b2#0 C[t.a]", "b#1 C[t.b]"]),
        ("a AS b", ["b#0 C[t.a]", "b#1 C[t.b]"]),
        ("b AS a", ["a#0 C[t.a]", "a#1 C[t.b]"]),
    ] {
        let expected: Vec<&str> = expected.into_iter().chain(["Closed"]).collect();
        for sql in [
            format!("{with}SELECT * RENAME ({rename}) FROM c"),
            format!("{with}SELECT * FROM (SELECT * RENAME ({rename}) FROM c) d"),
            format!("{with}, r AS (SELECT * RENAME ({rename}) FROM c) SELECT * FROM r")
                .replacen("), , ", "), ", 1),
        ] {
            expect_in(&sql, Dialect::Snowflake, &expected);
        }
    }
}

/// A name that a rename made duplicate is ambiguous, and a name that a rename
/// took away is gone.
#[test]
fn reading_renamed_names() {
    let with = "WITH c AS (SELECT t.a AS a, t.b AS b FROM t) ";
    expect_in(
        &format!("{with}SELECT b FROM (SELECT * RENAME (a AS b) FROM c) d"),
        Dialect::Snowflake,
        &["b#0 P[ | Ambiguous(b: cte c)]", "Closed"],
    );
    expect_in(
        &format!("{with}SELECT b FROM (SELECT * RENAME (b AS a) FROM c) d"),
        Dialect::Snowflake,
        &["b#0 P[ | Unbound(b)]", "Closed"],
    );
}

// One CTE read twice

/// Two ranges over one CTE are two sets of columns with the same sources.
#[test]
fn one_cte_read_through_two_ranges() {
    let with = "WITH c AS (SELECT t.a AS a, t.b AS b FROM t) ";
    expect(
        &format!("{with}SELECT c1.a, c2.b FROM c c1 JOIN c c2 ON TRUE"),
        &["a#0 C[t.a]", "b#1 C[t.b]", "Closed"],
    );
    expect(
        &format!("{with}SELECT c1.b AS p, c2.a AS q FROM c c1, c c2"),
        &["p#0 C[t.b]", "q#1 C[t.a]", "Closed"],
    );
    expect(
        &format!("{with}SELECT * FROM c c1, c c2"),
        &[
            "a#0 C[t.a]",
            "b#1 C[t.b]",
            "a#2 C[t.a]",
            "b#3 C[t.b]",
            "Closed",
        ],
    );
}

/// A star over two ranges of a CTE that has duplicate names keeps all four.
#[test]
fn one_cte_with_duplicate_names_read_through_two_ranges() {
    expect(
        "WITH c AS (SELECT t.a AS x, t.b AS x FROM t) SELECT * FROM c c1, c c2",
        &[
            "x#0 C[t.a]",
            "x#1 C[t.b]",
            "x#2 C[t.a]",
            "x#3 C[t.b]",
            "Closed",
        ],
    );
}

// Set operations over wrapped outputs

/// Duplicate names on one side of a set operation pair with the other side by
/// position, whichever side they are on and however they are wrapped.
#[test]
fn set_operation_pairs_duplicate_names_by_position() {
    let duplicated = "SELECT t.a AS x, t.b AS x FROM t";
    let plain = "SELECT u.p, u.q FROM u";
    expect_through_wrappers(
        &format!("{duplicated} UNION ALL {plain}"),
        &["x#0 C[t.a, u.p]", "x#1 C[t.b, u.q]", "Closed"],
    );
    for (left, right) in [
        (format!("WITH c AS ({duplicated}) SELECT * FROM c"), plain),
        (format!("SELECT * FROM ({duplicated}) d"), plain),
        (
            format!("WITH c AS ({duplicated}) SELECT * FROM (SELECT * FROM c) d"),
            plain,
        ),
    ] {
        expect(
            &format!("{left} UNION ALL {right}"),
            &["x#0 C[t.a, u.p]", "x#1 C[t.b, u.q]", "Closed"],
        );
    }
    expect(
        &format!("{plain} UNION ALL SELECT * FROM ({duplicated}) d"),
        &["p#0 C[t.a, u.p]", "q#1 C[t.b, u.q]", "Closed"],
    );
}

/// Anonymous outputs on one side of a set operation pair by position.
#[test]
fn set_operation_pairs_anonymous_outputs_by_position() {
    let anonymous = "SELECT t.a + 1, t.b + 1 FROM t";
    for left in [
        format!("WITH c AS ({anonymous}) SELECT * FROM c"),
        format!("SELECT * FROM ({anonymous}) d"),
    ] {
        expect(
            &format!("{left} UNION ALL SELECT u.p, u.q FROM u"),
            &[
                "_#0 C[t.a, u.p] Expression",
                "_#1 C[t.b, u.q] Expression",
                "Closed",
            ],
        );
        expect(
            &format!("{left} UNION ALL SELECT u.p, u.q FROM u UNION ALL SELECT v.r, v.s FROM v"),
            &[
                "_#0 C[t.a, u.p, v.r] Expression",
                "_#1 C[t.b, u.q, v.s] Expression",
                "Closed",
            ],
        );
    }
}

/// An unknown segment on one side of a set operation leaves the positions
/// after it unproven, through every wrapper.
#[test]
fn set_operation_with_an_unknown_segment_stays_unproven_through_wrappers() {
    let segment = "SELECT *, t.x AS y FROM t";
    for left in [
        format!("WITH c AS ({segment}) SELECT * FROM c"),
        format!("SELECT * FROM ({segment}) d"),
        format!("WITH c AS ({segment}) SELECT * FROM (SELECT * FROM c) d"),
    ] {
        expect(
            &format!("{left} UNION ALL SELECT u.p FROM u"),
            &["y#- P[t.x | AlignmentUnknown]", "Open[t after -]"],
        );
    }
    // A right-side unknown segment cannot add names or widen the shape beyond
    // the left output namespace. The left slot remains, with unresolved
    // alignment to the right input.
    expect(
        "SELECT u.p FROM u UNION ALL SELECT * FROM (SELECT *, t.x AS y FROM t) d",
        &["p#0 P[u.p | AlignmentUnknown]", "Closed"],
    );
}

/// A set operation read through a wrapper keeps its columns apart and in order.
#[test]
fn set_operation_columns_survive_wrappers() {
    let union = "SELECT u.p, u.q FROM u UNION ALL SELECT v.r, v.s FROM v";
    expect_through_wrappers(union, &["p#0 C[u.p, v.r]", "q#1 C[u.q, v.s]", "Closed"]);
    expect(
        &format!("WITH c AS ({union}) SELECT q, p FROM c"),
        &["q#0 C[u.q, v.s]", "p#1 C[u.p, v.r]", "Closed"],
    );
    expect(
        &format!("SELECT q, p FROM ({union}) d"),
        &["q#0 C[u.q, v.s]", "p#1 C[u.p, v.r]", "Closed"],
    );
}

/// Duplicate names inside a set operation pair by position when the operation
/// is itself read through a wrapper.
#[test]
fn duplicate_names_inside_a_set_operation_read_through_a_wrapper() {
    expect(
        "WITH s AS (SELECT t.a AS x, t.b AS x FROM t UNION ALL SELECT u.p, u.q FROM u) \
         SELECT * FROM s",
        &["x#0 C[t.a, u.p]", "x#1 C[t.b, u.q]", "Closed"],
    );
}

/// Nested set operations, on either side and through wrappers, pair every
/// operand by position.
#[test]
fn nested_set_operations_pair_every_operand_by_position() {
    let expected = [
        "a#0 C[t1.a, t2.c, t3.e]",
        "b#1 C[t1.b, t2.d, t3.f]",
        "Closed",
    ];
    let first = "SELECT t1.a, t1.b FROM t1";
    let second = "SELECT t2.c, t2.d FROM t2";
    let third = "SELECT t3.e, t3.f FROM t3";
    expect(
        &format!("({first} UNION ALL {second}) UNION ALL {third}"),
        &expected,
    );
    expect(
        &format!("{first} UNION ALL ({second} UNION ALL {third})"),
        &expected,
    );
    expect(
        &format!("SELECT * FROM ({first} UNION ALL {second}) d UNION ALL {third}"),
        &expected,
    );
    expect(
        &format!("WITH s AS ({first} UNION ALL {second}) SELECT * FROM s UNION ALL {third}"),
        &expected,
    );
    expect(
        &format!(
            "SELECT * FROM ({first} UNION ALL {second}) d UNION ALL {third} \
             UNION ALL SELECT t4.g, t4.h FROM t4"
        ),
        &[
            "a#0 C[t1.a, t2.c, t3.e, t4.g]",
            "b#1 C[t1.b, t2.d, t3.f, t4.h]",
            "Closed",
        ],
    );
}

/// A column listed before the unknown segments of both operands pairs by
/// position, and the shape stays open.
#[test]
fn set_operation_prefix_before_unknown_segments_pairs_through_wrappers() {
    let union = "SELECT t.id AS k, t.* FROM t UNION ALL SELECT u.id AS k, u.* FROM u";
    // Each operand's unknown interval appears once in the output shape, and
    // wrapping the operation does not duplicate either marker.
    expect_through_wrappers(union, &["k#0 C[t.id, u.id]", "Open[t after 0; u after 0]"]);
}
