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

        // Prefix is "  " (2) + col padded to col_width + "  →  " (5 display cols)
        let indent = " ".repeat(2 + col_width + 5);
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
            let src_str = format_source(
                first.table.as_str(),
                first.column.as_str(),
                &first.model_path,
            );
            writeln!(
                w,
                "  {:width$}  →  {} ({})",
                entry.column,
                src_str,
                transformation_label(&entry.transformation),
                width = col_width
            )?;
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
    // Collect edges: (src_model, src_col, dst_model, dst_col, label)
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
                let final_label = transformation_label(&entry.transformation).to_string();
                if src.model_path.is_empty() {
                    model_columns
                        .entry(src.table.clone())
                        .or_default()
                        .insert(src.column.clone());
                    edges.push((
                        src.table.clone(),
                        src.column.clone(),
                        target_model.clone(),
                        entry.column.clone(),
                        final_label,
                    ));
                } else {
                    // Build edge chain: leaf → path[-1] → ... → path[0] → target
                    // model_path is ordered from target outward, so reverse it for the chain.
                    // Each hop carries its own transformation label; the final edge uses
                    // entry.transformation.
                    // chain: (model, col, edge_label_for_edge_arriving_at_this_node)
                    let mut chain: Vec<(String, String, String)> = Vec::new();
                    chain.push((src.table.clone(), src.column.clone(), String::new()));
                    for (m, c, t) in src.model_path.iter().rev() {
                        chain.push((m.clone(), c.clone(), transformation_label(t).to_string()));
                    }
                    chain.push((target_model.clone(), entry.column.clone(), final_label));

                    for (m, c, _) in &chain {
                        model_columns.entry(m.clone()).or_default().insert(c.clone());
                    }
                    for window in chain.windows(2) {
                        let (from_m, from_c, _) = &window[0];
                        let (to_m, to_c, label) = &window[1];
                        edges.push((
                            from_m.clone(),
                            from_c.clone(),
                            to_m.clone(),
                            to_c.clone(),
                            label.clone(),
                        ));
                    }
                }
            }
        }
    }

    // Build stable index maps to avoid ID collisions between models whose names
    // differ only in characters that sanitize_id maps to '_' (e.g. "raw.orders"
    // vs "raw_orders" both become "raw_orders").
    let model_index: BTreeMap<&str, usize> = model_columns
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    // Pre-compute column → index per model for O(1) node ID lookup.
    let column_index: BTreeMap<&str, BTreeMap<&str, usize>> = model_columns
        .iter()
        .map(|(model, cols)| {
            let col_map = cols
                .iter()
                .enumerate()
                .map(|(i, c)| (c.as_str(), i))
                .collect();
            (model.as_str(), col_map)
        })
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
        let from_node = indexed_node_id(&model_index, &column_index, from_model, from_col);
        let to_node = indexed_node_id(&model_index, &column_index, to_model, to_col);
        let edge_str = format!("  {} -->|\"{}\"|{}", from_node, label, to_node);
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
            if report.impacted_columns.len() == 1 {
                ""
            } else {
                "s"
            },
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
                "  {:width$}  →  {} ({}{})",
                ic.column,
                ic.model,
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
                let escaped: Vec<String> = intermediate
                    .iter()
                    .map(|m| super::mermaid_escape(m))
                    .collect();
                format!(" (via {})", escaped.join(" \u{2192} "))
            } else {
                String::new()
            };
            let label = format!("{}{}", transformation_label(&ic.transformation), via_str);

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

    // Build stable index maps to avoid ID collisions (same reason as column graph).
    let model_index: BTreeMap<&str, usize> = model_columns
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let column_index: BTreeMap<&str, BTreeMap<&str, usize>> = model_columns
        .iter()
        .map(|(model, cols)| {
            let col_map = cols
                .iter()
                .enumerate()
                .map(|(i, c)| (c.as_str(), i))
                .collect();
            (model.as_str(), col_map)
        })
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
        let from_node = indexed_node_id(&model_index, &column_index, from_model, from_col);
        let to_node = indexed_node_id(&model_index, &column_index, to_model, to_col);
        let edge_str = format!("  {} -->|\"{}\"|{}", from_node, label, to_node);
        if seen.insert(edge_str.clone()) {
            writeln!(w, "{}", edge_str)?;
        }
    }

    Ok(())
}

// ── Column graph (upstream) — DOT ─────────────────────────────────────────────

pub fn render_column_graph_dot(reports: &[ModelColumnLineage]) {
    super::handle_stdout_result(render_column_graph_dot_to_writer(
        reports,
        &mut std::io::stdout().lock(),
    ));
}

