use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};

use crate::graph::column_lineage::{
    ColumnImpactReport, DependencyKind, DependencyRef, ExplorationStatus, Ident,
    ModelColumnLineage, ModelOutputRef, ModelPathHop, OutputShape, OutputSlotId, TraceGap,
    TransformationType, UnexpandedStar, UnresolvedContribution,
};

/// Suffix appended to a node label that has an unresolved part.
const PARTIAL_MARK: &str = "(partial)";
/// Fill of the DOT note nodes that report gaps without a column.
const GAP_NOTE_COLOR: &str = "#F5B7B1";
/// Fill of the DOT nodes that stand for columns that are not listed.
const UNLISTED_COLOR: &str = "#FFFFFF";

// ── Graph model shared by the Mermaid and DOT renderers ──────────────────────

/// The owner of a group of column nodes: a dbt model, which is identified by
/// its unique id and shown by its name, or an external relation shown by its
/// name. Owners are ordered by the name that is shown.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Owner {
    label: String,
    unique_id: Option<String>,
}

impl Owner {
    fn model(unique_id: &str, label: &str) -> Self {
        Self {
            label: label.to_string(),
            unique_id: Some(unique_id.to_string()),
        }
    }

    fn external(label: &str) -> Self {
        Self {
            label: label.to_string(),
            unique_id: None,
        }
    }

    fn display(&self) -> &str {
        if self.label.is_empty() {
            "(literal)"
        } else {
            &self.label
        }
    }
}

/// A column node within its owner. A model output is identified by its slot,
/// so columns with the same name are different nodes. Slots come first, in
/// SQL order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum ColumnKey {
    Slot(OutputSlotId),
    Named(String),
    Note(usize),
}

type NodeKey = (Owner, ColumnKey);

#[derive(Debug, Clone)]
struct NodeInfo {
    label: String,
    transformation: Option<TransformationType>,
    /// A node that notes unlisted columns instead of standing for one.
    unlisted: bool,
}

#[derive(Default)]
struct ColumnGraph {
    owners: BTreeSet<Owner>,
    nodes: BTreeMap<NodeKey, NodeInfo>,
    edges: Vec<(NodeKey, NodeKey, String)>,
    marks: BTreeMap<NodeKey, &'static str>,
    notes: BTreeSet<String>,
}

impl ColumnGraph {
    fn add_node(
        &mut self,
        key: NodeKey,
        label: String,
        transformation: Option<&TransformationType>,
    ) -> NodeKey {
        self.owners.insert(key.0.clone());
        let node = self.nodes.entry(key.clone()).or_insert(NodeInfo {
            label,
            transformation: None,
            unlisted: false,
        });
        if node.transformation.is_none() {
            node.transformation = transformation.cloned();
        }
        key
    }

    fn output_node(
        &mut self,
        output: &ModelOutputRef,
        transformation: Option<&TransformationType>,
    ) -> NodeKey {
        self.add_node(
            (
                Owner::model(&output.unique_id, &output.model),
                ColumnKey::Slot(output.slot),
            ),
            output.column.clone(),
            transformation,
        )
    }

    fn external_node(&mut self, table: &str, column: &Ident) -> NodeKey {
        let column = column.to_string();
        self.add_node(
            (Owner::external(table), ColumnKey::Named(column.clone())),
            column,
            None,
        )
    }

    /// A node for a column that is only known by name, such as the input of an
    /// impact search that did not identify an output.
    fn named_node(&mut self, owner: Owner, column: &str) -> NodeKey {
        self.add_node(
            (owner, ColumnKey::Named(column.to_string())),
            column.to_string(),
            None,
        )
    }

    fn note_unlisted(&mut self, owner: &Owner, shape: &OutputShape) {
        let OutputShape::Open { unexpanded } = shape else {
            return;
        };
        for (index, star) in unexpanded.iter().enumerate() {
            let key = (owner.clone(), ColumnKey::Note(index));
            self.owners.insert(owner.clone());
            self.nodes.insert(
                key,
                NodeInfo {
                    label: unexpanded_note(star),
                    transformation: None,
                    unlisted: true,
                },
            );
        }
    }

    fn edge(&mut self, from: NodeKey, to: NodeKey, label: &str) {
        self.edges.push((from, to, label.to_string()));
    }

    fn label(&self, key: &NodeKey) -> String {
        let node = &self.nodes[key];
        match self.marks.get(key) {
            Some(mark) => format!("{} {mark}", node.label),
            None => node.label.clone(),
        }
    }

