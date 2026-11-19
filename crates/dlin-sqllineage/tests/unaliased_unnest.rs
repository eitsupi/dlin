//! BigQuery `UNNEST` without a range variable. The element of an array of
//! STRUCTs publishes its fields as columns of the FROM clause, so a name may
//! be a field of the element or a column of an enclosing query. Without the
//! element type, the reference stays open between the candidates instead of
//! being attributed to the enclosing relation.

mod common;

use common::{KnownTables, analyze_one_with, find_mapping, known_sources, unresolved};
use dlin_sqllineage::{Candidate, Dialect, Ident, Unresolved};

/// The column names the array expression of each element candidate reads,
/// and the relation names of the other candidates.
fn candidate_names(candidates: &[Candidate]) -> Vec<String> {
    candidates
        .iter()
        .map(|candidate| match candidate {
            Candidate::Relation(relation) => relation.name.value.clone(),
            Candidate::ArrayElement(element) => format!(
                "element of [{}]",
                element
                    .array_sources
                    .iter()
                    .map(|source| format!("{}.{}", source.relation.name.value, source.column.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            other => format!("{other:?}"),
        })
        .collect()
}

/// The single `Unverified` reason of an output column, as
/// `(present, possible)` candidate names.
fn unverified(
    analysis: &dlin_sqllineage::StatementAnalysis,
    output: &str,
    name: &str,
) -> (Vec<String>, Vec<String>) {
    let column = find_mapping(&analysis.output.columns, output);
    assert!(known_sources(column).is_empty(), "{column:?}");
    match unresolved(column) {
        [
            Unresolved::Unverified {
                column,
                present_candidates,
                possible_candidates,
            },
        ] => {
            assert_eq!(column, &Ident::unquoted(name));
            (
                candidate_names(present_candidates),
                candidate_names(possible_candidates),
            )
        }
        other => panic!("expected one unverified reason: {other:?}"),
    }
}

const PARAMS: &str = r#"
SELECT
  (SELECT value.string_value FROM UNNEST(params) WHERE key = "page") AS page_bare,
  (SELECT p.value.string_value FROM UNNEST(params) AS p WHERE p.key = "page") AS page_aliased
FROM events
"#;

#[test]
fn element_field_over_a_relation_without_the_name_stays_open() {
    // `events` lists its columns and has no `value`, so only the element
    // can carry it; its type is unknown, so nothing shows that it does.
    let catalog = KnownTables(&[("events", &["id", "params"])]);
    let analysis = analyze_one_with(PARAMS, Dialect::BigQuery, Some(&catalog));
    assert_eq!(
        unverified(&analysis, "page_bare", "value"),
        (
            Vec::<String>::new(),
            vec!["element of [events.params]".to_string()]
        )
    );
    let aliased = find_mapping(&analysis.output.columns, "page_aliased");
    assert_eq!(
        known_sources(aliased)
            .iter()
            .map(|source| source.column.value.as_str())
            .collect::<Vec<_>>(),
        ["params"]
    );
    assert!(unresolved(aliased).is_empty());
}

#[test]
fn element_field_over_a_relation_with_unknown_columns_lists_both() {
    let analysis = analyze_one_with(PARAMS, Dialect::BigQuery, None);
    assert_eq!(
        unverified(&analysis, "page_bare", "value"),
        (
            Vec::<String>::new(),
            vec![
                "events".to_string(),
                "element of [events.params]".to_string()
            ]
        )
    );
}

#[test]
fn element_may_hide_a_column_of_the_enclosing_query() {
    let catalog = KnownTables(&[("events", &["id", "params", "value"])]);
    let analysis = analyze_one_with(PARAMS, Dialect::BigQuery, Some(&catalog));
    assert_eq!(
        unverified(&analysis, "page_bare", "value"),
        (
            vec!["events".to_string()],
            vec!["element of [events.params]".to_string()]
        )
    );
}

#[test]
fn element_in_the_same_from_clause_as_a_relation() {
    let catalog = KnownTables(&[("events", &["id", "params", "value"])]);
    let analysis = analyze_one_with(
        r#"SELECT value AS v, id FROM events, UNNEST(events.params)"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    assert_eq!(
        unverified(&analysis, "v", "value"),
        (
            vec!["events".to_string()],
            vec!["element of [events.params]".to_string()]
        )
    );
}

#[test]
fn offset_of_an_unaliased_unnest_is_a_known_column() {
    let catalog = KnownTables(&[("events", &["id", "params"])]);
    let analysis = analyze_one_with(
        r#"SELECT (SELECT MAX(offset) FROM UNNEST(params) WITH OFFSET) AS last_index FROM events"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    let column = find_mapping(&analysis.output.columns, "last_index");
    assert!(unresolved(column).is_empty(), "{column:?}");
}

#[test]
fn offset_of_an_unaliased_scalar_unnest_is_a_known_column() {
    // The element has no fields, but the expansion still publishes its
    // offset, which no relation of the enclosing query supplies.
    let catalog = KnownTables(&[("events", &["id", "offset"])]);
    let analysis = analyze_one_with(
        r#"SELECT
  (SELECT MAX(offset) FROM UNNEST([1, 2]) WITH OFFSET) AS last_index,
  (SELECT MAX(pos) FROM UNNEST(GENERATE_ARRAY(1, 3)) WITH OFFSET AS pos) AS last_pos
FROM events"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    for name in ["last_index", "last_pos"] {
        let column = find_mapping(&analysis.output.columns, name);
        assert!(unresolved(column).is_empty(), "{column:?}");
        assert!(known_sources(column).is_empty(), "{column:?}");
    }
}

#[test]
fn scalar_arrays_publish_no_field_names() {
    // A literal of scalars and a generated series have no fields, so names
    // still reach the enclosing query.
    let catalog = KnownTables(&[("events", &["id", "threshold"])]);
    let analysis = analyze_one_with(
        r#"SELECT
  (SELECT COUNT(*) FROM UNNEST([1, 2, 3]) WHERE threshold > 1) AS literal_count,
  (SELECT COUNT(*) FROM UNNEST(GENERATE_ARRAY(1, 10)) WHERE threshold > 1) AS series_count,
  (SELECT MAX(threshold) FROM UNNEST(GENERATE_DATE_ARRAY('2024-01-01', '2024-01-31'))) AS max_threshold
FROM events"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    let column = find_mapping(&analysis.output.columns, "max_threshold");
    assert!(unresolved(column).is_empty(), "{column:?}");
    assert_eq!(
        known_sources(column)
            .iter()
            .map(|source| source.column.value.as_str())
            .collect::<Vec<_>>(),
        ["threshold"]
    );
}

#[test]
fn other_dialects_keep_their_unnest_rules() {
    let catalog = KnownTables(&[("events", &["id", "params", "value"])]);
    let analysis = analyze_one_with(
        r#"SELECT (SELECT value FROM UNNEST(params)) AS v FROM events"#,
        Dialect::Generic,
        Some(&catalog),
    );
    let column = find_mapping(&analysis.output.columns, "v");
    assert!(
        !unresolved(column)
            .iter()
            .any(|reason| matches!(reason, Unresolved::Unverified { .. })),
        "{column:?}"
    );
}

/// The names of the known source columns of an output column, as
/// `relation.column`.
fn known_names(analysis: &dlin_sqllineage::StatementAnalysis, output: &str) -> Vec<String> {
    let column = find_mapping(&analysis.output.columns, output);
    assert!(unresolved(column).is_empty(), "{column:?}");
    known_sources(column)
        .iter()
        .map(|source| format!("{}.{}", source.relation.name.value, source.column.value))
        .collect()
}

#[test]
fn explicit_struct_type_decides_the_element_fields() {
    // The element publishes `key` and `value`, which hide the enclosing
    // relation's `value`; a name it does not publish reaches the enclosing
    // relation.
    let catalog = KnownTables(&[("events", &["id", "params", "value", "threshold"])]);
    let analysis = analyze_one_with(
        r#"SELECT
  (SELECT value.string_value
   FROM UNNEST(CAST(params AS ARRAY<STRUCT<key STRING, value STRUCT<string_value STRING>>>))
   WHERE key = "page") AS page,
  (SELECT MAX(threshold)
   FROM UNNEST(CAST(params AS ARRAY<STRUCT<key STRING, value STRING>>))) AS max_threshold
FROM events"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    assert_eq!(known_names(&analysis, "page"), ["events.params"]);
    assert_eq!(
        known_names(&analysis, "max_threshold"),
        ["events.threshold"]
    );
}

#[test]
fn struct_literals_decide_the_element_fields() {
    let catalog = KnownTables(&[("events", &["id", "amount", "fee", "label"])]);
    let analysis = analyze_one_with(
        r#"SELECT
  (SELECT SUM(v) FROM UNNEST([STRUCT('amount' AS k, amount AS v), STRUCT('fee' AS k, fee AS v)])) AS total,
  (SELECT MAX(v) FROM UNNEST([STRUCT<k STRING, v INT64>('amount', amount)])) AS typed,
  (SELECT MAX(label) FROM UNNEST([STRUCT('amount' AS k, amount AS v)])) AS outer_label,
  (SELECT MAX(k) FROM UNNEST([STRUCT('amount' AS k, amount AS v)])) AS constant_key,
  (SELECT MAX(k) FROM UNNEST([STRUCT<k STRING, v INT64>(label, amount)])) AS typed_key
FROM events"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    assert_eq!(
        known_names(&analysis, "total"),
        ["events.amount", "events.fee"]
    );
    assert_eq!(known_names(&analysis, "typed"), ["events.amount"]);
    assert_eq!(known_names(&analysis, "outer_label"), ["events.label"]);
    // Each field takes its value from its own expressions only.
    assert_eq!(known_names(&analysis, "constant_key"), Vec::<String>::new());
    assert_eq!(known_names(&analysis, "typed_key"), ["events.label"]);
}

#[test]
fn struct_literals_without_consistent_names_stay_unknown() {
    // Unnamed fields, and literals that disagree on their names, give no
    // element type to rely on.
    let catalog = KnownTables(&[("events", &["id", "amount", "fee"])]);
    let analysis = analyze_one_with(
        r#"SELECT
  (SELECT MAX(v) FROM UNNEST([STRUCT(amount, fee)])) AS unnamed,
  (SELECT MAX(v) FROM UNNEST([STRUCT(amount AS v), STRUCT(fee AS w)])) AS mixed
FROM events"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    for name in ["unnamed", "mixed"] {
        let (present, possible) = unverified(&analysis, name, "v");
        assert!(present.is_empty(), "{name}");
        assert_eq!(possible.len(), 1, "{name}: {possible:?}");
        assert!(
            possible[0].starts_with("element of"),
            "{name}: {possible:?}"
        );
    }
}

#[test]
fn a_merged_column_is_one_known_provider() {
    // USING merges the two `x` columns into one, so only the element can
    // compete with it.
    let catalog = KnownTables(&[("a", &["id", "x", "params"]), ("b", &["id", "x"])]);
    let analysis = analyze_one_with(
        r#"SELECT x AS v FROM a JOIN b USING (x), UNNEST(a.params)"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    let column = find_mapping(&analysis.output.columns, "v");
    assert!(
        matches!(unresolved(column), [Unresolved::Unverified { .. }]),
        "{column:?}"
    );
}

#[test]
fn known_fields_of_two_elements_are_ambiguous() {
    // Both elements publish `x`, and so does a relation that lists it next to
    // an element that publishes it: the name is ambiguous whatever else is
    // unknown, as BigQuery rejects it.
    let catalog = KnownTables(&[("events", &["id", "x"])]);
    for sql in [
        r#"SELECT x AS v FROM UNNEST([STRUCT(1 AS x)]), UNNEST([STRUCT(2 AS x)])"#,
        r#"SELECT x AS v FROM events, UNNEST([STRUCT(2 AS x)])"#,
        r#"SELECT x AS v FROM events AS e1, events AS e2, UNNEST(e1.params)"#,
        r#"SELECT x AS v FROM UNNEST([STRUCT(1 AS x)]), UNNEST([STRUCT(2 AS x)]), UNNEST(events_params)"#,
    ] {
        let analysis = analyze_one_with(sql, Dialect::BigQuery, Some(&catalog));
        let column = find_mapping(&analysis.output.columns, "v");
        let reasons = unresolved(column);
        assert!(
            reasons.iter().any(|reason| matches!(
                reason,
                Unresolved::Ambiguous { candidates, .. } if candidates.len() >= 2
            )) && !reasons
                .iter()
                .any(|reason| matches!(reason, Unresolved::Unverified { .. })),
            "{sql}: {column:?}"
        );
    }
}

#[test]
fn star_reads_the_unnamed_element_of_a_scalar_array() {
    let catalog = KnownTables(&[("events", &["id", "scores"])]);
    let analysis = analyze_one_with(
        r#"SELECT * FROM events, UNNEST(CAST(events.scores AS ARRAY<INT64>)) WITH OFFSET"#,
        Dialect::BigQuery,
        Some(&catalog),
    );
    let columns = &analysis.output.columns;
    assert_eq!(columns.len(), 4, "{columns:?}");
    assert_eq!(columns[2].name, dlin_sqllineage::OutputName::Anonymous);
    assert_eq!(
        known_sources(&columns[2])
            .iter()
            .map(|source| format!("{}.{}", source.relation.name.value, source.column.value))
            .collect::<Vec<_>>(),
        ["events.scores"]
    );
    assert!(unresolved(&columns[2]).is_empty(), "{:?}", columns[2]);
    assert_eq!(
        common::output_name(&columns[3]),
        Some("offset"),
        "{:?}",
        columns[3]
    );

    let literal = analyze_one_with(
        r#"SELECT * FROM UNNEST([10, 20]) WITH OFFSET"#,
        Dialect::BigQuery,
        None,
    );
    assert_eq!(literal.output.columns.len(), 2, "{:?}", literal.output);
    assert_eq!(
        literal.output.columns[0].name,
        dlin_sqllineage::OutputName::Anonymous
    );
}
