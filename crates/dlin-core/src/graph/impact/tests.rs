use super::*;
use crate::graph::dependency::ProjectGap;
use std::path::PathBuf;

#[test]
fn impact_scope_requires_a_public_resource_type() {
    assert!(ImpactScope::new([]).is_none());
    assert!(ImpactScope::new([NodeType::Phantom]).is_none());
    assert_eq!(
        ImpactScope::new([NodeType::Test, NodeType::Model, NodeType::Test]),
        ImpactScope::new([NodeType::Model, NodeType::Test])
    );
}

/// The impact of `source_idx` in a hand-made graph with the given gaps.
fn impact_of(
    graph: &LineageGraph,
    source_idx: NodeIndex,
    project_gaps: &[ProjectGap],
) -> Option<ImpactReport> {
    compute_impact(
        &GraphBuild::from_parts(graph.clone(), project_gaps.to_vec()),
        source_idx,
        &ImpactScope::new(NodeType::RESOURCE_TYPES).unwrap(),
    )
}

fn make_node(
    unique_id: &str,
    label: &str,
    node_type: NodeType,
    materialization: Option<&str>,
    file_path: Option<&str>,
) -> NodeData {
    NodeData {
        unique_id: unique_id.into(),
        label: label.into(),
        node_type,
        file_path: file_path.map(PathBuf::from),
        description: None,
        materialization: materialization.map(|s| s.to_string()),
        tags: vec![],
        columns: vec![],
        exposure: None,
        aliases: vec![],
        dependency_analysis: crate::graph::dependency::DependencyAnalysis::new([]),
    }
}

fn make_test_graph() -> (LineageGraph, NodeIndex) {
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "source.raw.orders",
        "raw.orders",
        NodeType::Source,
        None,
        None,
    ));
    let stg = g.add_node(make_node(
        "model.stg_orders",
        "stg_orders",
        NodeType::Model,
        Some("view"),
        Some("models/staging/stg_orders.sql"),
    ));
    let mart = g.add_node(make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        Some("table"),
        Some("models/marts/orders.sql"),
    ));
    let test = g.add_node(make_node(
        "test.orders_positive",
        "orders_positive",
        NodeType::Test,
        None,
        None,
    ));
    let exp = g.add_node(make_node(
        "exposure.dashboard",
        "dashboard",
        NodeType::Exposure,
        None,
        None,
    ));

    g.add_edge(src, stg, EdgeData::direct(EdgeType::Source));
    g.add_edge(stg, mart, EdgeData::direct(EdgeType::Ref));
    g.add_edge(mart, test, EdgeData::direct(EdgeType::Test));
    g.add_edge(mart, exp, EdgeData::direct(EdgeType::Exposure));

    (g, stg)
}

#[test]
fn test_classify_severity_exposure() {
    let node = make_node("exposure.x", "x", NodeType::Exposure, None, None);
    assert_eq!(classify_severity(&node), ImpactSeverity::Critical);
}

#[test]
fn test_classify_severity_test() {
    let node = make_node("test.x", "x", NodeType::Test, None, None);
    assert_eq!(classify_severity(&node), ImpactSeverity::Low);
}

#[test]
fn test_classify_severity_mart_table() {
    let node = make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        Some("table"),
        None,
    );
    assert_eq!(classify_severity(&node), ImpactSeverity::High);
}

#[test]
fn test_classify_severity_mart_incremental() {
    let node = make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        Some("incremental"),
        None,
    );
    assert_eq!(classify_severity(&node), ImpactSeverity::High);
}

#[test]
fn test_classify_severity_mart_path() {
    let node = make_node(
        "model.orders",
        "orders",
        NodeType::Model,
        None,
        Some("models/marts/orders.sql"),
    );
    assert_eq!(classify_severity(&node), ImpactSeverity::High);
}

#[test]
fn test_classify_severity_staging() {
    let node = make_node(
        "model.stg_orders",
        "stg_orders",
        NodeType::Model,
        Some("view"),
        Some("models/staging/stg_orders.sql"),
    );
    assert_eq!(classify_severity(&node), ImpactSeverity::Medium);
}

