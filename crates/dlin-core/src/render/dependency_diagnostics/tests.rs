use super::*;
use crate::graph::dependency::{
    DependencyAnalysis, InputFailure, InputKind, InputLocation, InputStage, NodeCallSite,
    NotAssessedScope, PathSetting, RuntimeValueSource,
};
use crate::graph::types::NodeData;
use crate::render::test_helpers::make_node;

fn at(unique_id: &str) -> NodeCallSite {
    NodeCallSite {
        unique_id: unique_id.to_string(),
        path: "models/a.sql".to_string(),
        line: Some(3),
        expr: Some("dbt_utils.star(ref('b'))".to_string()),
    }
}

fn namespaced(unique_id: &str, namespace: &str, macro_name: &str) -> NodeDependencyReason {
    DependencyReason::UnsupportedMacroNamespace {
        namespace: namespace.to_string(),
        macro_name: macro_name.to_string(),
        at: at(unique_id),
    }
}

fn node(unique_id: &str, node_type: NodeType, analysis: DependencyAnalysis) -> NodeData {
    let mut node = make_node(unique_id, unique_id, node_type);
    node.dependency_analysis = analysis;
    node
}

fn skipped() -> ProjectGap {
    ProjectGap::InputSkipped {
        location: InputLocation {
            path: "models/broken.yml".to_string(),
            line: None,
        },
        kind: InputKind::Yaml,
        stage: InputStage::Parse,
        failure: InputFailure::ParseError,
    }
}

fn project_graph() -> LineageGraph {
    let mut g = LineageGraph::new();
    g.add_node(node(
        "model.a",
        NodeType::Model,
        DependencyAnalysis::new([
            namespaced("model.a", "dbt", "type_string"),
            namespaced("model.a", "dbt_utils", "star"),
        ]),
    ));
    g.add_node(node(
        "model.b",
        NodeType::Model,
        DependencyAnalysis::new([
            namespaced("model.b", "dbt", "type_string"),
            DependencyReason::UnresolvedVar {
                name: Some("kind".to_string()),
                at: at("model.b"),
            },
        ]),
    ));
    g.add_node(node(
        "model.c",
        NodeType::Model,
        DependencyAnalysis::new([]),
    ));
    g.add_node(node(
        "metric.m",
        NodeType::Metric,
        DependencyAnalysis::not_assessed(NotAssessedScope::SemanticDependencies),
    ));
    // Phantoms are never counted.
    g.add_node(node(
        "model.missing",
        NodeType::Phantom,
        DependencyAnalysis::not_assessed(NotAssessedScope::PhantomDependencies),
    ));
    g
}

#[test]
fn summary_counts_reasons_by_type_and_namespace() {
    let diagnostics = DependencyDiagnostics::collect(&project_graph(), &[skipped()], &[], None);
    let summary = diagnostics.summary().unwrap().to_json();
    assert_eq!(
        summary,
        json!({
            "level": "warning",
            "code": "dependency_analysis_summary",
            "what": "model dependency analysis is incomplete: 2 node(s) partially analysed, \
                     1 node(s) not assessed, 1 input(s) skipped",
            "why": "reasons: unsupported_macro_namespace 3 (dbt 2, dbt_utils 1), \
                    unresolved_var 1; skipped inputs: 1; not assessed: semantic_dependencies",
            "hint": "run with --verbose to list each reason. These counts cover the whole \
                     project; the `exploration` of an impact report says whether its own \
                     result can miss nodes",
            "partial_nodes": 3,
            "not_assessed_nodes": 1,
            "node_reason_count": 5,
            "project_gap_count": 1,
            "reason_counts": {
                "not_assessed": 1,
                "unresolved_var": 1,
                "unsupported_macro_namespace": 3
            },
            "namespace_counts": {"unsupported_macro_namespace": {"dbt": 2, "dbt_utils": 1}},
            "project_gap_counts": {"input_skipped": 1},
            "not_assessed_scopes": ["semantic_dependencies"]
        })
    );
}

#[test]
fn complete_project_has_no_summary() {
    let mut g = LineageGraph::new();
    g.add_node(node(
        "model.c",
        NodeType::Model,
        DependencyAnalysis::new([]),
    ));
    g.add_node(node(
        "model.missing",
        NodeType::Phantom,
        DependencyAnalysis::not_assessed(NotAssessedScope::PhantomDependencies),
    ));
    let diagnostics = DependencyDiagnostics::collect(&g, &[], &[], None);
    assert!(diagnostics.summary().is_none());
    assert!(diagnostics.default_diagnostics().is_empty());
    assert!(diagnostics.verbose_diagnostics().is_empty());
}

/// In manifest mode every node is not assessed; the summary says so
/// rather than reporting failed analyses.
#[test]
fn manifest_summary_reports_unassessed_nodes() {
    let mut g = LineageGraph::new();
    for id in ["model.a", "model.b"] {
        g.add_node(node(
            id,
            NodeType::Model,
            DependencyAnalysis::not_assessed(NotAssessedScope::ManifestDependencies),
        ));
    }
    let gaps = [ProjectGap::NotAssessed {
        scope: ProjectScope::ManifestDependencies,
    }];
    let diagnostics = DependencyDiagnostics::collect(&g, &gaps, &[], None);
    let summary = diagnostics.summary().unwrap();
    assert_eq!(summary.level, Level::Info);
    assert_eq!(
        summary.what,
        "model dependency analysis is incomplete: 2 node(s) not assessed"
    );
    assert_eq!(
        summary.why.as_deref(),
        Some("not assessed: manifest_dependencies")
    );
    let json = summary.to_json();
    // Both nodes are partial in node JSON, and only not assessed.
    assert_eq!(json["partial_nodes"], 2);
    assert_eq!(json["not_assessed_nodes"], 2);
    assert_eq!(json["node_reason_count"], 2);
    // One diagnostic per node reason and per gap: the scope of each node is
    // kept apart by its owner.
    assert_eq!(diagnostics.verbose_diagnostics().len(), 1 + 2 + 1);
}

