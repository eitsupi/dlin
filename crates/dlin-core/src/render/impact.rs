use std::io::{self, IsTerminal, Write};

use colored::Colorize;

use crate::graph::exploration::{ExplorationGap, ExplorationStatus};
use crate::graph::impact::{ImpactReport, ImpactSeverity};

/// Render impact report as colored text to stdout
pub fn render_impact_text(report: &ImpactReport) {
    super::handle_stdout_result(render_impact_text_to_writer(
        report,
        &mut std::io::stdout().lock(),
    ));
}

fn severity_color(severity: ImpactSeverity) -> colored::Color {
    match severity {
        ImpactSeverity::Low => colored::Color::Green,
        ImpactSeverity::Medium => colored::Color::Yellow,
        ImpactSeverity::High => colored::Color::Red,
        ImpactSeverity::Critical => colored::Color::BrightRed,
    }
}

pub fn render_impact_text_to_writer<W: Write>(report: &ImpactReport, w: &mut W) -> io::Result<()> {
    writeln!(w)?;
    writeln!(
        w,
        "{}",
        format!("Impact Analysis: {}", report.source_model).bold()
    )?;
    writeln!(w, "{}", "=".repeat(50))?;

    writeln!(w, "Node Types: {}", report.node_types().join(", "))?;
    if let Some(severity) = report.overall_severity {
        let label = severity
            .label()
            .to_uppercase()
            .color(severity_color(severity))
            .bold();
        writeln!(w, "Overall Severity: {label}")?;
    } else {
        writeln!(w, "Overall Severity: none")?;
    }
    writeln!(w)?;

    writeln!(w, "{}", "Summary:".bold())?;
    for (label, count) in [
        ("Affected models:", report.affected_models),
        ("Affected tests:", report.affected_tests),
        ("Affected exposures:", report.affected_exposures),
    ] {
        let count = count.map_or_else(|| "out of scope".to_string(), |count| count.to_string());
        writeln!(w, "  {label:<19} {count}")?;
    }
    writeln!(w)?;

    write_exploration(report, w)?;

    if !report.exposure_paths.is_empty() {
        writeln!(w, "{}", "Exposure Paths:".bold())?;
        for ep in &report.exposure_paths {
            writeln!(w, "  {}", ep.path.join(" -> "))?;
        }
        if report.exposure_paths_truncated {
            writeln!(
                w,
                "  {} Use `dlin graph {}` to see the full lineage.",
                "(truncated)".dimmed(),
                report.source_model
            )?;
        }
        writeln!(w)?;
    }

    if !report.impacted_nodes().is_empty() {
        writeln!(w, "{}", "Impacted Nodes:".bold())?;
        for node in report.impacted_nodes() {
            let sev = node.severity.label().color(severity_color(node.severity));
            if let Some(ref path) = node.file_path {
                writeln!(
                    w,
                    "  [{:<8}] {} ({}, distance: {}) [{}]",
                    sev, node.label, node.node_type, node.distance, path
                )?;
            } else {
                writeln!(
                    w,
                    "  [{:<8}] {} ({}, distance: {})",
                    sev, node.label, node.node_type, node.distance
                )?;
            }
            if let Some(ref sql) = node.sql_content {
                writeln!(w, "    {}", "--- SQL ---".dimmed())?;
                for line in sql.lines() {
                    writeln!(w, "    {}", line)?;
                }
                writeln!(w, "    {}", "----------".dimmed())?;
            }
        }
    }

    writeln!(w)?;
    Ok(())
}

/// The number of nodes with unresolved dependencies listed by name.
const LISTED_GAP_NODES: usize = 10;

/// Whether the impacted nodes can be missing a node, and why.
fn write_exploration<W: Write>(report: &ImpactReport, w: &mut W) -> io::Result<()> {
    if report.exploration().is_empty() {
        writeln!(
            w,
            "{} {} (the input is an unresolved reference)",
            "Exploration:".bold(),
            "not explored".yellow().bold()
        )?;
        writeln!(w)?;
        return Ok(());
    }
    for exploration in report.exploration() {
        if exploration.status() == ExplorationStatus::Complete {
            writeln!(w, "{} {}", "Exploration:".bold(), "complete".green())?;
            continue;
        }
        let mut nodes = Vec::new();
        let mut skipped = 0usize;
        let mut scopes = Vec::new();
        for gap in exploration.gaps() {
            match gap {
                ExplorationGap::UnresolvedDependencies { unique_id, .. } => nodes.push(unique_id),
                ExplorationGap::InputSkipped { .. } => skipped += 1,
                ExplorationGap::NotAssessed { scope } => scopes.push(
                    serde_json::to_value(scope)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_string))
                        .unwrap_or_default(),
                ),
            }
        }
        let mut parts = Vec::new();
        if !nodes.is_empty() {
            parts.push(format!(
                "{} node(s) with unresolved dependencies",
                nodes.len()
            ));
        }
        if skipped > 0 {
            parts.push(format!("{skipped} skipped input(s)"));
        }
        if !scopes.is_empty() {
            parts.push(format!("not assessed: {}", scopes.join(", ")));
        }
        writeln!(
            w,
            "{} {} ({})",
            "Exploration:".bold(),
            "incomplete".yellow().bold(),
            parts.join("; ")
        )?;
        writeln!(
            w,
            "  Other nodes in scope may still depend on {}.",
            report.source_model
        )?;
        for unique_id in nodes.iter().take(LISTED_GAP_NODES) {
            writeln!(w, "  {unique_id}")?;
        }
        if nodes.len() > LISTED_GAP_NODES {
            writeln!(
                w,
                "  {} and {} more; use -o json for every gap",
                "...".dimmed(),
                nodes.len() - LISTED_GAP_NODES
            )?;
        }
    }
    writeln!(w)?;
    Ok(())
}