#[test]
fn test_compute_impact() {
    let (g, stg) = make_test_graph();
    let report = impact_of(&g, stg, &[]).unwrap();

    assert_eq!(report.source_model, "stg_orders");
    assert_eq!(report.affected_models, Some(1)); // orders
    assert_eq!(report.affected_tests, Some(1)); // orders_positive
    assert_eq!(report.affected_exposures, Some(1)); // dashboard
    assert_eq!(report.overall_severity, Some(ImpactSeverity::Critical));
    assert_eq!(report.impacted_nodes.len(), 3);

    // Exposure path: stg_orders -> orders -> dashboard
    assert_eq!(report.exposure_paths.len(), 1);
    assert_eq!(report.exposure_paths[0].exposure, "dashboard");
    assert_eq!(
        report.exposure_paths[0].path,
        vec!["stg_orders", "orders", "dashboard"]
    );
}

#[test]
fn test_compute_impact_leaf_node() {
    let (g, _) = make_test_graph();
    let exp = g
        .node_indices()
        .find(|&i| g[i].label == "dashboard")
        .unwrap();
    let report = impact_of(&g, exp, &[]).unwrap();

    assert_eq!(report.source_model, "dashboard");
    assert_eq!(report.affected_models, Some(0));
    assert_eq!(report.affected_tests, Some(0));
    assert_eq!(report.affected_exposures, Some(0));
    assert!(report.impacted_nodes.is_empty());
    assert!(report.exposure_paths.is_empty());
}

#[test]
fn test_impact_severity_ordering() {
    assert!(ImpactSeverity::Low < ImpactSeverity::Medium);
    assert!(ImpactSeverity::Medium < ImpactSeverity::High);
    assert!(ImpactSeverity::High < ImpactSeverity::Critical);
}

#[test]
fn test_impact_isolated_node() {
    let mut g = LineageGraph::new();
    let n = g.add_node(make_node("model.x", "x", NodeType::Model, None, None));
    let report = impact_of(&g, n, &[]).unwrap();
    assert_eq!(report.affected_models, Some(0));
    assert_eq!(report.affected_tests, Some(0));
    assert_eq!(report.affected_exposures, Some(0));
    assert!(report.impacted_nodes.is_empty());
}

#[test]
fn test_exposure_paths_multiple_exposures() {
    // Diamond graph: src -> A -> exp1, src -> B -> exp2
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "model.src",
        "src",
        NodeType::Model,
        Some("view"),
        None,
    ));
    let a = g.add_node(make_node(
        "model.a",
        "a",
        NodeType::Model,
        Some("view"),
        None,
    ));
    let b = g.add_node(make_node(
        "model.b",
        "b",
        NodeType::Model,
        Some("view"),
        None,
    ));
    let exp1 = g.add_node(make_node(
        "exposure.dashboard",
        "dashboard",
        NodeType::Exposure,
        None,
        None,
    ));
    let exp2 = g.add_node(make_node(
        "exposure.report",
        "report",
        NodeType::Exposure,
        None,
        None,
    ));

    g.add_edge(src, a, EdgeData::direct(EdgeType::Ref));
    g.add_edge(src, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(a, exp1, EdgeData::direct(EdgeType::Exposure));
    g.add_edge(b, exp2, EdgeData::direct(EdgeType::Exposure));

    let report = impact_of(&g, src, &[]).unwrap();
    assert_eq!(report.affected_exposures, Some(2));
    assert_eq!(report.exposure_paths.len(), 2);

    // Sorted by exposure name
    assert_eq!(report.exposure_paths[0].exposure, "dashboard");
    assert_eq!(report.exposure_paths[0].path, vec!["src", "a", "dashboard"]);
    assert_eq!(report.exposure_paths[1].exposure, "report");
    assert_eq!(report.exposure_paths[1].path, vec!["src", "b", "report"]);
}

