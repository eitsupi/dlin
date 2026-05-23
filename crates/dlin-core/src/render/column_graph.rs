use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use crate::graph::column_lineage::{ColumnImpactReport, ModelColumnLineage, TransformationType};

// ── Column graph (upstream lineage) ──────────────────────────────────────────

pub fn render_column_graph_plain(reports: &[ModelColumnLineage]) {
    super::handle_stdout_result(render_column_graph_plain_to_writer(
        reports,
        &mut std::io::stdout().lock(),
    ));
}

pub fn render_column_graph_plain_to_writer<W: Write>(
    reports: &[ModelColumnLineage],
    w: &mut W,
) -> io::Result<()> {
    for (i, report) in reports.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        writeln!(
            w,
            "{} ({}/{} columns traced)",
            report.model, report.traced_columns, report.total_columns
        )?;

        if report.columns.is_empty() {
            continue;
        }

        // Measure column name width for alignment
        let col_width = report
            .columns
            .iter()
            .map(|e| e.column.len())
            .max()
            .unwrap_or(0);

        for entry in &report.columns {
            if entry.sources.is_empty() {
                writeln!(
                    w,
                    "  {:width$}  (no sources)",
                    entry.column,
                    width = col_width
                )?;
                continue;
            }

            let first = &entry.sources[0];
            let src_str = format_source(first.table.as_str(), first.column.as_str(), &first.model_path);
            writeln!(
                w,
                "  {:width$}  →  {} ({})",
                entry.column,
                src_str,
                transformation_label(&entry.transformation),
                width = col_width
            )?;

            // Additional sources on continuation lines
            let indent = " ".repeat(2 + col_width + 2 + 3 + 2); // "  " + col + "  →  "
            for src in entry.sources.iter().skip(1) {
                let src_str = format_source(src.table.as_str(), src.column.as_str(), &src.model_path);
                writeln!(
                    w,
                    "{}  {} ({})",
                    indent,
                    src_str,
                    transformation_label(&entry.transformation),
                )?;
            }
        }
    }
    Ok(())
}

pub fn render_column_graph_mermaid(reports: &[ModelColumnLineage]) {
    super::handle_stdout_result(render_column_graph_mermaid_to_writer(
        reports,
        &mut std::io::stdout().lock(),
    ));
}

pub fn render_column_graph_mermaid_to_writer<W: Write>(
    reports: &[ModelColumnLineage],
    w: &mut W,
) -> io::Result<()> {
    writeln!(w, "flowchart LR")?;

    // Collect all models and their columns to build subgraphs.
    // Key: model name → BTreeSet of column names (sorted)
    let mut model_columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Collect edges: (target_model, target_col, src_model, src_col, label)
    let mut edges: Vec<(String, String, String, String, String)> = Vec::new();

    for report in reports {
        let target_model = &report.model;
        model_columns.entry(target_model.clone()).or_default();

        for entry in &report.columns {
            model_columns
                .entry(target_model.clone())
                .or_default()
                .insert(entry.column.clone());

            for src in &entry.sources {
                // Register the source model and column
                model_columns
                    .entry(src.table.clone())
                    .or_default()
                    .insert(src.column.clone());

                // Add intermediate models from model_path (no column info available)
                for mid in &src.model_path {
                    model_columns.entry(mid.clone()).or_default();
                }

                edges.push((
                    target_model.clone(),
                    entry.column.clone(),
                    src.table.clone(),
                    src.column.clone(),
                    transformation_label(&entry.transformation).to_string(),
                ));
            }
        }
    }

    // Build stable index map to avoid ID collisions between models whose names
    // differ only in characters that sanitize_id maps to '_' (e.g. "raw.orders"
    // vs "raw_orders" both become "raw_orders").
    let model_index: BTreeMap<&str, usize> = model_columns
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();

    // Render subgraphs
    for (model, columns) in &model_columns {
        let midx = model_index[model.as_str()];
        writeln!(
            w,
            "  subgraph sg{}[\"{}\"]",
            midx,
            super::mermaid_escape(model)
        )?;
        for (cidx, col) in columns.iter().enumerate() {
            writeln!(
                w,
                "    n{}_{}[\"{}\"]",
                midx,
                cidx,
                super::mermaid_escape(col)
            )?;
        }
        writeln!(w, "  end")?;
    }

    writeln!(w)?;

    // Render edges (deduplicated)
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (from_model, from_col, to_model, to_col, label) in &edges {
        let from_node = indexed_node_id(&model_index, &model_columns, from_model, from_col);
        let to_node = indexed_node_id(&model_index, &model_columns, to_model, to_col);
        let edge_str = format!("  {} -->|\"{}\"|{}", to_node, label, from_node);
        if seen.insert(edge_str.clone()) {
            writeln!(w, "{}", edge_str)?;
        }
    }

    Ok(())
}

// ── Column impact (downstream analysis) ──────────────────────────────────────

pub fn render_column_impact_plain(reports: &[ColumnImpactReport]) {
    super::handle_stdout_result(render_column_impact_plain_to_writer(
        reports,
        &mut std::io::stdout().lock(),
    ));
}