#[test]
fn verbose_lists_each_reason_and_gap() {
    let diagnostics = DependencyDiagnostics::collect(&project_graph(), &[skipped()], &[], None);
    let verbose = diagnostics.verbose_diagnostics();
    assert_eq!(verbose[0].code, SUMMARY_CODE);
    // 5 node reasons and 1 project gap.
    assert_eq!(verbose.len(), 1 + 5 + 1);
    let reason = verbose
        .iter()
        .find(|d| d.code == "unresolved_var")
        .unwrap()
        .to_json();
    assert_eq!(reason["level"], "warning");
    assert_eq!(reason["unique_id"], "model.b");
    assert_eq!(reason["reason"]["type"], "unresolved_var");
    assert_eq!(reason["reason"]["name"], "kind");
    assert_eq!(
        reason["what"],
        "model.b (models/a.sql:3, `dbt_utils.star(ref('b'))`): var('kind') has no value and no default"
    );
    let gap = verbose.last().unwrap().to_json();
    assert_eq!(gap["code"], "input_skipped");
    assert!(gap["unique_id"].is_null());
    assert_eq!(gap["gap"]["location"]["path"], "models/broken.yml");
    assert_eq!(
        gap["what"],
        "models/broken.yml: yaml input skipped at the parse step (parse error)"
    );
}

#[test]
fn config_warnings_are_default_and_info_is_verbose() {
    let config = [
        ConfigDiagnostic::ConfiguredInputPathMissing {
            paths: vec![SettingPath {
                setting: PathSetting::ModelPaths,
                path: "modelz".to_string(),
            }],
            all_model_paths_missing: true,
        },
        ConfigDiagnostic::DefaultInputPathMissing {
            setting: PathSetting::SnapshotPaths,
            path: "snapshots".to_string(),
        },
    ];
    let g = LineageGraph::new();
    let diagnostics = DependencyDiagnostics::collect(&g, &[], &config, None);
    let default = diagnostics.default_diagnostics();
    assert_eq!(default.len(), 1);
    assert_eq!(default[0].code, "configured_input_path_missing");
    assert_eq!(
        default[0].to_text(),
        "Warning: [configured_input_path_missing] paths set in dbt_project.yml do not exist: model-paths 'modelz'\n  \
         Why: no model-paths directory exists, so no model is read; dbt treats a missing \
         directory as empty\n  Hint: check the paths in dbt_project.yml and the --project-dir"
    );
    assert_eq!(
        default[0].to_json()["config"]["type"],
        "configured_input_path_missing"
    );
    let verbose = diagnostics.verbose_diagnostics();
    assert_eq!(verbose.len(), 2);
    assert_eq!(verbose[1].level, Level::Info);
    assert_eq!(verbose[1].code, "default_input_path_missing");
}

#[test]
fn text_summary_keeps_every_reason_type() {
    let mut g = LineageGraph::new();
    let mut reasons: Vec<NodeDependencyReason> = (0..7)
        .map(|i| namespaced("model.a", &format!("pkg{i}"), "m"))
        .collect();
    reasons.push(DependencyReason::RuntimeValueUnknown {
        source: RuntimeValueSource::EnvVar,
        name: Some("X".to_string()),
        at: at("model.a"),
    });
    g.add_node(node(
        "model.a",
        NodeType::Model,
        DependencyAnalysis::new(reasons),
    ));
    let summary = DependencyDiagnostics::collect(&g, &[], &[], None)
        .summary()
        .unwrap();
    assert_eq!(
        summary.why.as_deref(),
        Some(
            "reasons: unsupported_macro_namespace 7 (pkg0 1, pkg1 1, pkg2 1, pkg3 1, pkg4 1, \
             2 more namespace(s)), runtime_value_unknown 1"
        )
    );
}

#[test]
fn empty_project_names_the_searched_paths_and_the_project_directory() {
    let config = [ConfigDiagnostic::ProjectInputsEmpty {
        project_name: "p".to_string(),
        searched: vec![SettingPath {
            setting: PathSetting::ModelPaths,
            path: "models".to_string(),
        }],
    }];
    let diagnostics = DependencyDiagnostics::collect(
        &LineageGraph::new(),
        &[],
        &config,
        Some("/w/p".to_string()),
    );
    let diagnostic = &diagnostics.default_diagnostics()[0];
    assert_eq!(
        diagnostic.to_text(),
        "Warning: [project_inputs_empty] no input file was found for project 'p' in /w/p\n  \
         Why: the searched paths hold no SQL, YAML, CSV or macro file: model-paths 'models'\n  \
         Hint: check the --project-dir and the paths in dbt_project.yml"
    );
    let json = diagnostic.to_json();
    assert_eq!(json["project_dir"], "/w/p");
    assert_eq!(json["config"]["searched"][0]["path"], "models");
}