#[test]
fn test_exposure_paths_diamond_convergent() {
    // Diamond: src -> A -> C -> exp, src -> B -> C -> exp
    // BFS finds shortest path (through whichever of A/B is visited first)
    let mut g = LineageGraph::new();
    let src = g.add_node(make_node(
        "model.src",
        "src",
        NodeType::Model,
        Some("view"),
        None,
    ));
    let a = g.add_node(make_node(
        "model.a",
        "a",
        NodeType::Model,
        Some("view"),
        None,
    ));
    let b = g.add_node(make_node(
        "model.b",
        "b",
        NodeType::Model,
        Some("view"),
        None,
    ));
    let c = g.add_node(make_node(
        "model.c",
        "c",
        NodeType::Model,
        Some("table"),
        None,
    ));
    let exp = g.add_node(make_node(
        "exposure.dashboard",
        "dashboard",
        NodeType::Exposure,
        None,
        None,
    ));

    g.add_edge(src, a, EdgeData::direct(EdgeType::Ref));
    g.add_edge(src, b, EdgeData::direct(EdgeType::Ref));
    g.add_edge(a, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(b, c, EdgeData::direct(EdgeType::Ref));
    g.add_edge(c, exp, EdgeData::direct(EdgeType::Exposure));

    let report = impact_of(&g, src, &[]).unwrap();
    assert_eq!(report.affected_exposures, Some(1));
    // Both paths should be found: src->a->c->dashboard and src->b->c->dashboard
    assert_eq!(report.exposure_paths.len(), 2);
    assert_eq!(
        report.exposure_paths[0].path,
        vec!["src", "a", "c", "dashboard"]
    );
    assert_eq!(
        report.exposure_paths[1].path,
        vec!["src", "b", "c", "dashboard"]
    );
}

#[test]
fn test_classify_severity_source_seed_snapshot() {
    // Covers the wildcard arm (line 76): Source, Seed, Snapshot → Medium
    let source = make_node("source.raw.o", "raw.o", NodeType::Source, None, None);
    assert_eq!(classify_severity(&source), ImpactSeverity::Medium);

    let seed = make_node("seed.countries", "countries", NodeType::Seed, None, None);
    assert_eq!(classify_severity(&seed), ImpactSeverity::Medium);

    let snap = make_node("snapshot.snap", "snap", NodeType::Snapshot, None, None);
    assert_eq!(classify_severity(&snap), ImpactSeverity::Medium);
}

mod exploration {
    use super::*;
    use crate::graph::dependency::{
        DependencyAnalysis, DependencyReason, DependencyReasonCode, InputFailure, InputKind,
        InputLocation, InputStage, NodeCallSite, NodeDependencyReason, NotAssessedScope,
        ProjectGap, ProjectScope, RuntimeValueSource,
    };
    use crate::graph::exploration::{ExplorationGap, ExplorationStatus};
    use crate::graph::impact::{DependencyPolicy, NamespaceMatch};
    use serde_json::json;

    fn at(unique_id: &str) -> NodeCallSite {
        NodeCallSite {
            unique_id: unique_id.to_string(),
            path: format!("models/{unique_id}.sql"),
            line: Some(1),
            expr: None,
            var_template: None,
        }
    }

    fn namespaced(unique_id: &str, namespace: &str) -> NodeDependencyReason {
        DependencyReason::UnsupportedMacroNamespace {
            namespace: namespace.to_string(),
            macro_name: "m".to_string(),
            at: at(unique_id),
        }
    }

    fn dispatch(unique_id: &str, namespace: Option<&str>) -> NodeDependencyReason {
        DependencyReason::UnsupportedDispatch {
            name: Some("m".to_string()),
            namespace: namespace.map(str::to_string),
            at: at(unique_id),
        }
    }

    fn model(unique_id: &str, reasons: Vec<NodeDependencyReason>) -> NodeData {
        let mut node = make_node(unique_id, unique_id, NodeType::Model, None, None);
        node.dependency_analysis = DependencyAnalysis::new(reasons);
        node
    }

    fn node_with(unique_id: &str, node_type: NodeType, analysis: DependencyAnalysis) -> NodeData {
        let mut node = make_node(unique_id, unique_id, node_type, None, None);
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

    fn find(graph: &LineageGraph, unique_id: &str) -> NodeIndex {
        graph
            .node_indices()
            .find(|&idx| graph[idx].unique_id == unique_id)
            .unwrap_or_else(|| panic!("missing node {unique_id}"))
    }

    fn report(graph: &LineageGraph, unique_id: &str, gaps: &[ProjectGap]) -> ImpactReport {
        impact_of(graph, find(graph, unique_id), gaps).unwrap()
    }

    fn gaps(report: &ImpactReport) -> &[ExplorationGap] {
        assert_eq!(report.exploration.len(), 1);
        report.exploration[0].gaps()
    }

    #[test]
    fn scoped_exploration_keeps_partial_intermediates_and_project_gaps() {
        let mut graph = LineageGraph::new();
        let source = graph.add_node(model("model.src", vec![]));
        let bridge = graph.add_node(node_with(
            "test.bridge",
            NodeType::Test,
            DependencyAnalysis::new([namespaced("test.bridge", "dbt")]),
        ));
        let leaf = graph.add_node(model("model.leaf", vec![]));
        graph.add_edge(bridge, leaf, EdgeData::direct(EdgeType::Test));
        graph.add_node(node_with(
            "test.unrelated",
            NodeType::Test,
            DependencyAnalysis::new([namespaced("test.unrelated", "dbt")]),
        ));

        let build = GraphBuild::from_parts(graph.clone(), vec![skipped()]);
        let report = compute_impact(
            &build,
            source,
            &ImpactScope::new([NodeType::Model]).unwrap(),
        )
        .unwrap();
        assert_eq!(report.node_types(), ["model"]);
        assert_eq!(report.affected_tests, None);
        assert_eq!(report.overall_severity, None);
        assert!(!report.proves_no_impact());
        assert_eq!(gaps(&report).len(), 2);
        assert!(gaps(&report).contains(&ExplorationGap::from(skipped())));
        assert!(matches!(
            &gaps(&report)[0],
            ExplorationGap::UnresolvedDependencies { unique_id, .. } if unique_id == "test.bridge"
        ));

        graph.add_edge(source, bridge, EdgeData::direct(EdgeType::Test));
        let build = GraphBuild::from_parts(graph, vec![]);
        let report = compute_impact(
            &build,
            source,
            &ImpactScope::new([NodeType::Model]).unwrap(),
        )
        .unwrap();
        assert_eq!(report.impacted_nodes().len(), 1);
        assert_eq!(report.impacted_nodes()[0].unique_id, "model.leaf");
        assert_eq!(report.impacted_nodes()[0].distance, 2);
        assert!(gaps(&report).is_empty());
        assert!(report.is_acceptable_under(&DependencyPolicy::deny_all()));
    }

    /// src -> a (partial) ; b (partial, unreached) ; c (complete, unreached)
    fn graph_with_partial_nodes() -> LineageGraph {
        let mut g = LineageGraph::new();
        let src = g.add_node(model("model.src", vec![]));
        let a = g.add_node(model("model.a", vec![namespaced("model.a", "dbt")]));
        g.add_node(model("model.b", vec![namespaced("model.b", "dbt_utils")]));
        g.add_node(model("model.c", vec![]));
        g.add_edge(src, a, EdgeData::direct(EdgeType::Ref));
        g
    }

    #[test]
    fn only_unreached_partial_nodes_are_gaps() {
        let g = graph_with_partial_nodes();
        let report = report(&g, "model.src", &[]);
        // `a` is reached: a dependency hidden in it adds no node to the
        // reached set.
        assert_eq!(
            gaps(&report),
            [ExplorationGap::UnresolvedDependencies {
                unique_id: "model.b".to_string(),
                depth: None,
                reasons: vec![namespaced("model.b", "dbt_utils")],
            }]
        );
        assert_eq!(
            report.exploration[0].status(),
            ExplorationStatus::Incomplete
        );
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn exploration_serialises_kind_status_and_gaps() {
        let mut g = LineageGraph::new();
        g.add_node(model("model.src", vec![]));
        let report = report(&g, "model.src", &[]);
        assert_eq!(
            serde_json::to_value(&report.exploration).unwrap(),
            json!([{"kind": "model_dependency", "status": "complete", "gaps": []}])
        );
        assert_eq!(
            serde_json::to_value(&report.source_node).unwrap(),
            json!({
                "unique_id": "model.src",
                "dependency_analysis": {"status": "complete", "unresolved": []}
            })
        );
        assert!(report.proves_no_impact());
    }

    #[test]
    fn impacted_nodes_carry_their_dependency_analysis() {
        let g = graph_with_partial_nodes();
        let report = report(&g, "model.src", &[]);
        let value = serde_json::to_value(&report.impacted_nodes).unwrap();
        assert_eq!(value[0]["unique_id"], "model.a");
        assert_eq!(value[0]["dependency_analysis"]["status"], "partial");
        assert_eq!(
            value[0]["dependency_analysis"]["unresolved"][0]["type"],
            "unsupported_macro_namespace"
        );
    }

    #[test]
    fn each_input_is_judged_by_its_own_reached_set() {
        let g = graph_with_partial_nodes();
        let from_src = report(&g, "model.src", &[]);
        let from_b = report(&g, "model.b", &[]);
        assert!(gaps(&from_src).iter().any(|gap| matches!(
            gap,
            ExplorationGap::UnresolvedDependencies { unique_id, .. } if unique_id == "model.b"
        )));
        // From `b`, `a` is not reached and becomes the gap.
        assert_eq!(
            gaps(&from_b),
            [ExplorationGap::UnresolvedDependencies {
                unique_id: "model.a".to_string(),
                depth: None,
                reasons: vec![namespaced("model.a", "dbt")],
            }]
        );
    }

    #[test]
    fn not_assessed_scopes_are_merged_and_phantoms_are_left_out() {
        let mut g = LineageGraph::new();
        g.add_node(model("model.src", vec![]));
        // A phantom carries no reason a gap could hold.
        g.add_node(node_with(
            "model.missing",
            NodeType::Phantom,
            DependencyAnalysis::not_assessed(NotAssessedScope::PhantomDependencies),
        ));
        g.add_node(node_with(
            "semantic_model.s1",
            NodeType::SemanticModel,
            DependencyAnalysis::not_assessed(NotAssessedScope::SemanticDependencies),
        ));
        g.add_node(node_with(
            "metric.m1",
            NodeType::Metric,
            DependencyAnalysis::not_assessed(NotAssessedScope::SemanticDependencies),
        ));
        let report = report(
            &g,
            "model.src",
            &[ProjectGap::NotAssessed {
                scope: ProjectScope::SemanticDependencies,
            }],
        );
        assert_eq!(
            gaps(&report),
            [ExplorationGap::NotAssessed {
                scope: ProjectScope::SemanticDependencies
            }]
        );
    }

    #[test]
    fn project_gaps_are_always_gaps() {
        let mut g = LineageGraph::new();
        g.add_node(model("model.src", vec![]));
        let report = report(&g, "model.src", &[skipped()]);
        assert_eq!(gaps(&report), [ExplorationGap::from(skipped())]);
        assert!(!report.proves_no_impact());
    }

    /// A manifest with one model: every node is reached, so only the
    /// project gap keeps the exploration incomplete.
    #[test]
    fn manifest_with_a_single_model_does_not_prove_no_impact() {
        let mut g = LineageGraph::new();
        g.add_node(node_with(
            "model.only",
            NodeType::Model,
            DependencyAnalysis::not_assessed(NotAssessedScope::ManifestDependencies),
        ));
        // A manifest build records the unassessed scope itself, so a
        // caller cannot leave it out.
        let report = compute_impact(
            &GraphBuild::from_manifest(g.clone()),
            find(&g, "model.only"),
            &ImpactScope::new(NodeType::RESOURCE_TYPES).unwrap(),
        )
        .unwrap();
        assert_eq!(
            gaps(&report),
            [ExplorationGap::NotAssessed {
                scope: ProjectScope::ManifestDependencies
            }]
        );
        assert!(!report.proves_no_impact());
        assert!(report.is_acceptable_under(
            &DependencyPolicy::deny_all().allow_not_assessed(ProjectScope::ManifestDependencies)
        ));
    }

    #[test]
    fn phantom_source_is_not_explored() {
        let mut g = LineageGraph::new();
        let phantom = g.add_node(node_with(
            "model.missing",
            NodeType::Phantom,
            DependencyAnalysis::not_assessed(NotAssessedScope::PhantomDependencies),
        ));
        let user = g.add_node(model("model.user", vec![]));
        g.add_edge(phantom, user, EdgeData::direct(EdgeType::Ref));
        let report = impact_of(&g, phantom, &[]).unwrap();
        assert!(report.exploration.is_empty());
        assert!(!report.proves_no_impact());
        assert!(!report.is_acceptable_under(&DependencyPolicy::deny_all()));

        let mut isolated = LineageGraph::new();
        let phantom = isolated.add_node(node_with(
            "model.missing",
            NodeType::Phantom,
            DependencyAnalysis::not_assessed(NotAssessedScope::PhantomDependencies),
        ));
        let report = impact_of(&isolated, phantom, &[]).unwrap();
        assert!(report.impacted_nodes.is_empty());
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn missing_source_index_has_no_report() {
        let mut g = LineageGraph::new();
        let idx = g.add_node(model("model.src", vec![]));
        g.remove_node(idx);
        assert!(impact_of(&g, idx, &[]).is_none());
    }

    #[test]
    fn impacted_nodes_prevent_proving_no_impact() {
        let mut g = LineageGraph::new();
        let src = g.add_node(model("model.src", vec![]));
        let a = g.add_node(model("model.a", vec![]));
        g.add_edge(src, a, EdgeData::direct(EdgeType::Ref));
        let report = report(&g, "model.src", &[]);
        assert_eq!(report.exploration[0].status(), ExplorationStatus::Complete);
        assert!(!report.proves_no_impact());
        assert!(report.is_acceptable_under(&DependencyPolicy::deny_all()));
    }

    #[test]
    fn policy_checks_every_reason_of_a_gap() {
        let mut g = LineageGraph::new();
        g.add_node(model("model.src", vec![]));
        g.add_node(model(
            "model.b",
            vec![
                namespaced("model.b", "dbt"),
                DependencyReason::RuntimeValueUnknown {
                    source: RuntimeValueSource::EnvVar,
                    name: Some("X".to_string()),
                    at: at("model.b"),
                },
            ],
        ));
        let report = report(&g, "model.src", &[]);
        let dbt_only = DependencyPolicy::deny_all().allow_reason(
            DependencyReasonCode::UnsupportedMacroNamespace,
            NamespaceMatch::Exact("dbt".to_string()),
        );
        assert!(!report.is_acceptable_under(&DependencyPolicy::deny_all()));
        // One accepted reason does not accept the gap.
        assert!(!report.is_acceptable_under(&dbt_only));
        let both = dbt_only.allow_reason(
            DependencyReasonCode::RuntimeValueUnknown,
            NamespaceMatch::Any,
        );
        assert!(report.is_acceptable_under(&both));
        // Accepting gaps proves nothing.
        assert!(!report.proves_no_impact());
    }

    #[test]
    fn namespace_rules_match_the_written_namespace() {
        let policy = DependencyPolicy::deny_all().allow_reason(
            DependencyReasonCode::UnsupportedMacroNamespace,
            NamespaceMatch::Exact("dbt".to_string()),
        );
        assert!(policy.accepts_reason(&namespaced("model.a", "dbt")));
        assert!(!policy.accepts_reason(&namespaced("model.a", "dbt_utils")));
        // An exact rule never matches a reason of another type.
        assert!(!policy.accepts_reason(&dispatch("model.a", Some("dbt"))));

        let unspecified = DependencyPolicy::deny_all().allow_reason(
            DependencyReasonCode::UnsupportedDispatch,
            NamespaceMatch::Unspecified,
        );
        assert!(unspecified.accepts_reason(&dispatch("model.a", None)));
        assert!(!unspecified.accepts_reason(&dispatch("model.a", Some("p"))));

        let exact = DependencyPolicy::deny_all().allow_reason(
            DependencyReasonCode::UnsupportedDispatch,
            NamespaceMatch::Exact("p".to_string()),
        );
        assert!(!exact.accepts_reason(&dispatch("model.a", None)));
        assert!(exact.accepts_reason(&dispatch("model.a", Some("p"))));
    }

    #[test]
    fn project_gaps_need_their_own_permission() {
        let empty_reasons = ExplorationGap::UnresolvedDependencies {
            unique_id: "model.x".to_string(),
            depth: None,
            reasons: vec![],
        };
        let permissive = DependencyPolicy::deny_all().allow_reason(
            DependencyReasonCode::UnsupportedMacroNamespace,
            NamespaceMatch::Any,
        );
        assert!(!permissive.accepts_gap(&empty_reasons));
        assert!(!permissive.accepts_gap(&ExplorationGap::from(skipped())));
        assert!(
            permissive
                .clone()
                .allow_input_skipped()
                .accepts_gap(&ExplorationGap::from(skipped()))
        );
        let semantic = ExplorationGap::NotAssessed {
            scope: ProjectScope::SemanticDependencies,
        };
        assert!(!permissive.accepts_gap(&semantic));
        assert!(
            !permissive
                .clone()
                .allow_not_assessed(ProjectScope::ManifestDependencies)
                .accepts_gap(&semantic)
        );
        assert!(
            permissive
                .allow_not_assessed(ProjectScope::SemanticDependencies)
                .accepts_gap(&semantic)
        );
    }
}

/// Impact reports built from projects, as the CLI builds them.
mod projects {
    use super::*;
    use crate::graph::builder::build_graph_report;
    use crate::graph::exploration::{ExplorationGap, ExplorationStatus};
    use crate::parser::discovery::discover_files;
    use crate::parser::project::DbtProject;
    use serde_json::json;
    use std::fs;

    fn impact(files: &[(&str, &str)], input: &str) -> ImpactReport {
        impact_with_scope(
            files,
            input,
            &ImpactScope::new(NodeType::RESOURCE_TYPES).unwrap(),
        )
    }

    fn impact_with_scope(files: &[(&str, &str)], input: &str, scope: &ImpactScope) -> ImpactReport {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("dbt_project.yml"), "name: p\n").unwrap();
        for (path, content) in files {
            let path = tmp.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let project_dir = tmp.path().canonicalize().unwrap();
        let project = DbtProject::load(&project_dir).unwrap();
        let discovered = discover_files(&project.resolve_paths(&project_dir)).unwrap();
        let build =
            build_graph_report(&project_dir, &project.name, &discovered, &project.vars).unwrap();
        let idx = crate::graph::filter::resolve_node_by_name(build.graph(), input).unwrap();
        compute_impact(&build, idx, scope).unwrap()
    }

    fn gap_ids(report: &ImpactReport) -> Vec<String> {
        report.exploration[0]
            .gaps()
            .iter()
            .filter_map(|gap| match gap {
                ExplorationGap::UnresolvedDependencies { unique_id, .. } => Some(unique_id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn isolated_model_proves_no_impact_with_a_generic_test_out_of_scope() {
        let files = [
            ("models/isolated.sql", "select 1 as id"),
            ("models/validated.sql", "select 1 as id"),
            (
                "models/schema.yml",
                r#"version: 2
models:
  - name: validated
    columns:
      - name: id
        tests: [not_null]
"#,
            ),
        ];
        let report = impact_with_scope(&files, "isolated", &ImpactScope::default());
        assert_eq!(
            report.exploration()[0].status(),
            ExplorationStatus::Complete
        );
        assert!(report.proves_no_impact());
        assert_eq!(report.affected_tests, None);

        let scope = ImpactScope::new([NodeType::Test]).unwrap();
        let report = impact_with_scope(&files, "isolated", &scope);
        assert_eq!(
            report.exploration()[0].status(),
            ExplorationStatus::Incomplete
        );
        assert!(!report.proves_no_impact());
        assert_eq!(report.affected_tests, Some(0));
        assert_eq!(report.affected_models, None);
        assert_eq!(report.affected_exposures, None);
    }

    /// A generic test that reads a model only in its body is not reached
    /// from that model, so the test is a gap.
    #[test]
    fn test_reading_a_model_only_in_its_body_is_a_gap() {
        let files = [
            ("models/a.sql", "select 1 as id"),
            ("models/thresholds.sql", "select 1 as limit_value"),
            (
                "macros/check_threshold.sql",
                "{% test check_threshold(model) %}select * from {{ model }} cross join {{ ref('thresholds') }}{% endtest %}",
            ),
            (
                "models/schema.yml",
                r#"
version: 2
models:
  - name: a
    tests:
      - check_threshold
"#,
            ),
        ];
        let report = impact(&files, "thresholds");
        assert!(report.impacted_nodes.is_empty());
        assert!(!report.proves_no_impact());
        let value = serde_json::to_value(&report.exploration).unwrap();
        assert_eq!(value[0]["status"], "incomplete");
        assert_eq!(
            value[0]["gaps"],
            json!([{
                "type": "unresolved_dependencies",
                "unique_id": "test.check_threshold.a",
                "reasons": [{
                    "type": "generic_test_body_not_analyzed",
                    "test_name": "check_threshold",
                    "at": {
                        "unique_id": "test.check_threshold.a",
                        "path": "models/schema.yml",
                        "line": null,
                        "expr": null
                    }
                }]
            }])
        );
        // From `a`, the test is reached through its known edge.
        let from_a = impact(&files, "a");
        assert!(!gap_ids(&from_a).contains(&"test.check_threshold.a".to_string()));
    }

    /// A ref whose name comes from an environment variable without a
    /// default has no edge, so the referencing model is a gap.
    #[test]
    fn ref_named_by_an_unset_env_var_is_a_gap() {
        let report = impact(
            &[
                ("models/base.sql", "select 1 as id"),
                (
                    "models/downstream.sql",
                    "select * from {{ ref(env_var('DLIN_REVIEW_UPSTREAM')) }}",
                ),
            ],
            "base",
        );
        assert!(report.impacted_nodes.is_empty());
        assert!(!report.proves_no_impact());
        assert_eq!(gap_ids(&report), ["model.downstream"]);
        let value = serde_json::to_value(&report.exploration[0].gaps()[0]).unwrap();
        assert_eq!(value["reasons"][0]["type"], "runtime_value_unknown");
        assert_eq!(value["reasons"][0]["source"], "env_var");
    }

    #[test]
    fn skipped_input_makes_every_report_incomplete() {
        let report = impact(
            &[
                ("models/base.sql", "select 1 as id"),
                ("models/broken.yml", "version: 2\nmodels: [\n"),
            ],
            "base",
        );
        assert!(!report.proves_no_impact());
        let value = serde_json::to_value(report.exploration[0].gaps()).unwrap();
        assert_eq!(value[0]["type"], "input_skipped");
        assert_eq!(value[0]["location"]["path"], "models/broken.yml");
    }

    #[test]
    fn complete_project_proves_no_impact_of_a_leaf() {
        let report = impact(
            &[
                ("models/base.sql", "select 1 as id"),
                ("models/leaf.sql", "select * from {{ ref('base') }}"),
            ],
            "leaf",
        );
        assert!(report.proves_no_impact());
        let from_base = impact(
            &[
                ("models/base.sql", "select 1 as id"),
                ("models/leaf.sql", "select * from {{ ref('base') }}"),
            ],
            "base",
        );
        assert!(!from_base.proves_no_impact());
        assert_eq!(
            from_base.exploration[0].status(),
            crate::graph::exploration::ExplorationStatus::Complete
        );
    }
}
