use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use crate::graph::column_lineage::{
    ColumnImpactReport, DependencyKind, DependencyRef, ExplorationStatus, ModelColumnLineage,
    TraceGap, TransformationType, UnresolvedContribution,
};

/// Suffix appended to a node label that has an unresolved part.
const PARTIAL_MARK: &str = "(partial)";
const UNRESOLVED_MARK: &str = "(unresolved)";
/// Fill of the DOT note nodes that report gaps without a column.
const GAP_NOTE_COLOR: &str = "#F5B7B1";

/// Nodes (model, column) that carry a visible marker, with the marker text.
type NodeMarks = BTreeMap<(String, String), &'static str>;

fn marked_label(marks: &NodeMarks, model: &str, column: &str) -> String {
    match marks.get(&(model.to_string(), column.to_string())) {
        Some(mark) => format!("{column} {mark}"),
        None => column.to_string(),
    }
}

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
            let known = entry.lineage.known();
            if known.is_empty() {
                let note = if entry.lineage.is_complete() {
                    "(no sources)"
                } else {
                    "(no known sources)"
                };
                writeln!(w, "  {:width$}  {}", entry.column, note, width = col_width)?;
                write_unresolved_lines(w, &indent, entry.lineage.unresolved())?;
                continue;
            }

            let first = &known[0];
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
            for src in known.iter().skip(1) {
                let src_str =
                    format_source(src.table.as_str(), src.column.as_str(), &src.model_path);
                writeln!(
                    w,
                    "{}  {} ({})",
                    indent,
                    src_str,
                    transformation_label(&entry.transformation),
                )?;
            }
            write_unresolved_lines(w, &indent, entry.lineage.unresolved())?;
        }
    }
    Ok(())
}

