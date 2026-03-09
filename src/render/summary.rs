use std::io::{self, IsTerminal, Write};

use serde::Serialize;

use crate::graph::filter::KNOWN_NODE_TYPE_LABELS;
use crate::graph::types::*;

/// Summary report data, suitable for both text and JSON rendering.
#[derive(Debug, Serialize)]
pub struct SummaryReport {
    pub project_name: String,
    pub source_mode: String,
    pub node_counts: NodeCounts,
    pub edge_count: usize,
    pub vars_count: usize,
    pub manifest_status: Option<ManifestStatus>,
}

#[derive(Debug, Serialize)]
pub struct NodeCounts {
    pub model: usize,
    pub source: usize,
    pub seed: usize,
    pub snapshot: usize,
    pub test: usize,
    pub exposure: usize,
    pub phantom: usize,
    pub total: usize,
}

/// Maximum number of file names to show in text output.
const MAX_FILES_TEXT: usize = 5;

#[derive(Debug, Serialize)]
pub struct ManifestStatus {
    pub found: bool,
    pub is_stale: bool,
    pub stale_file_count: usize,
    pub stale_files: Vec<String>,
    pub deleted_file_count: usize,
    pub deleted_files: Vec<String>,
}

/// Count nodes by type from a lineage graph.
pub fn count_nodes(graph: &LineageGraph) -> NodeCounts {
    let mut model = 0;
    let mut source = 0;
    let mut seed = 0;
    let mut snapshot = 0;
    let mut test = 0;
    let mut exposure = 0;
    let mut phantom = 0;

    for idx in graph.node_indices() {
        match graph[idx].node_type {
            NodeType::Model => model += 1,
            NodeType::Source => source += 1,
            NodeType::Seed => seed += 1,
            NodeType::Snapshot => snapshot += 1,
            NodeType::Test => test += 1,
            NodeType::Exposure => exposure += 1,
            NodeType::Phantom => phantom += 1,
        }
    }

    let total = model + source + seed + snapshot + test + exposure + phantom;
    NodeCounts { model, source, seed, snapshot, test, exposure, phantom, total }
}

/// Render summary as human-readable text to stdout.
pub fn render_summary_text_stdout(report: &SummaryReport) {
    let mut stdout = io::stdout().lock();
    super::handle_stdout_result(render_summary_text(report, &mut stdout));
}

/// Render summary as JSON to stdout.
pub fn render_summary_json_stdout(report: &SummaryReport) {
    let mut stdout = io::stdout().lock();
    let pretty = stdout.is_terminal();
    super::handle_stdout_result(render_summary_json(report, &mut stdout, pretty));
}

fn render_file_list<W: Write>(w: &mut W, label: &str, files: &[String], max: usize) -> io::Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let show = files.len().min(max);
    //           "Manifest:    " is 13 chars; align sub-labels to same column
    writeln!(w, "  {}:", label)?;
    for f in &files[..show] {
        writeln!(w, "  - {}", f)?;
    }
    let remaining = files.len() - show;
    if remaining > 0 {
        writeln!(w, "  ... and {} more", remaining)?;
    }
    Ok(())
}