pub fn render_column_impact_plain_to_writer<W: Write>(
    reports: &[ColumnImpactReport],
    w: &mut W,
) -> io::Result<()> {
    for (i, report) in reports.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        writeln!(
            w,
            "{}.{} ({} impacted column{})",
            report.model,
            report.column,
            report.impacted_columns.len(),
            if report.impacted_columns.len() == 1 { "" } else { "s" },
        )?;

        if report.impacted_columns.is_empty() {
            continue;
        }

        let col_width = report
            .impacted_columns
            .iter()
            .map(|c| c.column.len())
            .max()
            .unwrap_or(0);

        for ic in &report.impacted_columns {
            // model_path ends with ic.model itself (BFS always pushes dep_name).
            // The intermediate hops are everything before the last element.
            let via_str = if ic.model_path.len() > 1 {
                let intermediate = &ic.model_path[..ic.model_path.len() - 1];
                format!(", via {}", intermediate.join(" → "))
            } else {
                String::new()
            };
            writeln!(
                w,
                "  {:width$}  →  {}.{} ({}{})",
                ic.column,
                ic.model,
                ic.column,
                transformation_label(&ic.transformation),
                via_str,
                width = col_width
            )?;
        }
    }
    Ok(())
}

pub fn render_column_impact_mermaid(reports: &[ColumnImpactReport]) {
    super::handle_stdout_result(render_column_impact_mermaid_to_writer(
        reports,
        &mut std::io::stdout().lock(),
    ));
}

