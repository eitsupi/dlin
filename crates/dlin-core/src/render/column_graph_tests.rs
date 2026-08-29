use super::*;
use crate::graph::column_lineage::{
    ColumnLineageEntry, ColumnSource, ImpactedColumn, ModelColumnLineage,
};

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
                sources: sources
                    .into_iter()
                    .map(|(table, column)| ColumnSource {
                        table: table.to_string(),
                        column: column.to_string(),
                        model_path: vec![],
                    })
                    .collect(),
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
            sources: vec![],
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
            sources: vec![ColumnSource {
                table: "raw".to_string(),
                column: "postcode".to_string(),
                model_path: vec![(
                    "stg_app".to_string(),
                    "area".to_string(),
                    TransformationType::Expression,
                )],
            }],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(graph_plain(&[report]));
}

#[test]
fn test_impact_plain() {
    let report = ColumnImpactReport {
        model: "stg_orders".to_string(),
        column: "order_id".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.orders".to_string(),
            model: "orders".to_string(),
            column: "order_id".to_string(),
            transformation: TransformationType::Direct,
            model_path: vec![(
                "orders".to_string(),
                "order_id".to_string(),
                TransformationType::Direct,
            )],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(impact_plain(&[report]));
}

#[test]
fn test_impact_plain_non_direct_intermediate() {
    let report = ColumnImpactReport {
        model: "raw".to_string(),
        column: "postcode".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.mart".to_string(),
            model: "mart".to_string(),
            column: "area".to_string(),
            transformation: TransformationType::Direct,
            model_path: vec![
                (
                    "stg_app".to_string(),
                    "area".to_string(),
                    TransformationType::Expression,
                ),
                (
                    "mart".to_string(),
                    "area".to_string(),
                    TransformationType::Direct,
                ),
            ],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(impact_plain(&[report]));
}

#[test]
fn test_impact_plain_multi_hop() {
    let report = ColumnImpactReport {
        model: "stg_orders".to_string(),
        column: "order_id".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.customers".to_string(),
            model: "customers".to_string(),
            column: "customer_order_id".to_string(),
            transformation: TransformationType::Direct,
            model_path: vec![
                (
                    "orders".to_string(),
                    "order_id".to_string(),
                    TransformationType::Direct,
                ),
                (
                    "customers".to_string(),
                    "customer_order_id".to_string(),
                    TransformationType::Direct,
                ),
            ],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(impact_plain(&[report]));
}

#[test]
fn test_impact_mermaid() {
    let report = ColumnImpactReport {
        model: "stg_orders".to_string(),
        column: "order_id".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.orders".to_string(),
            model: "orders".to_string(),
            column: "order_id".to_string(),
            transformation: TransformationType::Direct,
            model_path: vec![(
                "orders".to_string(),
                "order_id".to_string(),
                TransformationType::Direct,
            )],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(impact_mermaid(&[report]));
}

#[test]
fn test_impact_mermaid_indirect_edge_label() {
    let report = ColumnImpactReport {
        model: "stg_orders".to_string(),
        column: "order_id".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.customers".to_string(),
            model: "customers".to_string(),
            column: "customer_order_id".to_string(),
            transformation: TransformationType::Direct,
            model_path: vec![
                (
                    "orders".to_string(),
                    "order_id".to_string(),
                    TransformationType::Direct,
                ),
                (
                    "customers".to_string(),
                    "customer_order_id".to_string(),
                    TransformationType::Direct,
                ),
            ],
        }],
        errors: vec![],
    };
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
            sources: vec![ColumnSource {
                table: "raw".to_string(),
                column: "id".to_string(),
                model_path: vec![(
                    "stg_orders".to_string(),
                    "order_id".to_string(),
                    TransformationType::Direct,
                )],
            }],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(graph_dot(&[report]));
}

#[test]
fn test_dot_impact_single() {
    let report = ColumnImpactReport {
        model: "stg_orders".to_string(),
        column: "order_id".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.orders".to_string(),
            model: "orders".to_string(),
            column: "order_id".to_string(),
            transformation: TransformationType::Direct,
            model_path: vec![(
                "orders".to_string(),
                "order_id".to_string(),
                TransformationType::Direct,
            )],
        }],
        errors: vec![],
    };
    insta::assert_snapshot!(impact_dot(&[report]));
}

#[test]
fn test_dot_impact_indirect() {
    let report = ColumnImpactReport {
        model: "stg_orders".to_string(),
        column: "order_id".to_string(),
        impacted_columns: vec![ImpactedColumn {
            unique_id: "model.customers".to_string(),
            model: "customers".to_string(),
            column: "customer_order_id".to_string(),
            transformation: TransformationType::Expression,
            model_path: vec![
                (
                    "orders".to_string(),
                    "order_id".to_string(),
                    TransformationType::Direct,
                ),
                (
                    "customers".to_string(),
                    "customer_order_id".to_string(),
                    TransformationType::Expression,
                ),
            ],
        }],
        errors: vec![],
    };
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