pub fn render_summary_text<W: Write>(report: &SummaryReport, w: &mut W) -> io::Result<()> {
    writeln!(w, "Project: {}", report.project_name)?;
    writeln!(w, "Source:  {}", report.source_mode)?;
    writeln!(w)?;

    writeln!(w, "Nodes:")?;
    for &type_label in KNOWN_NODE_TYPE_LABELS {
        let count = match type_label {
            "model" => report.node_counts.model,
            "source" => report.node_counts.source,
            "seed" => report.node_counts.seed,
            "snapshot" => report.node_counts.snapshot,
            "test" => report.node_counts.test,
            "exposure" => report.node_counts.exposure,
            _ => 0,
        };
        if count > 0 {
            writeln!(w, "  {:<12} {}", type_label, count)?;
        }
    }
    if report.node_counts.phantom > 0 {
        writeln!(w, "  {:<12} {}", "phantom", report.node_counts.phantom)?;
    }
    writeln!(w, "  {:<12} {}", "total", report.node_counts.total)?;

    writeln!(w, "Edges:         {}", report.edge_count)?;

    if report.vars_count > 0 {
        writeln!(w, "Vars:          {}", report.vars_count)?;
    }

    if let Some(ref ms) = report.manifest_status {
        writeln!(w)?;
        if !ms.found {
            writeln!(w, "Manifest:      not found")?;
        } else if ms.is_stale {
            let mut parts = Vec::new();
            if ms.stale_file_count > 0 {
                parts.push(format!("{} file{} newer",
                    ms.stale_file_count,
                    if ms.stale_file_count == 1 { "" } else { "s" }
                ));
            }
            if ms.deleted_file_count > 0 {
                parts.push(format!("{} deleted",
                    ms.deleted_file_count,
                ));
            }
            writeln!(w, "Manifest:      stale ({})", parts.join(", "))?;
            render_file_list(w, "newer", &ms.stale_files, MAX_FILES_TEXT)?;
            render_file_list(w, "deleted", &ms.deleted_files, MAX_FILES_TEXT)?;
        } else {
            writeln!(w, "Manifest:      up-to-date")?;
        }
    }

    Ok(())
}