pub fn render_column_graph_dot_to_writer<W: Write>(
    reports: &[ModelColumnLineage],
    w: &mut W,
) -> io::Result<()> {
    let mut model_columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut column_transformation: BTreeMap<String, BTreeMap<String, TransformationType>> =
        BTreeMap::new();
    let mut edges: Vec<(String, String, String, String, String)> = Vec::new();

    for report in reports {
        let target_model = &report.model;
        model_columns.entry(target_model.clone()).or_default();

        for entry in &report.columns {
            model_columns
                .entry(target_model.clone())
                .or_default()
                .insert(entry.column.clone());
            column_transformation
                .entry(target_model.clone())
                .or_default()
                .entry(entry.column.clone())
                .or_insert_with(|| entry.transformation.clone());

            for src in &entry.sources {
                let final_label = transformation_label(&entry.transformation).to_string();
                if src.model_path.is_empty() {
                    model_columns
                        .entry(src.table.clone())
                        .or_default()
                        .insert(src.column.clone());
                    edges.push((
                        src.table.clone(),
                        src.column.clone(),
                        target_model.clone(),
                        entry.column.clone(),
                        final_label,
                    ));
                } else {
                    let mut chain: Vec<(String, String, String)> = Vec::new();
                    chain.push((src.table.clone(), src.column.clone(), String::new()));
                    for (m, c, t) in src.model_path.iter().rev() {
                        chain.push((m.clone(), c.clone(), transformation_label(t).to_string()));
                    }
                    chain.push((target_model.clone(), entry.column.clone(), final_label));

                    for (m, c, _) in &chain {
                        model_columns.entry(m.clone()).or_default().insert(c.clone());
                    }
                    for window in chain.windows(2) {
                        let (from_m, from_c, _) = &window[0];
                        let (to_m, to_c, label) = &window[1];
                        edges.push((
                            from_m.clone(),
                            from_c.clone(),
                            to_m.clone(),
                            to_c.clone(),
                            label.clone(),
                        ));
                    }
                }
            }
        }
    }

    write_dot_column_graph(w, &model_columns, &column_transformation, &edges)
}

// ── Column impact (downstream) — DOT ─────────────────────────────────────────

pub fn render_column_impact_dot(reports: &[ColumnImpactReport]) {
    super::handle_stdout_result(render_column_impact_dot_to_writer(
        reports,
        &mut std::io::stdout().lock(),
    ));
}

pub fn render_column_impact_dot_to_writer<W: Write>(
    reports: &[ColumnImpactReport],
    w: &mut W,
) -> io::Result<()> {
    let mut model_columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut column_transformation: BTreeMap<String, BTreeMap<String, TransformationType>> =
        BTreeMap::new();
    let mut edges: Vec<(String, String, String, String, String)> = Vec::new();

    for report in reports {
        model_columns
            .entry(report.model.clone())
            .or_default()
            .insert(report.column.clone());

        for ic in &report.impacted_columns {
            model_columns
                .entry(ic.model.clone())
                .or_default()
                .insert(ic.column.clone());
            column_transformation
                .entry(ic.model.clone())
                .or_default()
                .entry(ic.column.clone())
                .or_insert_with(|| ic.transformation.clone());

            let via_str = if ic.model_path.len() > 1 {
                let intermediate = &ic.model_path[..ic.model_path.len() - 1];
                format!(" (via {})", intermediate.join(" -> "))
            } else {
                String::new()
            };
            let label = format!("{}{}", transformation_label(&ic.transformation), via_str);
            edges.push((
                report.model.clone(),
                report.column.clone(),
                ic.model.clone(),
                ic.column.clone(),
                label,
            ));
        }
    }

    write_dot_column_graph(w, &model_columns, &column_transformation, &edges)
}

// ── DOT shared rendering ──────────────────────────────────────────────────────