pub fn render_column_impact_mermaid_to_writer<W: Write>(
    reports: &[ColumnImpactReport],
    w: &mut W,
) -> io::Result<()> {
    writeln!(w, "flowchart LR")?;

    let mut model_columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Collect edges: (src_model, src_col, target_model, target_col, label)
    let mut edges: Vec<(String, String, String, String, String)> = Vec::new();

    for report in reports {
        // Source node (the column being analyzed for impact)
        model_columns
            .entry(report.model.clone())
            .or_default()
            .insert(report.column.clone());

        for ic in &report.impacted_columns {
            model_columns
                .entry(ic.model.clone())
                .or_default()
                .insert(ic.column.clone());

            // For multi-hop impacts, label the edge with the intermediate path so
            // the diagram does not imply a direct connection that does not exist.
            // model_path ends with ic.model; the intermediate hops precede it.
            let via_str = if ic.model_path.len() > 1 {
                let intermediate = &ic.model_path[..ic.model_path.len() - 1];
                format!(" (via {})", intermediate.join(" → "))
            } else {
                String::new()
            };
            let label = format!(
                "{}{}",
                transformation_label(&ic.transformation),
                via_str
            );

            // Edge direction: source column → impacted column
            edges.push((
                report.model.clone(),
                report.column.clone(),
                ic.model.clone(),
                ic.column.clone(),
                label,
            ));
        }
    }

    // Build stable index map to avoid ID collisions (same reason as column graph).
    let model_index: BTreeMap<&str, usize> = model_columns
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();

    for (model, columns) in &model_columns {
        let midx = model_index[model.as_str()];
        writeln!(
            w,
            "  subgraph sg{}[\"{}\"]",
            midx,
            super::mermaid_escape(model)
        )?;
        for (cidx, col) in columns.iter().enumerate() {
            writeln!(
                w,
                "    n{}_{}[\"{}\"]",
                midx,
                cidx,
                super::mermaid_escape(col)
            )?;
        }
        writeln!(w, "  end")?;
    }

    writeln!(w)?;

    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (from_model, from_col, to_model, to_col, label) in &edges {
        let from_node = indexed_node_id(&model_index, &model_columns, from_model, from_col);
        let to_node = indexed_node_id(&model_index, &model_columns, to_model, to_col);
        let edge_str = format!("  {} -->|\"{}\"|{}", from_node, label, to_node);
        if seen.insert(edge_str.clone()) {
            writeln!(w, "{}", edge_str)?;
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn transformation_label(t: &TransformationType) -> &'static str {
    match t {
        TransformationType::Direct => "direct",
        TransformationType::Aggregation => "aggregation",
        TransformationType::Expression => "expression",
        TransformationType::Cast => "cast",
        TransformationType::Conditional => "conditional",
        TransformationType::Unknown => "unknown",
    }
}

fn format_source(table: &str, column: &str, model_path: &[String]) -> String {
    if model_path.is_empty() {
        format!("{}.{}", table, column)
    } else {
        format!("{}.{} via {}", table, column, model_path.join(" → "))
    }
}

/// Return the Mermaid node ID for `(model, col)` using index-based stable IDs.
fn indexed_node_id(
    model_index: &BTreeMap<&str, usize>,
    model_columns: &BTreeMap<String, BTreeSet<String>>,
    model: &str,
    col: &str,
) -> String {
    let midx = model_index.get(model).copied().unwrap_or(0);
    let cidx = model_columns
        .get(model)
        .and_then(|cols| cols.iter().position(|c| c.as_str() == col))
        .unwrap_or(0);
    format!("n{}_{}", midx, cidx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::column_lineage::{
        ColumnLineageEntry, ColumnSource, ImpactedColumn, ModelColumnLineage,
    };

    fn make_lineage(model: &str, entries: Vec<(&str, TransformationType, Vec<(&str, &str)>)>) -> ModelColumnLineage {
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

    #[test]
    fn test_plain_single_model() {
        let report = make_lineage(
            "orders",
            vec![
                ("order_id", TransformationType::Direct, vec![("stg_orders", "order_id")]),
                ("total", TransformationType::Expression, vec![("stg_orders", "price")]),
            ],
        );
        let mut buf = Vec::new();
        render_column_graph_plain_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("orders (2/2 columns traced)"), "header: {}", out);
        assert!(out.contains("order_id"), "column: {}", out);
        assert!(out.contains("stg_orders.order_id"), "source: {}", out);
        assert!(out.contains("direct"), "transformation: {}", out);
        assert!(out.contains("expression"), "transformation: {}", out);
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
        let mut buf = Vec::new();
        render_column_graph_plain_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("(no sources)"), "should show no sources: {}", out);
    }

    #[test]
    fn test_mermaid_single_model() {
        let report = make_lineage(
            "orders",
            vec![("order_id", TransformationType::Direct, vec![("stg_orders", "order_id")])],
        );
        let mut buf = Vec::new();
        render_column_graph_mermaid_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("flowchart LR"), "header: {}", out);
        // Subgraphs now use index-based IDs with quoted labels
        assert!(out.contains(r#"["orders"]"#), "orders subgraph label: {}", out);
        assert!(out.contains(r#"["stg_orders"]"#), "stg_orders subgraph label: {}", out);
        assert!(out.contains("direct"), "edge label: {}", out);
    }

    #[test]
    fn test_mermaid_dotted_table_name() {
        let report = make_lineage(
            "orders",
            vec![("id", TransformationType::Direct, vec![("raw.orders", "id")])],
        );
        let mut buf = Vec::new();
        render_column_graph_mermaid_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        // dotted names should be quoted in subgraph label
        assert!(out.contains(r#""raw.orders""#), "quoted label: {}", out);
    }

    #[test]
    fn test_mermaid_id_collision_avoided() {
        // "raw.orders" and "raw_orders" sanitize to the same string but must
        // produce distinct subgraph IDs.
        let report = make_lineage(
            "raw_orders",
            vec![("id", TransformationType::Direct, vec![("raw.orders", "id")])],
        );
        let mut buf = Vec::new();
        render_column_graph_mermaid_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains(r#"["raw.orders"]"#), "raw.orders label: {}", out);
        assert!(out.contains(r#"["raw_orders"]"#), "raw_orders label: {}", out);
        // The two subgraph IDs must differ (sg0 vs sg1)
        assert!(out.contains("subgraph sg0"), "sg0: {}", out);
        assert!(out.contains("subgraph sg1"), "sg1: {}", out);
    }

    #[test]
    fn test_mermaid_label_escaping() {
        let report = make_lineage(
            "orders",
            vec![("amount<usd>", TransformationType::Direct, vec![("raw.orders", "amount<usd>")])],
        );
        let mut buf = Vec::new();
        render_column_graph_mermaid_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("#lt;"), "less-than escaped: {}", out);
        assert!(out.contains("#gt;"), "greater-than escaped: {}", out);
        assert!(!out.contains("amount<usd>"), "raw angle brackets absent: {}", out);
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
                model_path: vec!["orders".to_string()],
            }],
            errors: vec![],
        };
        let mut buf = Vec::new();
        render_column_impact_plain_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("stg_orders.order_id"), "header: {}", out);
        assert!(out.contains("1 impacted column"), "count: {}", out);
        assert!(out.contains("orders.order_id"), "downstream model: {}", out);
        assert!(!out.contains("via"), "no via for direct: {}", out);
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
                model_path: vec!["orders".to_string(), "customers".to_string()],
            }],
            errors: vec![],
        };
        let mut buf = Vec::new();
        render_column_impact_plain_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("customers.customer_order_id"), "target: {}", out);
        assert!(out.contains("via orders"), "intermediate path: {}", out);
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
                model_path: vec!["orders".to_string()],
            }],
            errors: vec![],
        };
        let mut buf = Vec::new();
        render_column_impact_mermaid_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.starts_with("flowchart LR"), "header: {}", out);
        assert!(out.contains(r#"["stg_orders"]"#), "source subgraph label: {}", out);
        assert!(out.contains(r#"["orders"]"#), "target subgraph label: {}", out);
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
                model_path: vec!["orders".to_string(), "customers".to_string()],
            }],
            errors: vec![],
        };
        let mut buf = Vec::new();
        render_column_impact_mermaid_to_writer(&[report], &mut buf).unwrap();
        let out = String::from_utf8(buf).unwrap();
        assert!(out.contains("via orders"), "indirect edge label: {}", out);
    }
}
