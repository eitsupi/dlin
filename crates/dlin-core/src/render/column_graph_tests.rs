use super::*;
use crate::graph::column_lineage::{
    ColumnLineageErrorKind, ColumnSource, ConfirmedImpact, DependencyKind, DependencyRef,
    ExplorationStatus, Ident, ImpactEdge, ImpactScope, KindExploration, ModelColumnLineage,
    ModelOutputRef, ModelPathHop, OutputName, OutputShape, OutputSlot, OutputSlotId, OutputSlots,
    Quote, TraceGap, UnexpandedStar, UnresolvedContribution, UnresolvedReason, ValueLineage,
};

fn output_ref(model: &str, slot: usize, column: &str) -> ModelOutputRef {
    ModelOutputRef {
        unique_id: format!("model.{model}"),
        model: model.to_string(),
        slot: OutputSlotId::new(slot),
        column: column.to_string(),
    }
}

fn hop(model: &str, slot: usize, column: &str, transformation: TransformationType) -> ModelPathHop {
    ModelPathHop {
        output: output_ref(model, slot, column),
        transformation,
    }
}

fn slot(
    index: usize,
    name: &str,
    transformation: TransformationType,
    lineage: ValueLineage<ColumnSource>,
) -> OutputSlot<ColumnSource> {
    OutputSlot {
        id: OutputSlotId::new(index),
        ordinal: Some(index),
        name: OutputName::Explicit(Ident::unquoted(name)),
        transformation,
        lineage,
    }
}

fn source(table: &str, column: &str) -> ColumnSource {
    ColumnSource {
        table: table.to_string(),
        column: Ident::unquoted(column),
        model_path: vec![],
    }
}

fn lineage_of(
    model: &str,
    columns: Vec<OutputSlot<ColumnSource>>,
    shape: OutputShape,
) -> ModelColumnLineage {
    let mut report = ModelColumnLineage {
        boundaries: Vec::new(),
        unique_id: format!("model.{model}"),
        model: model.to_string(),
        output: Some(OutputSlots { columns, shape }),
        listed_columns: 0,
        value_complete_columns: 0,
        value_partial_columns: 0,
        errors: vec![],
    };
    report.recount();
    report
}

fn impact_report(model: &str, column: &str, confirmed: Vec<ConfirmedImpact>) -> ColumnImpactReport {
    ColumnImpactReport {
        model: model.to_string(),
        column: column.to_string(),
        input: Some(output_ref(model, 0, column)),
        scope: ImpactScope::ValueOnly,
        confirmed,
        possible: vec![],
        exploration: vec![KindExploration {
            kind: DependencyKind::Value,
            status: ExplorationStatus::Complete,
        }],
        errors: vec![],
    }
}

/// A confirmed impact whose path is given as `(model, column, transformation)`
/// hops from the analysed column `input` to the target.
fn impacted(
    unique_id: &str,
    input: (&str, &str),
    hops: Vec<(&str, &str, TransformationType)>,
) -> ConfirmedImpact {
    let (target_model, target_column, transformation) = hops.last().cloned().unwrap();
    let mut path = Vec::new();
    let mut from = (input.0.to_string(), input.1.to_string());
    for (model, column, transformation) in &hops {
        let to = output_ref(model, 0, column);
        path.push(ImpactEdge {
            from: DependencyRef::Column(output_ref(&from.0, 0, &from.1)),
            to,
            kind: DependencyKind::Value,
            transformation: transformation.clone(),
        });
        from = (model.to_string(), column.to_string());
    }
    ConfirmedImpact {
        target: ModelOutputRef {
            unique_id: unique_id.to_string(),
            ..output_ref(target_model, 0, target_column)
        },
        kind: DependencyKind::Value,
        transformation,
        path,
    }
}

type SourceSpec<'a> = Vec<(&'a str, &'a str)>;
type LineageSpec<'a> = Vec<(&'a str, TransformationType, SourceSpec<'a>)>;

fn make_lineage(model: &str, entries: LineageSpec<'_>) -> ModelColumnLineage {
    lineage_of(
        model,
        entries
            .into_iter()
            .enumerate()
            .map(|(index, (col, trans, sources))| {
                slot(
                    index,
                    col,
                    trans,
                    ValueLineage::complete(
                        sources
                            .into_iter()
                            .map(|(table, column)| source(table, column))
                            .collect(),
                    ),
                )
            })
            .collect(),
        OutputShape::Closed,
    )
}