/// Render impact reports as a JSON array to stdout.
/// Pretty-prints when stdout is a terminal, compact otherwise.
pub fn render_impact_json(reports: &[ImpactReport]) {
    let mut stdout = std::io::stdout().lock();
    let pretty = stdout.is_terminal();
    super::handle_stdout_result(render_impact_json_to_writer(reports, &mut stdout, pretty));
}

pub fn render_impact_json_to_writer<W: Write>(
    reports: &[ImpactReport],
    w: &mut W,
    pretty: bool,
) -> io::Result<()> {
    if pretty {
        serde_json::to_writer_pretty(&mut *w, reports).map_err(super::serde_io_error)?;
    } else {
        serde_json::to_writer(&mut *w, reports).map_err(super::serde_io_error)?;
    }
    writeln!(w)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::dependency::DependencyAnalysis;
    use crate::graph::exploration::{Exploration, ExplorationKind};
    use crate::graph::impact::{
        ExposurePath, ImpactReport, ImpactReportParts, ImpactSeverity, ImpactSource, ImpactedNode,
    };

    fn make_report() -> ImpactReport {
        ImpactReport::from(ImpactReportParts {
            source_model: "stg_orders".to_string(),
            overall_severity: Some(ImpactSeverity::Critical),
            affected_models: Some(1),
            affected_tests: Some(1),
            affected_exposures: Some(1),
            exposure_paths: vec![ExposurePath {
                exposure_id: String::new(),
                exposure: "dashboard".to_string(),
                path: vec![
                    "stg_orders".to_string(),
                    "orders".to_string(),
                    "dashboard".to_string(),
                ],
            }],
            exposure_paths_truncated: false,
            source_node: ImpactSource {
                unique_id: "model.stg_orders".to_string(),
                dependency_analysis: DependencyAnalysis::new([]),
            },
            exploration: vec![Exploration::new(ExplorationKind::ModelDependency, [])],
            impacted_nodes: vec![
                ImpactedNode {
                    unique_id: "exposure.dashboard".to_string(),
                    label: "dashboard".to_string(),
                    node_type: "exposure".to_string(),
                    file_path: None,
                    severity: ImpactSeverity::Critical,
                    distance: 2,
                    sql_content: None,
                    dependency_analysis: DependencyAnalysis::new([]),
                },
                ImpactedNode {
                    unique_id: "model.orders".to_string(),
                    label: "orders".to_string(),
                    node_type: "model".to_string(),
                    file_path: Some("models/marts/orders.sql".to_string()),
                    severity: ImpactSeverity::High,
                    distance: 1,
                    sql_content: None,
                    dependency_analysis: DependencyAnalysis::new([]),
                },
                ImpactedNode {
                    unique_id: "test.orders_positive".to_string(),
                    label: "orders_positive".to_string(),
                    node_type: "test".to_string(),
                    file_path: None,
                    severity: ImpactSeverity::Low,
                    distance: 2,
                    sql_content: None,
                    dependency_analysis: DependencyAnalysis::new([]),
                },
            ],
        })
    }

    #[test]
    fn test_render_impact_text() {
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(output.contains("Impact Analysis: stg_orders"));
        assert!(output.contains("Affected models:    1"));
        assert!(output.contains("Affected tests:     1"));
        assert!(output.contains("Affected exposures: 1"));
        assert!(output.contains("Exposure Paths:"));
        assert!(output.contains("stg_orders -> orders -> dashboard"));
        assert!(output.contains("Impacted Nodes:"));
    }

    #[test]
    fn test_render_impact_json() {
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report], &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let first = &arr[0];
        assert_eq!(first["source_model"], "stg_orders");
        assert_eq!(first["overall_severity"], "critical");
        assert_eq!(first["affected_models"], 1);
        assert_eq!(first["impacted_nodes"].as_array().unwrap().len(), 3);

        let paths = first["exposure_paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0]["exposure"], "dashboard");
        assert_eq!(
            paths[0]["path"].as_array().unwrap(),
            &["stg_orders", "orders", "dashboard"]
        );
    }

    #[test]
    fn test_render_impact_text_empty() {
        let report = ImpactReport::from(ImpactReportParts {
            source_model: "isolated".to_string(),
            overall_severity: None,
            affected_models: Some(0),
            affected_tests: Some(0),
            affected_exposures: Some(0),
            exposure_paths: vec![],
            exposure_paths_truncated: false,
            source_node: ImpactSource {
                unique_id: "model.stg_orders".to_string(),
                dependency_analysis: DependencyAnalysis::new([]),
            },
            exploration: vec![Exploration::new(ExplorationKind::ModelDependency, [])],
            impacted_nodes: vec![],
        });
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Impact Analysis: isolated"));
        assert!(output.contains("Overall Severity: none"));
        assert!(output.contains("Affected models:    0"));
    }

    #[test]
    fn test_severity_color_all_levels() {
        assert_eq!(severity_color(ImpactSeverity::Low), colored::Color::Green);
        assert_eq!(
            severity_color(ImpactSeverity::Medium),
            colored::Color::Yellow
        );
        assert_eq!(severity_color(ImpactSeverity::High), colored::Color::Red);
        assert_eq!(
            severity_color(ImpactSeverity::Critical),
            colored::Color::BrightRed
        );
    }

    #[test]
    fn test_render_impact_text_medium_severity() {
        let report = ImpactReport::from(ImpactReportParts {
            source_model: "stg_payments".to_string(),
            overall_severity: Some(ImpactSeverity::Medium),
            affected_models: Some(2),
            affected_tests: Some(0),
            affected_exposures: Some(0),
            exposure_paths: vec![],
            exposure_paths_truncated: false,
            source_node: ImpactSource {
                unique_id: "model.stg_orders".to_string(),
                dependency_analysis: DependencyAnalysis::new([]),
            },
            exploration: vec![Exploration::new(ExplorationKind::ModelDependency, [])],
            impacted_nodes: vec![ImpactedNode {
                unique_id: "model.payments".to_string(),
                label: "payments".to_string(),
                node_type: "model".to_string(),
                file_path: None,
                severity: ImpactSeverity::Medium,
                distance: 1,
                sql_content: None,
                dependency_analysis: DependencyAnalysis::new([]),
            }],
        });
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Impact Analysis: stg_payments"));
        assert!(output.contains("MEDIUM"));
        assert!(output.contains("Affected models:    2"));
        assert!(output.contains("Impacted Nodes:"));
        assert!(output.contains("payments"));
    }

    #[test]
    fn test_snapshot_impact_text() {
        colored::control::set_override(false);
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_impact_json() {
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report], &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_render_impact_json_multiple() {
        let report1 = make_report();
        let report2 = ImpactReport::from(ImpactReportParts {
            source_model: "orders".to_string(),
            overall_severity: None,
            affected_models: Some(0),
            affected_tests: Some(0),
            affected_exposures: Some(0),
            exposure_paths: vec![],
            exposure_paths_truncated: false,
            source_node: ImpactSource {
                unique_id: "model.stg_orders".to_string(),
                dependency_analysis: DependencyAnalysis::new([]),
            },
            exploration: vec![Exploration::new(ExplorationKind::ModelDependency, [])],
            impacted_nodes: vec![],
        });
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report1, report2], &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["source_model"], "stg_orders");
        assert_eq!(arr[1]["source_model"], "orders");
    }

    #[test]
    fn test_compact_impact_json_single_line() {
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report], &mut buf, false).unwrap();
        let output = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = output.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "compact JSON should be a single line");
        let _: serde_json::Value = serde_json::from_str(&output).unwrap();
    }

    #[test]
    fn test_render_impact_json_empty() {
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[], &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    fn make_report_with_sql() -> ImpactReport {
        ImpactReport::from(ImpactReportParts {
            source_model: "stg_orders".to_string(),
            overall_severity: Some(ImpactSeverity::Critical),
            affected_models: Some(1),
            affected_tests: Some(1),
            affected_exposures: Some(1),
            exposure_paths: vec![ExposurePath {
                exposure_id: String::new(),
                exposure: "dashboard".to_string(),
                path: vec![
                    "stg_orders".to_string(),
                    "orders".to_string(),
                    "dashboard".to_string(),
                ],
            }],
            exposure_paths_truncated: false,
            source_node: ImpactSource {
                unique_id: "model.stg_orders".to_string(),
                dependency_analysis: DependencyAnalysis::new([]),
            },
            exploration: vec![Exploration::new(ExplorationKind::ModelDependency, [])],
            impacted_nodes: vec![
                ImpactedNode {
                    unique_id: "exposure.dashboard".to_string(),
                    label: "dashboard".to_string(),
                    node_type: "exposure".to_string(),
                    file_path: None,
                    severity: ImpactSeverity::Critical,
                    distance: 2,
                    sql_content: None,
                    dependency_analysis: DependencyAnalysis::new([]),
                },
                ImpactedNode {
                    unique_id: "model.orders".to_string(),
                    label: "orders".to_string(),
                    node_type: "model".to_string(),
                    file_path: Some("models/marts/orders.sql".to_string()),
                    severity: ImpactSeverity::High,
                    distance: 1,
                    sql_content: Some("SELECT\n    o.id,\n    o.status,\n    s.total\nFROM {{ ref('stg_orders') }} o\nJOIN {{ ref('stg_payments') }} s ON o.id = s.order_id".to_string()),
                    dependency_analysis: DependencyAnalysis::new([]),
                },
                ImpactedNode {
                    unique_id: "test.orders_positive".to_string(),
                    label: "orders_positive".to_string(),
                    node_type: "test".to_string(),
                    file_path: None,
                    severity: ImpactSeverity::Low,
                    distance: 2,
                    sql_content: None,
                    dependency_analysis: DependencyAnalysis::new([]),
                },
            ],
        })
    }

    #[test]
    fn test_snapshot_impact_text_with_sql() {
        colored::control::set_override(false);
        let report = make_report_with_sql();
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_impact_json_with_sql() {
        let report = make_report_with_sql();
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report], &mut buf, true).unwrap();
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_render_impact_text_truncated_paths() {
        let report = ImpactReport::from(ImpactReportParts {
            source_model: "stg_orders".to_string(),
            overall_severity: Some(ImpactSeverity::Critical),
            affected_models: Some(1),
            affected_tests: Some(0),
            affected_exposures: Some(1),
            exposure_paths: vec![ExposurePath {
                exposure_id: String::new(),
                exposure: "dashboard".to_string(),
                path: vec![
                    "stg_orders".to_string(),
                    "orders".to_string(),
                    "dashboard".to_string(),
                ],
            }],
            exposure_paths_truncated: true,
            source_node: ImpactSource {
                unique_id: "model.stg_orders".to_string(),
                dependency_analysis: DependencyAnalysis::new([]),
            },
            exploration: vec![Exploration::new(ExplorationKind::ModelDependency, [])],
            impacted_nodes: vec![],
        });
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("(truncated)"));
        assert!(output.contains("dlin graph stg_orders"));
    }

    fn text(report: &ImpactReport) -> String {
        colored::control::set_override(false);
        let mut buf = Vec::new();
        render_impact_text_to_writer(report, &mut buf).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn text_lists_the_gaps_of_an_incomplete_exploration() {
        use crate::graph::dependency::{
            DependencyReason, InputFailure, InputKind, InputLocation, InputStage, NodeCallSite,
            ProjectScope,
        };
        let node_gap = |i: usize| ExplorationGap::UnresolvedDependencies {
            unique_id: format!("model.m{i:02}"),
            depth: None,
            reasons: vec![DependencyReason::JinjaRenderFailed {
                at: NodeCallSite {
                    unique_id: format!("model.m{i:02}"),
                    path: "models/m.sql".to_string(),
                    line: None,
                    expr: None,
                    var_template: None,
                },
            }],
        };
        let mut report = make_report();
        report.set_exploration(vec![Exploration::new(
            ExplorationKind::ModelDependency,
            (0..12).map(node_gap).chain([
                ExplorationGap::InputSkipped {
                    location: InputLocation {
                        path: "models/broken.yml".to_string(),
                        line: None,
                    },
                    kind: InputKind::Yaml,
                    stage: InputStage::Parse,
                    failure: InputFailure::ParseError,
                },
                ExplorationGap::NotAssessed {
                    scope: ProjectScope::SemanticDependencies,
                },
            ]),
        )]);
        let output = text(&report);
        assert!(output.contains(
            "Exploration: incomplete (12 node(s) with unresolved dependencies; \
             1 skipped input(s); not assessed: semantic_dependencies)"
        ));
        assert!(output.contains("Other nodes in scope may still depend on stg_orders."));
        assert!(output.contains("  model.m09\n"));
        assert!(!output.contains("model.m10"));
        assert!(output.contains("and 2 more; use -o json for every gap"));
    }

    #[test]
    fn text_says_an_unresolved_input_was_not_explored() {
        let mut report = make_report();
        report.set_exploration(vec![]);
        assert!(
            text(&report)
                .contains("Exploration: not explored (the input is an unresolved reference)")
        );
    }
}