fn write_unresolved_lines<W: Write>(
    w: &mut W,
    indent: &str,
    unresolved: &[UnresolvedContribution],
) -> io::Result<()> {
    for contribution in unresolved {
        let location = if contribution.model_path.is_empty() {
            String::new()
        } else {
            format!(
                " (in {}.{})",
                contribution.origin.model, contribution.origin.column
            )
        };
        writeln!(
            w,
            "{}  unresolved: {}{}",
            indent, contribution.reason, location
        )?;
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
    let mut marks = NodeMarks::new();

    for report in reports {
        let target_model = &report.model;
        model_columns.entry(target_model.clone()).or_default();

        for entry in &report.columns {
            model_columns
                .entry(target_model.clone())
                .or_default()
                .insert(entry.column.clone());
            if entry.lineage.is_partial() {
                marks.insert((target_model.clone(), entry.column.clone()), PARTIAL_MARK);
            }

            for src in entry.lineage.known() {
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
                        model_columns
                            .entry(m.clone())
                            .or_default()
                            .insert(c.clone());
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
        let display = if model.is_empty() {
            "(literal)"
        } else {
            model.as_str()
        };
        writeln!(
            w,
            "  subgraph sg{}[\"{}\"]",
            midx,
            super::mermaid_escape(display)
        )?;
        for (cidx, col) in columns.iter().enumerate() {
            writeln!(
                w,
                "    n{}_{}[\"{}\"]",
                midx,
                cidx,
                super::mermaid_escape(&marked_label(&marks, model, col))
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
        let count = report.confirmed.len();
        let complete = report
            .exploration
            .iter()
            .all(|e| e.status == ExplorationStatus::Complete);
        let plural = if count == 1 { "" } else { "s" };
        let summary = match (count, complete) {
            (0, true) => "no impact".to_string(),
            (0, false) => "no confirmed impact; exploration incomplete".to_string(),
            (n, true) => format!("{n} impacted column{plural}"),
            (n, false) => format!("{n} impacted column{plural}; exploration incomplete"),
        };
        writeln!(w, "{}.{} ({})", report.model, report.column, summary)?;

        let col_width = report
            .confirmed
            .iter()
            .map(|c| c.target.column.len())
            .max()
            .unwrap_or(0);

        for ic in &report.confirmed {
            // path is ordered source → target; intermediate hops precede the last edge.
            let via_str = if ic.path.len() > 1 {
                let intermediate = &ic.path[..ic.path.len() - 1];
                let parts: Vec<String> = intermediate
                    .iter()
                    .map(|edge| {
                        if matches!(edge.transformation, TransformationType::Direct) {
                            format!("{}.{}", edge.to.model, edge.to.column)
                        } else {
                            format!(
                                "{}.{} [{}]",
                                edge.to.model,
                                edge.to.column,
                                transformation_label(&edge.transformation)
                            )
                        }
                    })
                    .collect();
                format!(", via {}", parts.join(" → "))
            } else {
                String::new()
            };
            writeln!(
                w,
                "  {:width$}  →  {} ({}{})",
                ic.target.column,
                ic.target.model,
                transformation_label(&ic.transformation),
                via_str,
                width = col_width
            )?;
        }
        write_exploration_gaps(w, report)?;
    }
    Ok(())
}

/// Print why an exploration is incomplete. Nothing is printed when every
/// requested exploration is complete.
fn write_exploration_gaps<W: Write>(w: &mut W, report: &ColumnImpactReport) -> io::Result<()> {
    for exploration in &report.exploration {
        let ExplorationStatus::Incomplete { gaps } = &exploration.status else {
            continue;
        };
        writeln!(
            w,
            "  {} exploration incomplete ({} gap{}):",
            dependency_kind_label(exploration.kind),
            gaps.len(),
            if gaps.len() == 1 { "" } else { "s" }
        )?;
        for gap in gaps {
            writeln!(w, "    - {}", describe_gap(gap))?;
        }
    }
    Ok(())
}

fn dependency_kind_label(kind: DependencyKind) -> &'static str {
    match kind {
        DependencyKind::Value => "value",
        DependencyKind::Row => "row",
    }
}

fn describe_gap(gap: &TraceGap) -> String {
    match gap {
        TraceGap::UnresolvedLineage { at, reasons } => {
            let reasons: Vec<String> = reasons.iter().map(ToString::to_string).collect();
            format!(
                "unresolved lineage at {}.{}: {}",
                at.model,
                at.column,
                reasons.join("; ")
            )
        }
        TraceGap::AnalysisFailed { model, error, .. } => {
            format!("analysis of {model} failed ({error:?})")
        }
        TraceGap::UnresolvedColumn { at } => {
            format!(
                "lineage of {}.{} could not be established",
                at.model, at.column
            )
        }
        TraceGap::NotCollected { kind } => {
            format!(
                "{} dependencies are not collected",
                dependency_kind_label(*kind)
            )
        }
    }
}

/// Markers for the nodes where an impact search found a gap.
fn gap_marks(report: &ColumnImpactReport) -> NodeMarks {
    let mut marks = NodeMarks::new();
    for (_, gap) in report.gaps() {
        match gap {
            TraceGap::UnresolvedLineage { at, .. } => {
                marks.insert((at.model.clone(), at.column.clone()), PARTIAL_MARK);
            }
            TraceGap::UnresolvedColumn { at } => {
                marks.insert((at.model.clone(), at.column.clone()), UNRESOLVED_MARK);
            }
            TraceGap::AnalysisFailed { .. } | TraceGap::NotCollected { .. } => {}
        }
    }
    marks
}

/// Labels of the gaps that have no column to mark: dependent models that
/// failed analysis and dependency kinds that were not collected. They are
/// drawn as standalone note nodes so that an impact search that could not look
/// somewhere never reads as "no impact".
fn gap_notes(reports: &[ColumnImpactReport]) -> BTreeSet<String> {
    let mut notes = BTreeSet::new();
    for report in reports {
        for (_, gap) in report.gaps() {
            match gap {
                TraceGap::AnalysisFailed { model, .. } => {
                    notes.insert(format!("{model}: analysis failed"));
                }
                TraceGap::NotCollected { kind } => {
                    notes.insert(format!(
                        "{} dependencies not collected",
                        dependency_kind_label(*kind)
                    ));
                }
                TraceGap::UnresolvedLineage { .. } | TraceGap::UnresolvedColumn { .. } => {}
            }
        }
    }
    notes
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
    let mut marks = NodeMarks::new();

    for report in reports {
        // Source node (the column being analyzed for impact)
        model_columns
            .entry(report.model.clone())
            .or_default()
            .insert(report.column.clone());

        marks.extend(gap_marks(report));
        for (model, column) in marks.keys() {
            model_columns
                .entry(model.clone())
                .or_default()
                .insert(column.clone());
        }
        for ic in &report.confirmed {
            for edge in &ic.path {
                let DependencyRef::Column {
                    model: from_model,
                    column: from_column,
                    ..
                } = &edge.from;
                for (m, c) in [(from_model, from_column), (&edge.to.model, &edge.to.column)] {
                    model_columns
                        .entry(m.clone())
                        .or_default()
                        .insert(c.clone());
                }
                edges.push((
                    from_model.clone(),
                    from_column.clone(),
                    edge.to.model.clone(),
                    edge.to.column.clone(),
                    transformation_label(&edge.transformation).to_string(),
                ));
            }
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
        let display = if model.is_empty() {
            "(literal)"
        } else {
            model.as_str()
        };
        writeln!(
            w,
            "  subgraph sg{}[\"{}\"]",
            midx,
            super::mermaid_escape(display)
        )?;
        for (cidx, col) in columns.iter().enumerate() {
            writeln!(
                w,
                "    n{}_{}[\"{}\"]",
                midx,
                cidx,
                super::mermaid_escape(&marked_label(&marks, model, col))
            )?;
        }
        writeln!(w, "  end")?;
    }
    for (i, note) in gap_notes(reports).iter().enumerate() {
        writeln!(w, "  gap{i}[\"{}\"]", super::mermaid_escape(note))?;
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
    let mut marks = NodeMarks::new();

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
            if entry.lineage.is_partial() {
                marks.insert((target_model.clone(), entry.column.clone()), PARTIAL_MARK);
            }

            for src in entry.lineage.known() {
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
                        column_transformation
                            .entry(m.clone())
                            .or_default()
                            .entry(c.clone())
                            .or_insert_with(|| t.clone());
                    }
                    chain.push((target_model.clone(), entry.column.clone(), final_label));

                    for (m, c, _) in &chain {
                        model_columns
                            .entry(m.clone())
                            .or_default()
                            .insert(c.clone());
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

    write_dot_column_graph(
        w,
        &model_columns,
        &column_transformation,
        &marks,
        &BTreeSet::new(),
        &edges,
    )
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
    let mut marks = NodeMarks::new();

    for report in reports {
        model_columns
            .entry(report.model.clone())
            .or_default()
            .insert(report.column.clone());

        marks.extend(gap_marks(report));
        for (model, column) in marks.keys() {
            model_columns
                .entry(model.clone())
                .or_default()
                .insert(column.clone());
        }
        for ic in &report.confirmed {
            for edge in &ic.path {
                let DependencyRef::Column {
                    model: from_model,
                    column: from_column,
                    ..
                } = &edge.from;
                for (m, c) in [(from_model, from_column), (&edge.to.model, &edge.to.column)] {
                    model_columns
                        .entry(m.clone())
                        .or_default()
                        .insert(c.clone());
                }
                column_transformation
                    .entry(edge.to.model.clone())
                    .or_default()
                    .entry(edge.to.column.clone())
                    .or_insert_with(|| edge.transformation.clone());
                edges.push((
                    from_model.clone(),
                    from_column.clone(),
                    edge.to.model.clone(),
                    edge.to.column.clone(),
                    transformation_label(&edge.transformation).to_string(),
                ));
            }
        }
    }

    write_dot_column_graph(
        w,
        &model_columns,
        &column_transformation,
        &marks,
        &gap_notes(reports),
        &edges,
    )
}

// ── DOT shared rendering ──────────────────────────────────────────────────────

fn write_dot_column_graph<W: Write>(
    w: &mut W,
    model_columns: &BTreeMap<String, BTreeSet<String>>,
    column_transformation: &BTreeMap<String, BTreeMap<String, TransformationType>>,
    marks: &NodeMarks,
    notes: &BTreeSet<String>,
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
        let display = if model.is_empty() {
            "(literal)"
        } else {
            model.as_str()
        };
        writeln!(w, "  subgraph cluster_{midx} {{")?;
        writeln!(w, r#"    label="{}";"#, dot_escape(display))?;
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
                dot_escape(&marked_label(marks, model, col))
            )?;
        }
        writeln!(w, "  }}")?;
    }
    for (i, note) in notes.iter().enumerate() {
        writeln!(
            w,
            r#"  "gap{i}" [label="{}", shape=note, fillcolor="{GAP_NOTE_COLOR}"];"#,
            dot_escape(note)
        )?;
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

use super::dot_escape;

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

fn format_source(
    table: &str,
    column: &str,
    model_path: &[(String, String, TransformationType)],
) -> String {
    let table_display = if table.is_empty() { "(literal)" } else { table };
    if model_path.is_empty() {
        format!("{}.{}", table_display, column)
    } else {
        let mut parts = vec![format!("{}.{}", table_display, column)];
        for (m, c, t) in model_path.iter().rev() {
            if matches!(t, TransformationType::Direct) {
                parts.push(format!("{}.{}", m, c));
            } else {
                parts.push(format!("{}.{} [{}]", m, c, transformation_label(t)));
            }
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
#[path = "column_graph_tests.rs"]
mod tests;