fn graph_plain(reports: &[ModelColumnLineage]) -> String {
    let mut buf = Vec::new();
    render_column_graph_plain_to_writer(reports, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn graph_mermaid(reports: &[ModelColumnLineage]) -> String {
    let mut buf = Vec::new();
    render_column_graph_mermaid_to_writer(reports, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn impact_plain(reports: &[ColumnImpactReport]) -> String {
    let mut buf = Vec::new();
    render_column_impact_plain_to_writer(reports, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn impact_mermaid(reports: &[ColumnImpactReport]) -> String {
    let mut buf = Vec::new();
    render_column_impact_mermaid_to_writer(reports, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn graph_dot(reports: &[ModelColumnLineage]) -> String {
    let mut buf = Vec::new();
    render_column_graph_dot_to_writer(reports, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

fn impact_dot(reports: &[ColumnImpactReport]) -> String {
    let mut buf = Vec::new();
    render_column_impact_dot_to_writer(reports, &mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

#[test]
fn test_plain_single_model() {
    let report = make_lineage(
        "orders",
        vec![
            (
                "order_id",
                TransformationType::Direct,
                vec![("stg_orders", "order_id")],
            ),
            (
                "total",
                TransformationType::Expression,
                vec![("stg_orders", "price")],
            ),
        ],
    );
    insta::assert_snapshot!(graph_plain(&[report]));
}

#[test]
fn test_plain_no_sources() {
    let report = lineage_of(
        "orders",
        vec![slot(
            0,
            "id",
            TransformationType::Unknown,
            ValueLineage::complete(vec![]),
        )],
        OutputShape::Closed,
    );
    insta::assert_snapshot!(graph_plain(&[report]));
}

#[test]
fn test_mermaid_single_model() {
    let report = make_lineage(
        "orders",
        vec![(
            "order_id",
            TransformationType::Direct,
            vec![("stg_orders", "order_id")],
        )],
    );
    insta::assert_snapshot!(graph_mermaid(&[report]));
}

#[test]
fn test_mermaid_dotted_table_name() {
    let report = make_lineage(
        "orders",
        vec![("id", TransformationType::Direct, vec![("raw.orders", "id")])],
    );
    insta::assert_snapshot!(graph_mermaid(&[report]));
}

#[test]
fn test_mermaid_id_collision_avoided() {
    // "raw.orders" and "raw_orders" must produce distinct subgraph IDs
    // despite sanitizing to the same string.
    let report = make_lineage(
        "raw_orders",
        vec![("id", TransformationType::Direct, vec![("raw.orders", "id")])],
    );
    insta::assert_snapshot!(graph_mermaid(&[report]));
}

#[test]
fn test_mermaid_label_escaping() {
    let report = make_lineage(
        "orders",
        vec![(
            "amount<usd>",
            TransformationType::Direct,
            vec![("raw.orders", "amount<usd>")],
        )],
    );
    insta::assert_snapshot!(graph_mermaid(&[report]));
}

#[test]
fn test_mermaid_empty_table_literal_label() {
    // table="" represents literal values (NULL, constants, UNNEST results).
    // Mermaid syntax requires a non-empty subgraph label; "(literal)" is used as fallback.
    let report = make_lineage(
        "orders",
        vec![("flag", TransformationType::Expression, vec![("", "NULL")])],
    );
    let output = graph_mermaid(&[report]);
    assert!(
        output.contains("(literal)"),
        "expected '(literal)' fallback label, got:\n{output}"
    );
    assert!(
        !output.contains(r#"subgraph sg0[""]"#),
        "empty subgraph label must not appear in output:\n{output}"
    );
}

#[test]
fn test_plain_empty_table_literal_label() {
    // table="" should render as "(literal).column" in plain output.
    let report = make_lineage(
        "orders",
        vec![("flag", TransformationType::Expression, vec![("", "NULL")])],
    );
    let output = graph_plain(&[report]);
    assert!(
        output.contains("(literal).NULL"),
        "expected '(literal).NULL' in plain output, got:\n{output}"
    );
}

#[test]
fn test_dot_empty_table_literal_label() {
    // table="" should render as label="(literal)" in DOT output, not label="".
    let report = make_lineage(
        "orders",
        vec![("flag", TransformationType::Expression, vec![("", "NULL")])],
    );
    let output = graph_dot(&[report]);
    assert!(
        output.contains("(literal)"),
        "expected '(literal)' in DOT output, got:\n{output}"
    );
    assert!(
        !output.contains(r#"label="";"#),
        "empty DOT cluster label must not appear in output:\n{output}"
    );
}

#[test]
fn test_plain_model_path_non_direct_annotation() {
    let report = lineage_of(
        "mart",
        vec![slot(
            0,
            "area",
            TransformationType::Direct,
            ValueLineage::complete(vec![ColumnSource {
                model_path: vec![hop("stg_app", 0, "area", TransformationType::Expression)],
                ..source("raw", "postcode")
            }]),
        )],
        OutputShape::Closed,
    );
    insta::assert_snapshot!(graph_plain(&[report]));
}

#[test]
fn test_impact_plain() {
    let report = impact_report(
        "stg_orders",
        "order_id",
        vec![impacted(
            "model.orders",
            ("stg_orders", "order_id"),
            vec![("orders", "order_id", TransformationType::Direct)],
        )],
    );
    insta::assert_snapshot!(impact_plain(&[report]));
}

#[test]
fn test_impact_plain_non_direct_intermediate() {
    let report = impact_report(
        "raw",
        "postcode",
        vec![impacted(
            "model.mart",
            ("raw", "postcode"),
            vec![
                ("stg_app", "area", TransformationType::Expression),
                ("mart", "area", TransformationType::Direct),
            ],
        )],
    );
    insta::assert_snapshot!(impact_plain(&[report]));
}

#[test]
fn test_impact_plain_multi_hop() {
    let report = impact_report(
        "stg_orders",
        "order_id",
        vec![impacted(
            "model.customers",
            ("stg_orders", "order_id"),
            vec![
                ("orders", "order_id", TransformationType::Direct),
                ("customers", "customer_order_id", TransformationType::Direct),
            ],
        )],
    );
    insta::assert_snapshot!(impact_plain(&[report]));
}

#[test]
fn test_impact_mermaid() {
    let report = impact_report(
        "stg_orders",
        "order_id",
        vec![impacted(
            "model.orders",
            ("stg_orders", "order_id"),
            vec![("orders", "order_id", TransformationType::Direct)],
        )],
    );
    insta::assert_snapshot!(impact_mermaid(&[report]));
}

#[test]
fn test_impact_mermaid_indirect_edge_label() {
    let report = impact_report(
        "stg_orders",
        "order_id",
        vec![impacted(
            "model.customers",
            ("stg_orders", "order_id"),
            vec![
                ("orders", "order_id", TransformationType::Direct),
                ("customers", "customer_order_id", TransformationType::Direct),
            ],
        )],
    );
    insta::assert_snapshot!(impact_mermaid(&[report]));
}

// ── DOT tests ─────────────────────────────────────────────────────────────

#[test]
fn test_dot_single_model() {
    let report = make_lineage(
        "orders",
        vec![(
            "order_id",
            TransformationType::Direct,
            vec![("stg_orders", "order_id")],
        )],
    );
    insta::assert_snapshot!(graph_dot(&[report]));
}

#[test]
fn test_dot_all_transformation_types() {
    let report = make_lineage(
        "orders",
        vec![
            ("id", TransformationType::Direct, vec![("raw", "id")]),
            (
                "total",
                TransformationType::Aggregation,
                vec![("raw", "amount")],
            ),
            ("label", TransformationType::Expression, vec![("raw", "a")]),
            ("id_cast", TransformationType::Cast, vec![("raw", "id_str")]),
            (
                "status",
                TransformationType::Conditional,
                vec![("raw", "flag")],
            ),
            ("mystery", TransformationType::Unknown, vec![("raw", "x")]),
        ],
    );
    insta::assert_snapshot!(graph_dot(&[report]));
}

#[test]
fn test_dot_id_collision_avoided() {
    // "raw.orders" and "raw_orders" would collide if IDs were sanitized
    let report = make_lineage(
        "raw_orders",
        vec![("id", TransformationType::Direct, vec![("raw.orders", "id")])],
    );
    insta::assert_snapshot!(graph_dot(&[report]));
}

#[test]
fn test_dot_via_path() {
    let report = lineage_of(
        "orders",
        vec![slot(
            0,
            "order_id",
            TransformationType::Direct,
            ValueLineage::complete(vec![ColumnSource {
                model_path: vec![hop("stg_orders", 0, "order_id", TransformationType::Direct)],
                ..source("raw", "id")
            }]),
        )],
        OutputShape::Closed,
    );
    insta::assert_snapshot!(graph_dot(&[report]));
}

#[test]
fn test_dot_impact_single() {
    let report = impact_report(
        "stg_orders",
        "order_id",
        vec![impacted(
            "model.orders",
            ("stg_orders", "order_id"),
            vec![("orders", "order_id", TransformationType::Direct)],
        )],
    );
    insta::assert_snapshot!(impact_dot(&[report]));
}

#[test]
fn test_dot_impact_indirect() {
    let report = impact_report(
        "stg_orders",
        "order_id",
        vec![impacted(
            "model.customers",
            ("stg_orders", "order_id"),
            vec![
                ("orders", "order_id", TransformationType::Direct),
                (
                    "customers",
                    "customer_order_id",
                    TransformationType::Expression,
                ),
            ],
        )],
    );
    insta::assert_snapshot!(impact_dot(&[report]));
}

#[test]
fn test_dot_escapes_special_chars() {
    // Model and column names containing `"` and `\` must be properly escaped
    // in DOT double-quoted strings to produce syntactically valid output.
    let report = make_lineage(
        r#"schema."orders""#,
        vec![(
            r#"col\"name"#,
            TransformationType::Direct,
            vec![(r#"raw\data"#, r#"id"field""#)],
        )],
    );
    insta::assert_snapshot!(graph_dot(&[report]));
}

// ── Partial lineage and incomplete exploration ────────────────────────────

fn behind_star_reason() -> UnresolvedReason {
    UnresolvedReason::BehindStar {
        relation: crate::graph::column_lineage::RelationRef::parse("raw.fees").unwrap(),
    }
}

fn partial_report() -> ModelColumnLineage {
    lineage_of(
        "partial_model",
        vec![slot(
            0,
            "total",
            TransformationType::Expression,
            ValueLineage::new(
                vec![source("source_model", "x")],
                vec![UnresolvedContribution {
                    reason: behind_star_reason(),
                    origin: output_ref("partial_model", 0, "total"),
                    model_path: vec![],
                }],
            ),
        )],
        OutputShape::Closed,
    )
}

#[test]
fn test_plain_partial_lineage_shows_known_sources_and_unresolved_part() {
    let output = graph_plain(&[partial_report()]);
    assert!(
        output.contains("→  source_model.x (expression)"),
        "{output}"
    );
    assert!(
        output.contains("unresolved: may come from an unexpanded star of raw.fees"),
        "{output}"
    );
}

#[test]
fn test_plain_partial_without_known_sources_is_not_called_empty() {
    let mut report = partial_report();
    let columns = &mut report.output.as_mut().unwrap().columns;
    columns[0].lineage = ValueLineage::new(vec![], columns[0].lineage.unresolved().to_vec());
    let output = graph_plain(&[report]);
    assert!(output.contains("(no known sources)"), "{output}");
    assert!(!output.contains("(no sources)"), "{output}");
    assert!(output.contains("unresolved:"), "{output}");
}

#[test]
fn test_mermaid_and_dot_mark_partial_columns() {
    let mermaid = graph_mermaid(&[partial_report()]);
    assert!(mermaid.contains("total (partial)"), "{mermaid}");
    let dot = graph_dot(&[partial_report()]);
    assert!(dot.contains("total (partial)"), "{dot}");
}

fn incomplete_report(confirmed: Vec<ConfirmedImpact>) -> ColumnImpactReport {
    let mut report = impact_report("source_model", "x", confirmed);
    report.exploration = vec![KindExploration {
        kind: DependencyKind::Value,
        status: ExplorationStatus::Incomplete {
            gaps: vec![
                TraceGap::UnresolvedLineage {
                    at: output_ref("partial_model", 0, "total"),
                    reasons: vec![behind_star_reason()],
                },
                TraceGap::NotCollected {
                    kind: DependencyKind::Row,
                },
            ],
        },
    }];
    report
}

#[test]
fn test_impact_plain_never_reports_no_impact_for_incomplete_exploration() {
    let output = impact_plain(&[incomplete_report(vec![])]);
    assert!(
        output.contains("no confirmed impact; exploration incomplete"),
        "{output}"
    );
    assert!(!output.contains("no impact)"), "{output}");
    assert!(
        output.contains("unresolved lineage at partial_model.total"),
        "{output}"
    );
    assert!(
        output.contains("row dependencies are not collected"),
        "{output}"
    );
}

#[test]
fn test_impact_plain_lists_confirmed_impacts_next_to_the_gaps() {
    let confirmed = vec![impacted(
        "model.partial_model",
        ("source_model", "x"),
        vec![("partial_model", "total", TransformationType::Expression)],
    )];
    let output = impact_plain(&[incomplete_report(confirmed)]);
    assert!(
        output.contains("1 impacted column; exploration incomplete"),
        "{output}"
    );
    assert!(
        output.contains("value exploration incomplete (2 gaps):"),
        "{output}"
    );
}

#[test]
fn test_impact_plain_says_no_impact_only_when_complete() {
    let output = impact_plain(&[impact_report("leaf", "x", vec![])]);
    assert!(output.contains("leaf.x (no impact)"), "{output}");
    assert!(!output.contains("exploration incomplete"), "{output}");
}

#[test]
fn test_impact_mermaid_and_dot_mark_gap_nodes() {
    let report = incomplete_report(vec![]);
    let mermaid = impact_mermaid(std::slice::from_ref(&report));
    assert!(mermaid.contains("total (partial)"), "{mermaid}");
    let dot = impact_dot(&[report]);
    assert!(dot.contains("total (partial)"), "{dot}");
}

fn gap_report(scope: ImpactScope, exploration: Vec<KindExploration>) -> ColumnImpactReport {
    let mut report = impact_report("source_model", "x", vec![]);
    report.scope = scope;
    report.exploration = exploration;
    report
}

fn failed_exploration() -> KindExploration {
    KindExploration {
        kind: DependencyKind::Value,
        status: ExplorationStatus::Incomplete {
            gaps: vec![TraceGap::AnalysisFailed {
                unique_id: "model.proj.broken\"model".to_string(),
                model: "broken\"model".to_string(),
                error: ColumnLineageErrorKind::ParseFailure,
            }],
        },
    }
}

fn row_not_collected_exploration() -> Vec<KindExploration> {
    vec![
        KindExploration {
            kind: DependencyKind::Value,
            status: ExplorationStatus::Complete,
        },
        KindExploration {
            kind: DependencyKind::Row,
            status: ExplorationStatus::Incomplete {
                gaps: vec![TraceGap::NotCollected {
                    kind: DependencyKind::Row,
                }],
            },
        },
    ]
}

#[test]
fn test_impact_diagrams_show_failed_dependent_model() {
    let report = gap_report(ImpactScope::ValueOnly, vec![failed_exploration()]);

    let plain = impact_plain(std::slice::from_ref(&report));
    assert!(
        plain.contains("analysis of broken\"model failed"),
        "{plain}"
    );

    let mermaid = impact_mermaid(std::slice::from_ref(&report));
    assert!(
        mermaid.contains("gap0[\"broken#quot;model: analysis failed\"]"),
        "{mermaid}"
    );
    assert!(!mermaid.contains("-->"), "{mermaid}");

    let dot = impact_dot(&[report]);
    assert!(
        dot.contains(
            r##""gap0" [label="broken\"model: analysis failed", shape=note, fillcolor="#F5B7B1"];"##
        ),
        "{dot}"
    );
}

#[test]
fn test_impact_diagrams_show_kind_not_collected() {
    let report = gap_report(ImpactScope::ValueAndRow, row_not_collected_exploration());

    let plain = impact_plain(std::slice::from_ref(&report));
    assert!(
        plain.contains("row dependencies are not collected"),
        "{plain}"
    );

    let mermaid = impact_mermaid(std::slice::from_ref(&report));
    assert!(
        mermaid.contains("gap0[\"row dependencies not collected\"]"),
        "{mermaid}"
    );

    let dot = impact_dot(&[report]);
    assert!(
        dot.contains(
            r##""gap0" [label="row dependencies not collected", shape=note, fillcolor="#F5B7B1"];"##
        ),
        "{dot}"
    );
}

#[test]
fn test_impact_diagrams_have_no_gap_notes_when_complete() {
    let report = impact_report("leaf", "x", vec![]);
    assert!(!impact_mermaid(std::slice::from_ref(&report)).contains("gap0"));
    assert!(!impact_dot(&[report]).contains("gap0"));
}

// ── Output slots: order, duplicates, quoting and unlisted columns ─────────

fn duplicate_report() -> ModelColumnLineage {
    lineage_of(
        "mart",
        vec![
            slot(
                0,
                "x",
                TransformationType::Direct,
                ValueLineage::complete(vec![source("raw", "a")]),
            ),
            slot(
                1,
                "x",
                TransformationType::Direct,
                ValueLineage::complete(vec![source("raw", "b")]),
            ),
        ],
        OutputShape::Closed,
    )
}

#[test]
fn test_plain_lists_columns_in_slot_order_and_keeps_duplicates() {
    let report = make_lineage(
        "mart",
        vec![
            ("z", TransformationType::Direct, vec![("raw", "z")]),
            ("a", TransformationType::Direct, vec![("raw", "a")]),
            ("a", TransformationType::Cast, vec![("raw", "b")]),
        ],
    );
    let output = graph_plain(&[report]);
    let rows: Vec<&str> = output.lines().skip(1).collect();
    assert_eq!(
        rows,
        [
            "  z  →  raw.z (direct)",
            "  a  →  raw.a (direct)",
            "  a  →  raw.b (cast)"
        ]
    );
}

#[test]
fn test_plain_header_shows_the_three_counts() {
    let mut report = partial_report();
    let columns = &mut report.output.as_mut().unwrap().columns;
    let mut second = columns[0].clone();
    second.id = OutputSlotId::new(1);
    second.lineage = ValueLineage::complete(vec![source("raw", "b")]);
    columns.push(second);
    report.recount();
    let output = graph_plain(&[report]);
    assert_eq!(
        output.lines().next(),
        Some("partial_model (2 columns listed: 1 complete, 1 partial)")
    );
}

#[test]
fn test_mermaid_and_dot_draw_duplicate_names_as_distinct_nodes() {
    let mermaid = graph_mermaid(&[duplicate_report()]);
    // Two nodes labelled x, told apart by their slot, and one edge each.
    assert_eq!(mermaid.matches("[\"x\"]").count(), 2, "{mermaid}");
    assert!(mermaid.contains("n0_0[\"x\"]"), "{mermaid}");
    assert!(mermaid.contains("n0_1[\"x\"]"), "{mermaid}");
    assert!(mermaid.contains("n1_0 -->|\"direct\"|n0_0"), "{mermaid}");
    assert!(mermaid.contains("n1_1 -->|\"direct\"|n0_1"), "{mermaid}");

    let dot = graph_dot(&[duplicate_report()]);
    assert_eq!(dot.matches("label=\"x\"").count(), 2, "{dot}");
    assert!(dot.contains("\"n1_0\" -> \"n0_0\""), "{dot}");
    assert!(dot.contains("\"n1_1\" -> \"n0_1\""), "{dot}");
}

#[test]
fn test_quoted_names_keep_their_quotes_in_labels() {
    let mut report = duplicate_report();
    let columns = &mut report.output.as_mut().unwrap().columns;
    columns[1].name = OutputName::Explicit(Ident::quoted("x", Quote::Double));
    columns[1].lineage = ValueLineage::complete(vec![ColumnSource {
        column: Ident::quoted("b", Quote::Backtick),
        ..source("raw", "b")
    }]);
    let plain = graph_plain(&[report]);
    assert!(plain.contains("\"x\"  →  raw.`b` (direct)"), "{plain}");
}

fn open_report() -> ModelColumnLineage {
    lineage_of(
        "mart",
        vec![slot(
            0,
            "a",
            TransformationType::Direct,
            ValueLineage::complete(vec![source("raw", "a")]),
        )],
        OutputShape::Open {
            unexpanded: vec![
                UnexpandedStar {
                    relation: Some(
                        crate::graph::column_lineage::RelationRef::parse("raw.fees").unwrap(),
                    ),
                    after_ordinal: Some(0),
                },
                UnexpandedStar {
                    relation: None,
                    after_ordinal: None,
                },
            ],
        },
    )
}

#[test]
fn test_open_shape_is_shown_as_a_modest_note() {
    let plain = graph_plain(&[open_report()]);
    assert!(
        plain.contains("  (plus unexpanded columns from raw.fees)"),
        "{plain}"
    );
    assert!(plain.contains("  (plus unexpanded columns)"), "{plain}");
    // The note is not counted as a listed column.
    assert!(plain.starts_with("mart (1 column listed: 1 complete, 0 partial)"));

    let mermaid = graph_mermaid(&[open_report()]);
    assert!(
        mermaid.contains("plus unexpanded columns from raw.fees"),
        "{mermaid}"
    );
    let dot = graph_dot(&[open_report()]);
    assert!(
        dot.contains("plus unexpanded columns from raw.fees"),
        "{dot}"
    );
    assert!(dot.contains("dashed"), "{dot}");
    // Notes are nodes without edges.
    assert_eq!(mermaid.matches("-->").count(), 1, "{mermaid}");
}

#[test]
fn test_impact_diagrams_keep_same_named_targets_apart() {
    let mut first = impacted(
        "model.mid",
        ("base", "a"),
        vec![("mid", "x", TransformationType::Direct)],
    );
    let mut second = impacted(
        "model.mid",
        ("base", "b"),
        vec![("mid", "x", TransformationType::Direct)],
    );
    first.target.slot = OutputSlotId::new(0);
    second.target.slot = OutputSlotId::new(1);
    first.path[0].to.slot = OutputSlotId::new(0);
    second.path[0].to.slot = OutputSlotId::new(1);
    second.path[0].from = DependencyRef::Column(output_ref("base", 1, "b"));
    let report = impact_report("base", "a", vec![first, second]);

    let mermaid = impact_mermaid(std::slice::from_ref(&report));
    assert_eq!(mermaid.matches("[\"x\"]").count(), 2, "{mermaid}");
    let dot = impact_dot(&[report]);
    assert_eq!(dot.matches("label=\"x\"").count(), 2, "{dot}");
}

#[test]
fn test_impact_gap_for_a_dependent_with_unlisted_columns() {
    let mut report = impact_report("base", "a", vec![]);
    report.exploration = vec![KindExploration {
        kind: DependencyKind::Value,
        status: ExplorationStatus::Incomplete {
            gaps: vec![TraceGap::OpenOutput {
                unique_id: "model.reader".to_string(),
                model: "reader".to_string(),
                unexpanded: vec![UnexpandedStar {
                    relation: None,
                    after_ordinal: None,
                }],
            }],
        },
    }];
    let plain = impact_plain(std::slice::from_ref(&report));
    assert!(
        plain.contains("reader has columns that are not listed"),
        "{plain}"
    );
    assert!(plain.contains("exploration incomplete"), "{plain}");
    let mermaid = impact_mermaid(&[report]);
    assert!(
        mermaid.contains("reader: unlisted columns may depend on the input"),
        "{mermaid}"
    );
}

#[test]
fn test_impact_diagrams_draw_an_unlisted_input_as_a_named_node() {
    let mut report = impact_report("mid", "hidden", vec![]);
    report.input = None;
    let mut found = impacted(
        "model.top",
        ("mid", "hidden"),
        vec![("top", "seen", TransformationType::Direct)],
    );
    found.path[0].from = DependencyRef::UnidentifiedColumn {
        unique_id: "model.mid".to_string(),
        model: "mid".to_string(),
        column: "hidden".to_string(),
    };
    report.confirmed = vec![found];
    report.exploration = vec![KindExploration {
        kind: DependencyKind::Value,
        status: ExplorationStatus::Incomplete {
            gaps: vec![TraceGap::UnidentifiedInput {
                unique_id: "model.mid".to_string(),
                model: "mid".to_string(),
                column: "hidden".to_string(),
                candidates: vec![],
            }],
        },
    }];
    let mermaid = impact_mermaid(std::slice::from_ref(&report));
    assert!(mermaid.contains("[\"hidden\"]"), "{mermaid}");
    assert_eq!(mermaid.matches("-->").count(), 1, "{mermaid}");
    // One node for the input: the root and the edge start agree.
    assert_eq!(mermaid.matches("[\"hidden\"]").count(), 1, "{mermaid}");
    assert!(
        mermaid.contains("mid.hidden: input not identified"),
        "{mermaid}"
    );
    let plain = impact_plain(&[report]);
    assert!(
        plain.contains("is not an enumerated output of mid"),
        "{plain}"
    );
}