    /// Stable node ids: `n{owner}_{column}` by position in the sorted maps, so
    /// owners whose names sanitize to the same text stay distinct.
    fn ids(&self) -> NodeIds {
        let owner_index: BTreeMap<&Owner, usize> = self
            .owners
            .iter()
            .enumerate()
            .map(|(index, owner)| (owner, index))
            .collect();
        let mut column_index = BTreeMap::new();
        let mut counts: BTreeMap<&Owner, usize> = BTreeMap::new();
        for key in self.nodes.keys() {
            let next = counts.entry(&key.0).or_default();
            column_index.insert(key.clone(), *next);
            *next += 1;
        }
        let ids = self
            .nodes
            .keys()
            .map(|key| (key.clone(), (owner_index[&key.0], column_index[key])))
            .collect();
        NodeIds {
            owners: self
                .owners
                .iter()
                .map(|owner| (owner.clone(), owner_index[owner]))
                .collect(),
            nodes: ids,
        }
    }

    fn nodes_of<'a>(
        &'a self,
        owner: &'a Owner,
    ) -> impl Iterator<Item = (&'a NodeKey, &'a NodeInfo)> {
        self.nodes.iter().filter(move |(key, _)| &key.0 == owner)
    }
}

struct NodeIds {
    owners: BTreeMap<Owner, usize>,
    nodes: BTreeMap<NodeKey, (usize, usize)>,
}

fn unexpanded_note(star: &UnexpandedStar) -> String {
    match &star.relation {
        Some(relation) => format!("plus unexpanded columns from {relation}"),
        None => "plus unexpanded columns".to_string(),
    }
}

/// The node that stands for a hop of a model path.
fn hop_node(graph: &mut ColumnGraph, hop: &ModelPathHop) -> NodeKey {
    graph.output_node(&hop.output, Some(&hop.transformation))
}

fn build_upstream_graph(reports: &[ModelColumnLineage]) -> ColumnGraph {
    let mut graph = ColumnGraph::default();
    for report in reports {
        let owner = Owner::model(&report.unique_id, &report.model);
        graph.owners.insert(owner.clone());
        let Some(output) = &report.output else {
            continue;
        };
        for slot in &output.columns {
            let target = graph.add_node(
                (owner.clone(), ColumnKey::Slot(slot.id)),
                slot.name.to_string(),
                Some(&slot.transformation),
            );
            if slot.lineage.is_partial() {
                graph.marks.insert(target.clone(), PARTIAL_MARK);
            }
            let final_label = transformation_label(&slot.transformation);
            for src in slot.lineage.known() {
                let leaf = graph.external_node(&src.table, &src.column);
                // The path is ordered from the target outward, so the chain
                // from the leaf to the target walks it in reverse. Each hop
                // carries its own transformation label.
                let mut chain = vec![(leaf, "")];
                for hop in src.model_path.iter().rev() {
                    let node = hop_node(&mut graph, hop);
                    chain.push((node, transformation_label(&hop.transformation)));
                }
                chain.push((target.clone(), final_label));
                for window in chain.windows(2) {
                    graph.edge(window[0].0.clone(), window[1].0.clone(), window[1].1);
                }
            }
        }
        graph.note_unlisted(&owner, &output.shape);
    }
    graph
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
            "{} ({} column{} listed: {} complete, {} partial)",
            report.model,
            report.listed_columns,
            if report.listed_columns == 1 { "" } else { "s" },
            report.value_complete_columns,
            report.value_partial_columns
        )?;

        let Some(output) = &report.output else {
            continue;
        };

        let names: Vec<String> = output
            .columns
            .iter()
            .map(|slot| slot.name.to_string())
            .collect();
        // Measure column name width for alignment
        let col_width = names
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap_or(0);

        // Prefix is "  " (2) + col padded to col_width + "  →  " (5 display cols)
        let indent = " ".repeat(2 + col_width + 5);
        for (slot, name) in output.columns.iter().zip(&names) {
            let known = slot.lineage.known();
            if known.is_empty() {
                let note = if slot.lineage.is_complete() {
                    "(no sources)"
                } else {
                    "(no known sources)"
                };
                writeln!(w, "  {:width$}  {}", name, note, width = col_width)?;
                write_unresolved_lines(w, &indent, slot.lineage.unresolved())?;
                continue;
            }

            let first = &known[0];
            let src_str = format_source(first.table.as_str(), &first.column, &first.model_path);
            writeln!(
                w,
                "  {:width$}  →  {} ({})",
                name,
                src_str,
                transformation_label(&slot.transformation),
                width = col_width
            )?;
            for src in known.iter().skip(1) {
                let src_str = format_source(src.table.as_str(), &src.column, &src.model_path);
                writeln!(
                    w,
                    "{}  {} ({})",
                    indent,
                    src_str,
                    transformation_label(&slot.transformation),
                )?;
            }
            write_unresolved_lines(w, &indent, slot.lineage.unresolved())?;
        }
        if let OutputShape::Open { unexpanded } = &output.shape {
            for star in unexpanded {
                writeln!(w, "  ({})", unexpanded_note(star))?;
            }
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
    write_mermaid(w, &build_upstream_graph(reports))
}