fn write_dot_column_graph<W: Write>(
    w: &mut W,
    model_columns: &BTreeMap<String, BTreeSet<String>>,
    column_transformation: &BTreeMap<String, BTreeMap<String, TransformationType>>,
    edges: &[(String, String, String, String, String)],
) -> io::Result<()> {
    writeln!(w, "digraph column_lineage {{")?;
    writeln!(w, r#"  compound=true;"#)?;
    writeln!(
        w,
        r#"  node [shape=box, style=filled, fontname="Helvetica"];"#
    )?;
    writeln!(w)?;

    // Build stable index maps (same approach as mermaid to avoid ID collisions).
    let model_index: BTreeMap<&str, usize> = model_columns
        .keys()
        .enumerate()
        .map(|(i, k)| (k.as_str(), i))
        .collect();
    let column_index: BTreeMap<&str, BTreeMap<&str, usize>> = model_columns
        .iter()
        .map(|(model, cols)| {
            let col_map = cols
                .iter()
                .enumerate()
                .map(|(i, c)| (c.as_str(), i))
                .collect();
            (model.as_str(), col_map)
        })
        .collect();

    for (model, columns) in model_columns {
        let midx = model_index[model.as_str()];
        writeln!(w, "  subgraph cluster_{midx} {{")?;
        writeln!(w, r#"    label="{}";"#, dot_escape(model))?;
        writeln!(w, "    style=rounded;")?;
        writeln!(w)?;
        for (cidx, col) in columns.iter().enumerate() {
            let trans = column_transformation
                .get(model.as_str())
                .and_then(|m| m.get(col.as_str()));
            let color = transformation_color(trans);
            writeln!(
                w,
                r#"    "n{midx}_{cidx}" [label="{}", fillcolor="{color}"];"#,
                dot_escape(col)
            )?;
        }
        writeln!(w, "  }}")?;
    }

    writeln!(w)?;

    // Deduplicate and sort edges via a structured key; format only when writing.
    let mut unique_edges: BTreeSet<(usize, usize, usize, usize, String)> = BTreeSet::new();
    for (from_model, from_col, to_model, to_col, label) in edges {
        let midx_from = model_index[from_model.as_str()];
        let cidx_from = column_index[from_model.as_str()][from_col.as_str()];
        let midx_to = model_index[to_model.as_str()];
        let cidx_to = column_index[to_model.as_str()][to_col.as_str()];
        unique_edges.insert((midx_from, cidx_from, midx_to, cidx_to, dot_escape(label)));
    }
    for (midx_from, cidx_from, midx_to, cidx_to, label) in &unique_edges {
        writeln!(
            w,
            r#"  "n{midx_from}_{cidx_from}" -> "n{midx_to}_{cidx_to}" [label="{label}"];"#
        )?;
    }

    writeln!(w, "}}")?;
    Ok(())
}

fn transformation_color(t: Option<&TransformationType>) -> &'static str {
    match t {
        Some(TransformationType::Direct) => "#AED6F1",
        Some(TransformationType::Aggregation) => "#FAD7A0",
        Some(TransformationType::Expression) => "#A9DFBF",
        Some(TransformationType::Cast) => "#D7BDE2",
        Some(TransformationType::Conditional) => "#F9E79F",
        Some(TransformationType::Unknown) | None => "#D5D8DC",
    }
}

/// Escape characters that are special inside DOT double-quoted strings.
///
/// DOT requires `\` and `"` to be backslash-escaped inside double-quoted
/// strings. Without this, model or column names containing these characters
/// would produce syntactically invalid DOT output.
fn dot_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out
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

fn format_source(table: &str, column: &str, model_path: &[(String, String, TransformationType)]) -> String {
    if model_path.is_empty() {
        format!("{}.{}", table, column)
    } else {
        let mut parts = vec![format!("{}.{}", table, column)];
        for (m, c, _) in model_path.iter().rev() {
            parts.push(format!("{}.{}", m, c));
        }
        parts.join(" → ")
    }
}

/// Return the Mermaid node ID for `(model, col)` using pre-built index maps.
fn indexed_node_id(
    model_index: &BTreeMap<&str, usize>,
    column_index: &BTreeMap<&str, BTreeMap<&str, usize>>,
    model: &str,
    col: &str,
) -> String {
    let midx = model_index
        .get(model)
        .copied()
        .expect("model must be registered before calling indexed_node_id");
    let cidx = column_index
        .get(model)
        .and_then(|m| m.get(col).copied())
        .expect("column must be registered before calling indexed_node_id");
    format!("n{}_{}", midx, cidx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::column_lineage::{
        ColumnLineageEntry, ColumnSource, ImpactedColumn, ModelColumnLineage,
    };

    fn make_lineage(
        model: &str,
        entries: Vec<(&str, TransformationType, Vec<(&str, &str)>)>,
    ) -> ModelColumnLineage {
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
                model_path: vec!["orders".to_string(), "customers".to_string()],
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
                model_path: vec!["orders".to_string()],
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
                model_path: vec!["orders".to_string(), "customers".to_string()],
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
                    model_path: vec![("stg_orders".to_string(), "order_id".to_string(), TransformationType::Direct)],
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
                model_path: vec!["orders".to_string()],
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
                model_path: vec!["orders".to_string(), "customers".to_string()],
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
}