pub fn render_summary_json<W: Write>(report: &SummaryReport, w: &mut W, pretty: bool) -> io::Result<()> {
    if pretty {
        serde_json::to_writer_pretty(&mut *w, report).map_err(super::serde_io_error)?;
    } else {
        serde_json::to_writer(&mut *w, report).map_err(super::serde_io_error)?;
    }
    writeln!(w)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_report() -> SummaryReport {
        SummaryReport {
            project_name: "my_project".to_string(),
            source_mode: "sql".to_string(),
            node_counts: NodeCounts {
                model: 5,
                source: 2,
                seed: 1,
                snapshot: 0,
                test: 3,
                exposure: 1,
                phantom: 0,
                total: 12,
            },
            edge_count: 10,
            vars_count: 2,
            manifest_status: None,
        }
    }

    #[test]
    fn test_count_nodes() {
        let graph = crate::render::test_helpers::make_sample_lineage_graph();
        let counts = count_nodes(&graph);
        assert_eq!(counts.model, 2);
        assert_eq!(counts.source, 1);
        assert_eq!(counts.test, 1);
        assert_eq!(counts.exposure, 1);
        assert_eq!(counts.seed, 0);
        assert_eq!(counts.snapshot, 0);
        assert_eq!(counts.phantom, 0);
        assert_eq!(counts.total, 5);
    }

    #[test]
    fn test_text_output() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Project: my_project"));
        assert!(output.contains("Source:  sql"));
        assert!(output.contains("model"));
        assert!(output.contains("5"));
        assert!(output.contains("total"));
        assert!(output.contains("12"));
        assert!(output.contains("Edges:"));
        assert!(output.contains("Vars:"));
    }

    #[test]
    fn test_text_hides_zero_counts() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // snapshot count is 0, should not appear
        assert!(!output.contains("snapshot"));
    }

    #[test]
    fn test_text_with_manifest_stale() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 3,
            stale_files: vec![
                "models/marts/orders.sql".to_string(),
                "models/staging/stg_orders.sql".to_string(),
                "models/staging/stg_payments.sql".to_string(),
            ],
            deleted_file_count: 0,
            deleted_files: vec![],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Manifest:      stale (3 files newer)"));
        assert!(output.contains("models/marts/orders.sql"));
    }

    #[test]
    fn test_text_with_manifest_up_to_date() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: false,
            stale_file_count: 0,
            stale_files: vec![],
            deleted_file_count: 0,
            deleted_files: vec![],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Manifest:      up-to-date"));
    }

    #[test]
    fn test_text_with_manifest_not_found() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: false,
            is_stale: false,
            stale_file_count: 0,
            stale_files: vec![],
            deleted_file_count: 0,
            deleted_files: vec![],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Manifest:      not found"));
    }

    #[test]
    fn test_json_output() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_json(&report, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["project_name"], "my_project");
        assert_eq!(parsed["source_mode"], "sql");
        assert_eq!(parsed["node_counts"]["model"], 5);
        assert_eq!(parsed["node_counts"]["total"], 12);
        assert_eq!(parsed["edge_count"], 10);
        assert_eq!(parsed["vars_count"], 2);
        assert!(parsed["manifest_status"].is_null());
    }

    #[test]
    fn test_json_with_manifest() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 5,
            stale_files: vec![
                "a.sql".to_string(), "b.sql".to_string(), "c.sql".to_string(),
                "d.sql".to_string(), "e.sql".to_string(),
            ],
            deleted_file_count: 0,
            deleted_files: vec![],
        });
        let mut buf = Vec::new();
        render_summary_json(&report, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["manifest_status"]["found"], true);
        assert_eq!(parsed["manifest_status"]["is_stale"], true);
        assert_eq!(parsed["manifest_status"]["stale_file_count"], 5);
    }

    #[test]
    fn test_json_compact_single_line() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_json(&report, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "compact JSON should be a single line");
    }

    #[test]
    fn test_json_pretty_multi_line() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_json(&report, &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert!(lines.len() > 1, "pretty JSON should be multi-line");
    }

    #[test]
    fn test_text_no_vars_when_zero() {
        let mut report = make_report();
        report.vars_count = 0;
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.contains("Vars:"));
    }

    #[test]
    fn test_snapshot_summary_text() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_summary_json() {
        let report = make_report();
        let mut buf = Vec::new();
        render_summary_json(&report, &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_text_phantom_shown_when_nonzero() {
        let mut report = make_report();
        report.node_counts.phantom = 2;
        report.node_counts.total = 14;
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("phantom"));
        assert!(output.contains("2"));
    }

    #[test]
    fn test_manifest_stale_singular() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 1,
            stale_files: vec!["models/staging/stg_orders.sql".to_string()],
            deleted_file_count: 0,
            deleted_files: vec![],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Manifest:      stale (1 file newer)"));
    }

    #[test]
    fn test_manifest_deleted_only() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 0,
            stale_files: vec![],
            deleted_file_count: 2,
            deleted_files: vec![
                "models/old_model.sql".to_string(),
                "models/removed.sql".to_string(),
            ],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Manifest:      stale (2 deleted)"));
        assert!(output.contains("models/old_model.sql"));
        assert!(!output.contains("newer"));
    }

    #[test]
    fn test_manifest_stale_and_deleted() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 1,
            stale_files: vec!["models/updated.sql".to_string()],
            deleted_file_count: 1,
            deleted_files: vec!["models/removed.sql".to_string()],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("stale (1 file newer, 1 deleted)"));
        assert!(output.contains("models/updated.sql"));
        assert!(output.contains("models/removed.sql"));
    }

    #[test]
    fn test_manifest_file_list_truncation() {
        let mut report = make_report();
        let files: Vec<String> = (0..8).map(|i| format!("models/model_{}.sql", i)).collect();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 8,
            stale_files: files,
            deleted_file_count: 0,
            deleted_files: vec![],
        });
        let mut buf = Vec::new();
        render_summary_text(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // Should show first 5 and "... and 3 more"
        assert!(output.contains("model_0.sql"));
        assert!(output.contains("model_4.sql"));
        assert!(!output.contains("model_5.sql"));
        assert!(output.contains("... and 3 more"));
    }

    #[test]
    fn test_json_includes_file_lists() {
        let mut report = make_report();
        report.manifest_status = Some(ManifestStatus {
            found: true,
            is_stale: true,
            stale_file_count: 1,
            stale_files: vec!["models/a.sql".to_string()],
            deleted_file_count: 1,
            deleted_files: vec!["models/b.sql".to_string()],
        });
        let mut buf = Vec::new();
        render_summary_json(&report, &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let ms = &parsed["manifest_status"];
        assert_eq!(ms["stale_file_count"], 1);
        assert_eq!(ms["stale_files"][0], "models/a.sql");
        assert_eq!(ms["deleted_file_count"], 1);
        assert_eq!(ms["deleted_files"][0], "models/b.sql");
    }
}