fn write_mermaid<W: Write>(w: &mut W, graph: &ColumnGraph) -> io::Result<()> {
    writeln!(w, "flowchart LR")?;
    let ids = graph.ids();

    for owner in &graph.owners {
        let midx = ids.owners[owner];
        writeln!(
            w,
            "  subgraph sg{}[\"{}\"]",
            midx,
            super::mermaid_escape(owner.display())
        )?;
        for (key, _) in graph.nodes_of(owner) {
            let (_, cidx) = ids.nodes[key];
            writeln!(
                w,
                "    n{}_{}[\"{}\"]",
                midx,
                cidx,
                super::mermaid_escape(&graph.label(key))
            )?;
        }
        writeln!(w, "  end")?;
    }
    for (i, note) in graph.notes.iter().enumerate() {
        writeln!(w, "  gap{i}[\"{}\"]", super::mermaid_escape(note))?;
    }

    writeln!(w)?;

    // Render edges (deduplicated)
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (from, to, label) in &graph.edges {
        let (from_owner, from_column) = ids.nodes[from];
        let (to_owner, to_column) = ids.nodes[to];
        let edge_str =
            format!("  n{from_owner}_{from_column} -->|\"{label}\"|n{to_owner}_{to_column}");
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
            .map(|c| c.target.column.chars().count())
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
        TraceGap::OpenOutput {
            model, unexpanded, ..
        } => {
            let stars: Vec<String> = unexpanded.iter().map(unexpanded_note).collect();
            format!(
                "{model} has columns that are not listed ({}); they may depend on the input",
                stars.join("; ")
            )
        }
        TraceGap::AmbiguousInput { at, candidates } => {
            let candidates: Vec<String> = candidates
                .iter()
                .map(|candidate| {
                    format!(
                        "{}.{} ({})",
                        candidate.model, candidate.column, candidate.slot
                    )
                })
                .collect();
            format!(
                "{}.{} reads a column name shared by {}",
                at.model,
                at.column,
                candidates.join(", ")
            )
        }
        TraceGap::UnidentifiedInput {
            model,
            column,
            candidates,
            ..
        } => {
            if candidates.is_empty() {
                format!("{model}.{column} is not an enumerated output of {model}")
            } else {
                format!(
                    "{model}.{column} names {} outputs of {model}; none is chosen",
                    candidates.len()
                )
            }
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
fn gap_outputs(report: &ColumnImpactReport) -> Vec<&ModelOutputRef> {
    report
        .gaps()
        .into_iter()
        .filter_map(|(_, gap)| match gap {
            TraceGap::UnresolvedLineage { at, .. } | TraceGap::AmbiguousInput { at, .. } => {
                Some(at)
            }
            TraceGap::AnalysisFailed { .. }
            | TraceGap::OpenOutput { .. }
            | TraceGap::UnidentifiedInput { .. }
            | TraceGap::NotCollected { .. } => None,
        })
        .collect()
}

/// Labels of the gaps that have no column to mark: dependent models that
/// failed analysis or have unlisted columns, inputs that did not identify an
/// output, and dependency kinds that were not collected. They are drawn as
/// standalone note nodes so that an impact search that could not look
/// somewhere never reads as "no impact".
fn gap_notes(reports: &[ColumnImpactReport]) -> BTreeSet<String> {
    let mut notes = BTreeSet::new();
    for report in reports {
        for (_, gap) in report.gaps() {
            match gap {
                TraceGap::AnalysisFailed { model, .. } => {
                    notes.insert(format!("{model}: analysis failed"));
                }
                TraceGap::OpenOutput { model, .. } => {
                    notes.insert(format!("{model}: unlisted columns may depend on the input"));
                }
                TraceGap::UnidentifiedInput { model, column, .. } => {
                    notes.insert(format!("{model}.{column}: input not identified"));
                }
                TraceGap::NotCollected { kind } => {
                    notes.insert(format!(
                        "{} dependencies not collected",
                        dependency_kind_label(*kind)
                    ));
                }
                TraceGap::UnresolvedLineage { .. } | TraceGap::AmbiguousInput { .. } => {}
            }
        }
    }
    notes
}

fn build_impact_graph(reports: &[ColumnImpactReport]) -> ColumnGraph {
    let mut graph = ColumnGraph::default();
    for report in reports {
        // The analysed column is the root of the search. It is a slot when the
        // query identified one, and otherwise only a name.
        match &report.input {
            Some(input) => {
                graph.output_node(input, None);
            }
            None => {
                let owner = report
                    .gaps()
                    .into_iter()
                    .find_map(|(_, gap)| match gap {
                        TraceGap::UnidentifiedInput {
                            unique_id, model, ..
                        } => Some(Owner::model(unique_id, model)),
                        _ => None,
                    })
                    .unwrap_or_else(|| Owner::external(&report.model));
                graph.named_node(owner, &report.column);
            }
        }
        for at in gap_outputs(report) {
            let node = graph.output_node(at, None);
            graph.marks.insert(node, PARTIAL_MARK);
        }
        for ic in &report.confirmed {
            for edge in &ic.path {
                let from = match &edge.from {
                    DependencyRef::Column(from) => graph.output_node(from, None),
                    DependencyRef::UnlistedColumn {
                        unique_id,
                        model,
                        column,
                    } => graph.named_node(Owner::model(unique_id, model), column),
                };
                let to = graph.output_node(&edge.to, Some(&edge.transformation));
                graph.edge(from, to, transformation_label(&edge.transformation));
            }
        }
    }
    graph.notes = gap_notes(reports);
    graph
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
    write_mermaid(w, &build_impact_graph(reports))
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
    write_dot_column_graph(w, &build_upstream_graph(reports))
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
    write_dot_column_graph(w, &build_impact_graph(reports))
}

// ── DOT shared rendering ──────────────────────────────────────────────────────

fn write_dot_column_graph<W: Write>(w: &mut W, graph: &ColumnGraph) -> io::Result<()> {
    writeln!(w, "digraph column_lineage {{")?;
    writeln!(w, r#"  compound=true;"#)?;
    writeln!(
        w,
        r#"  node [shape=box, style=filled, fontname="Helvetica"];"#
    )?;
    writeln!(w)?;

    let ids = graph.ids();

    for owner in &graph.owners {
        let midx = ids.owners[owner];
        writeln!(w, "  subgraph cluster_{midx} {{")?;
        writeln!(w, r#"    label="{}";"#, dot_escape(owner.display()))?;
        writeln!(w, "    style=rounded;")?;
        writeln!(w)?;
        for (key, node) in graph.nodes_of(owner) {
            let (_, cidx) = ids.nodes[key];
            let label = dot_escape(&graph.label(key));
            if node.unlisted {
                writeln!(
                    w,
                    r#"    "n{midx}_{cidx}" [label="{label}", fillcolor="{UNLISTED_COLOR}", style="filled,dashed"];"#,
                )?;
            } else {
                let color = transformation_color(node.transformation.as_ref());
                writeln!(
                    w,
                    r#"    "n{midx}_{cidx}" [label="{label}", fillcolor="{color}"];"#,
                )?;
            }
        }
        writeln!(w, "  }}")?;
    }
    for (i, note) in graph.notes.iter().enumerate() {
        writeln!(
            w,
            r#"  "gap{i}" [label="{}", shape=note, fillcolor="{GAP_NOTE_COLOR}"];"#,
            dot_escape(note)
        )?;
    }

    writeln!(w)?;

    // Deduplicate and sort edges via a structured key; format only when writing.
    let mut unique_edges: BTreeSet<(usize, usize, usize, usize, String)> = BTreeSet::new();
    for (from, to, label) in &graph.edges {
        let (midx_from, cidx_from) = ids.nodes[from];
        let (midx_to, cidx_to) = ids.nodes[to];
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

fn format_source(table: &str, column: &Ident, model_path: &[ModelPathHop]) -> String {
    let table_display = if table.is_empty() { "(literal)" } else { table };
    let mut parts = vec![format!("{table_display}.{column}")];
    for hop in model_path.iter().rev() {
        let output = &hop.output;
        if matches!(hop.transformation, TransformationType::Direct) {
            parts.push(format!("{}.{}", output.model, output.column));
        } else {
            parts.push(format!(
                "{}.{} [{}]",
                output.model,
                output.column,
                transformation_label(&hop.transformation)
            ));
        }
    }
    parts.join(" → ")
}

#[cfg(test)]
#[path = "column_graph_tests.rs"]
mod tests;
