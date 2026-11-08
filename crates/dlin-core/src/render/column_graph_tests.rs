use super::*;
use crate::graph::column_lineage::{
    ColumnLineageEntry, ColumnSource, ConfirmedImpact, DependencyKind, DependencyRef,
    ExplorationStatus, ImpactEdge, ImpactScope, KindExploration, ModelColumnLineage,
    ModelOutputRef, TraceGap, UnresolvedContribution, UnresolvedReason, ValueLineage,
};

fn impact_report(model: &str, column: &str, confirmed: Vec<ConfirmedImpact>) -> ColumnImpactReport {
    ColumnImpactReport {
        model: model.to_string(),
        column: column.to_string(),
        scope: ImpactScope::ValueOnly,
        confirmed,
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
        let to = ModelOutputRef {
            unique_id: format!("model.{model}"),
            model: model.to_string(),
            column: column.to_string(),
        };
        path.push(ImpactEdge {
            from: DependencyRef::Column {
                unique_id: format!("model.{}", from.0),
                model: from.0.clone(),
                column: from.1.clone(),
            },
            to,
            kind: DependencyKind::Value,
            transformation: transformation.clone(),
        });
        from = (model.to_string(), column.to_string());
    }
    ConfirmedImpact {
        target: ModelOutputRef {
            unique_id: unique_id.to_string(),
            model: target_model.to_string(),
            column: target_column.to_string(),
        },
        kind: DependencyKind::Value,
        transformation,
        path,
    }
}

type SourceSpec<'a> = Vec<(&'a str, &'a str)>;
type LineageSpec<'a> = Vec<(&'a str, TransformationType, SourceSpec<'a>)>;

fn make_lineage(model: &str, entries: LineageSpec<'_>) -> ModelColumnLineage {
    let traced = entries.len();
    let total = entries.len();
    ModelColumnLineage {
        model: model.to_string(),
        traced_columns: traced,
        total_columns: total,
        columns: entries
            .into_iter()
            .map(|(col, trans, sources)| ColumnLineageEntry {
                column: col.to_string(),
                transformation: trans,
                lineage: ValueLineage::complete(
                    sources
                        .into_iter()
                        .map(|(table, column)| ColumnSource {
                            table: table.to_string(),
                            column: column.to_string(),
                            model_path: vec![],
                        })
                        .collect(),
                ),
            })
            .collect(),
        errors: vec![],
    }
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
    let report = ModelColumnLineage {
        model: "orders".to_string(),
        traced_columns: 0,
        total_columns: 1,
        columns: vec![ColumnLineageEntry {
            column: "id".to_string(),
            transformation: TransformationType::Unknown,
            lineage: ValueLineage::complete(vec![]),
        }],
        errors: vec![],
    };
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
    let report = ModelColumnLineage {
        model: "mart".to_string(),
        traced_columns: 1,
        total_columns: 1,
        columns: vec![ColumnLineageEntry {
            column: "area".to_string(),
            transformation: TransformationType::Direct,
            lineage: ValueLineage::complete(vec![ColumnSource {
                table: "raw".to_string(),
                column: "postcode".to_string(),
                model_path: vec![(
                    "stg_app".to_string(),
                    "area".to_string(),
                    TransformationType::Expression,
                )],
            }]),
        }],
        errors: vec![],
    };
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
    let report = ModelColumnLineage {
        model: "orders".to_string(),
        traced_columns: 1,
        total_columns: 1,
        columns: vec![ColumnLineageEntry {
            column: "order_id".to_string(),
            transformation: TransformationType::Direct,
            lineage: ValueLineage::complete(vec![ColumnSource {
                table: "raw".to_string(),
                column: "id".to_string(),
                model_path: vec![(
                    "stg_orders".to_string(),
                    "order_id".to_string(),
                    TransformationType::Direct,
                )],
            }]),
        }],
        errors: vec![],
    };
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
    ModelColumnLineage {
        model: "partial_model".to_string(),
        traced_columns: 0,
        total_columns: 1,
        columns: vec![ColumnLineageEntry {
            column: "total".to_string(),
            transformation: TransformationType::Expression,
            lineage: ValueLineage::new(
                vec![ColumnSource {
                    table: "source_model".to_string(),
                    column: "x".to_string(),
                    model_path: vec![],
                }],
                vec![UnresolvedContribution {
                    reason: behind_star_reason(),
                    origin: ModelOutputRef {
                        unique_id: "model.proj.partial_model".to_string(),
                        model: "partial_model".to_string(),
                        column: "total".to_string(),
                    },
                    model_path: vec![],
                }],
            ),
        }],
        errors: vec![],
    }
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
    report.columns[0].lineage =
        ValueLineage::new(vec![], report.columns[0].lineage.unresolved().to_vec());
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
                    at: ModelOutputRef {
                        unique_id: "model.proj.partial_model".to_string(),
                        model: "partial_model".to_string(),
                        column: "total".to_string(),
                    },
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
