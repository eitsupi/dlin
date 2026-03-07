use std::io::Write;

use colored::Colorize;

use crate::graph::impact::{ImpactReport, ImpactSeverity};

/// Render impact report as colored text to stdout
pub fn render_impact_text(report: &ImpactReport) {
    render_impact_text_to_writer(report, &mut std::io::stdout().lock());
}

fn severity_color(severity: ImpactSeverity) -> colored::Color {
    match severity {
        ImpactSeverity::Low => colored::Color::Green,
        ImpactSeverity::Medium => colored::Color::Yellow,
        ImpactSeverity::High => colored::Color::Red,
        ImpactSeverity::Critical => colored::Color::BrightRed,
    }
}

pub fn render_impact_text_to_writer<W: Write>(report: &ImpactReport, w: &mut W) {
    writeln!(w).unwrap();
    writeln!(
        w,
        "{}",
        format!("Impact Analysis: {}", report.source_model).bold()
    )
    .unwrap();
    writeln!(w, "{}", "=".repeat(50)).unwrap();

    let severity_str = report
        .overall_severity
        .label()
        .to_uppercase()
        .color(severity_color(report.overall_severity))
        .bold();
    writeln!(w, "Overall Severity: {}", severity_str).unwrap();
    writeln!(w).unwrap();

    writeln!(w, "{}", "Summary:".bold()).unwrap();
    writeln!(w, "  Affected models:    {}", report.affected_models).unwrap();
    writeln!(w, "  Affected tests:     {}", report.affected_tests).unwrap();
    writeln!(w, "  Affected exposures: {}", report.affected_exposures).unwrap();
    writeln!(w).unwrap();

    if !report.exposure_paths.is_empty() {
        writeln!(w, "{}", "Exposure Paths:".bold()).unwrap();
        for ep in &report.exposure_paths {
            writeln!(w, "  {}", ep.path.join(" -> ")).unwrap();
        }
        if report.exposure_paths_truncated {
            writeln!(
                w,
                "  {} Use `dlin graph {} --include-exposures` to see the full lineage.",
                "(truncated)".dimmed(),
                report.source_model
            )
            .unwrap();
        }
        writeln!(w).unwrap();
    }

    if !report.impacted_nodes.is_empty() {
        writeln!(w, "{}", "Impacted Nodes:".bold()).unwrap();
        for node in &report.impacted_nodes {
            let sev = node.severity.label().color(severity_color(node.severity));
            if let Some(ref path) = node.file_path {
                writeln!(
                    w,
                    "  [{:<8}] {} ({}, {} hops) [{}]",
                    sev, node.label, node.node_type, node.distance, path
                )
                .unwrap();
            } else {
                writeln!(
                    w,
                    "  [{:<8}] {} ({}, {} hops)",
                    sev, node.label, node.node_type, node.distance
                )
                .unwrap();
            }
        }
    }

    writeln!(w).unwrap();
}

/// Render impact reports as a JSON array to stdout.
pub fn render_impact_json(reports: &[ImpactReport]) {
    render_impact_json_to_writer(reports, &mut std::io::stdout().lock());
}

pub fn render_impact_json_to_writer<W: Write>(reports: &[ImpactReport], w: &mut W) {
    serde_json::to_writer_pretty(&mut *w, reports).unwrap();
    writeln!(w).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::impact::{ExposurePath, ImpactReport, ImpactSeverity, ImpactedNode};

    fn make_report() -> ImpactReport {
        ImpactReport {
            source_model: "stg_orders".to_string(),
            overall_severity: ImpactSeverity::Critical,
            affected_models: 1,
            affected_tests: 1,
            affected_exposures: 1,
            exposure_paths: vec![ExposurePath {
                exposure: "dashboard".to_string(),
                path: vec![
                    "stg_orders".to_string(),
                    "orders".to_string(),
                    "dashboard".to_string(),
                ],
            }],
            exposure_paths_truncated: false,
            impacted_nodes: vec![
                ImpactedNode {
                    unique_id: "exposure.dashboard".to_string(),
                    label: "dashboard".to_string(),
                    node_type: "exposure".to_string(),
                    file_path: None,
                    severity: ImpactSeverity::Critical,
                    distance: 2,
                },
                ImpactedNode {
                    unique_id: "model.orders".to_string(),
                    label: "orders".to_string(),
                    node_type: "model".to_string(),
                    file_path: Some("models/marts/orders.sql".to_string()),
                    severity: ImpactSeverity::High,
                    distance: 1,
                },
                ImpactedNode {
                    unique_id: "test.orders_positive".to_string(),
                    label: "orders_positive".to_string(),
                    node_type: "test".to_string(),
                    file_path: None,
                    severity: ImpactSeverity::Low,
                    distance: 2,
                },
            ],
        }
    }

    #[test]
    fn test_render_impact_text() {
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf);
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
        render_impact_json_to_writer(&[report], &mut buf);
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
        let report = ImpactReport {
            source_model: "isolated".to_string(),
            overall_severity: ImpactSeverity::Low,
            affected_models: 0,
            affected_tests: 0,
            affected_exposures: 0,
            exposure_paths: vec![],
            exposure_paths_truncated: false,
            impacted_nodes: vec![],
        };
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("Impact Analysis: isolated"));
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
        let report = ImpactReport {
            source_model: "stg_payments".to_string(),
            overall_severity: ImpactSeverity::Medium,
            affected_models: 2,
            affected_tests: 0,
            affected_exposures: 0,
            exposure_paths: vec![],
            exposure_paths_truncated: false,
            impacted_nodes: vec![ImpactedNode {
                unique_id: "model.payments".to_string(),
                label: "payments".to_string(),
                node_type: "model".to_string(),
                file_path: None,
                severity: ImpactSeverity::Medium,
                distance: 1,
            }],
        };
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf);
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
        render_impact_text_to_writer(&report, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_snapshot_impact_json() {
        let report = make_report();
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report], &mut buf);
        let output = String::from_utf8(buf).unwrap();
        insta::assert_snapshot!(output);
    }

    #[test]
    fn test_render_impact_json_multiple() {
        let report1 = make_report();
        let report2 = ImpactReport {
            source_model: "orders".to_string(),
            overall_severity: ImpactSeverity::Low,
            affected_models: 0,
            affected_tests: 0,
            affected_exposures: 0,
            exposure_paths: vec![],
            exposure_paths_truncated: false,
            impacted_nodes: vec![],
        };
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[report1, report2], &mut buf);
        let output = String::from_utf8(buf).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["source_model"], "stg_orders");
        assert_eq!(arr[1]["source_model"], "orders");
    }

    #[test]
    fn test_render_impact_json_empty() {
        let mut buf = Vec::new();
        render_impact_json_to_writer(&[], &mut buf);
        let output = String::from_utf8(buf).unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_render_impact_text_truncated_paths() {
        let report = ImpactReport {
            source_model: "stg_orders".to_string(),
            overall_severity: ImpactSeverity::Critical,
            affected_models: 1,
            affected_tests: 0,
            affected_exposures: 1,
            exposure_paths: vec![ExposurePath {
                exposure: "dashboard".to_string(),
                path: vec![
                    "stg_orders".to_string(),
                    "orders".to_string(),
                    "dashboard".to_string(),
                ],
            }],
            exposure_paths_truncated: true,
            impacted_nodes: vec![],
        };
        let mut buf = Vec::new();
        render_impact_text_to_writer(&report, &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("(truncated)"));
        assert!(output.contains("dlin graph stg_orders --include-exposures"));
    }
}
