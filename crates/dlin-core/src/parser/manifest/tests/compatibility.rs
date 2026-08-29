use super::*;

#[test]
fn test_resource_type_classifier() {
    assert_eq!(
        classify_resource_type("model"),
        ManifestResourceType::Known(NodeType::Model)
    );
    assert_eq!(
        classify_resource_type("source"),
        ManifestResourceType::Known(NodeType::Source)
    );
    assert_eq!(
        classify_resource_type("seed"),
        ManifestResourceType::Known(NodeType::Seed)
    );
    assert_eq!(
        classify_resource_type("snapshot"),
        ManifestResourceType::Known(NodeType::Snapshot)
    );
    assert_eq!(
        classify_resource_type("test"),
        ManifestResourceType::Known(NodeType::Test)
    );
    assert_eq!(
        classify_resource_type("exposure"),
        ManifestResourceType::Known(NodeType::Exposure)
    );
    assert_eq!(
        classify_resource_type("unknown"),
        ManifestResourceType::Unknown("unknown".to_string())
    );
}

#[test]
fn test_resource_classifier_does_not_fallback_unknown_types_to_model() {
    assert_eq!(
        classify_resource_type("model"),
        ManifestResourceType::Known(NodeType::Model)
    );
    assert_eq!(
        classify_resource_type("analysis"),
        ManifestResourceType::Known(NodeType::Model)
    );
    assert_eq!(
        classify_resource_type("operation"),
        ManifestResourceType::Unknown("operation".to_string())
    );
    assert_eq!(
        classify_resource_type("future_resource"),
        ManifestResourceType::Unknown("future_resource".to_string())
    );
}

#[test]
fn test_unknown_manifest_resource_is_reported_and_omitted_from_graph() {
    let content = br#"{
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
            "dbt_version": "1.8.0"
        },
        "nodes": {
            "operation.proj.report": {
                "unique_id": "operation.proj.report",
                "name": "report",
                "resource_type": "operation",
                "depends_on": {"nodes": []},
                "config": {},
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "compiled_code": null,
                "database": null,
                "schema": null
            },
            "model.proj.orders": {
                "unique_id": "model.proj.orders",
                "name": "orders",
                "resource_type": "model",
                "depends_on": {"nodes": ["operation.proj.report"]},
                "config": {},
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "compiled_code": null,
                "database": null,
                "schema": null
            }
        }
    }"#;
    let report = load_manifest_report_from_bytes(content, Path::new("manifest.json"));
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType)
        .expect("unsupported resource should have a typed diagnostic");
    assert_eq!(
        diagnostic.raw_resource.as_deref(),
        Some("operation.proj.report")
    );
    assert_eq!(diagnostic.raw_type.as_deref(), Some("operation"));
    assert!(diagnostic.hint.is_some());
    let manifest = report.manifest.as_ref().expect("permissive load succeeds");
    let graph = build_graph_from_parsed_manifest(manifest).unwrap();
    assert!(
        graph
            .node_indices()
            .all(|idx| graph[idx].node_type != NodeType::Model || graph[idx].label == "orders")
    );
    assert_eq!(
        graph
            .node_indices()
            .filter(|&idx| graph[idx].node_type == NodeType::Phantom)
            .count(),
        0,
        "a manifest resource omitted for unsupported type is not an unresolved dependency"
    );
    assert!(build_graph_from_parsed_manifest_strict(manifest).is_err());
}

#[test]
fn analysis_is_a_model_with_ordinary_dependency_edges() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {
                "model.proj.upstream": {
                    "unique_id": "model.proj.upstream",
                    "name": "upstream",
                    "resource_type": "model",
                    "depends_on": {"nodes": []},
                    "config": {},
                    "description": null,
                    "path": null,
                    "original_file_path": null,
                    "columns": {},
                    "compiled_code": null,
                    "database": null,
                    "schema": null
                },
                "analysis.proj.report": {
                    "unique_id": "analysis.proj.report",
                    "name": "report",
                    "resource_type": "analysis",
                    "depends_on": {"nodes": ["model.proj.upstream"]},
                    "config": {},
                    "description": null,
                    "path": null,
                    "original_file_path": null,
                    "columns": {},
                    "compiled_code": null,
                    "database": null,
                    "schema": null
                }
            }
        }"#,
        std::path::Path::new("manifest.json"),
    );
    assert!(
        !report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType)
    );
    let manifest = report.manifest.expect("permissive load succeeds");
    let graph = build_graph_from_parsed_manifest_strict(&manifest).unwrap();
    let upstream = graph
        .node_indices()
        .find(|&index| graph[index].label == "upstream")
        .unwrap();
    let analysis = graph
        .node_indices()
        .find(|&index| graph[index].label == "report")
        .unwrap();
    assert_eq!(graph[analysis].node_type, NodeType::Model);
    assert!(graph.find_edge(upstream, analysis).is_some());
}

#[test]
fn strict_graph_rejects_functions_and_unknown_resource_maps() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {},
            "functions": {
                "function.proj.helper": {"name": "helper"}
            },
            "future_resources": {
                "future.proj.item": {"resource_type": "future_resource"}
            }
        }"#,
        std::path::Path::new("manifest.json"),
    );
    let manifest = report.manifest.expect("permissive load succeeds");
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest resource 'function.proj.helper' uses unsupported resource type 'function'"###);

    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {},
            "future_resources": {
                "future.proj.item": {"resource_type": "future_resource"}
            }
        }"#,
        std::path::Path::new("manifest.json"),
    );
    let manifest = report.manifest.expect("permissive load succeeds");
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest resource 'future.proj.item' uses unsupported resource type 'future_resource'"###);
}

#[test]
fn strict_graph_rejects_future_schema_capability() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v99/manifest.json",
                "dbt_version": "1.9.0"
            },
            "nodes": {}
        }"#,
        std::path::Path::new("manifest.json"),
    );
    let manifest = report.manifest.expect("permissive load succeeds");
    assert!(manifest.capabilities.future_schema);
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest uses a future dbt schema version"###);
}

#[test]
fn strict_graph_rejects_unit_tests() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {},
            "unit_tests": {
                "unit_test.proj.check": {"name": "check"}
            }
        }"#,
        std::path::Path::new("manifest.json"),
    );
    let manifest = report.manifest.expect("permissive load succeeds");
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest resource 'unit_test.proj.check' uses unsupported resource type 'unit_test'"###);
}

#[test]
fn strict_graph_rejects_future_schema_from_direct_deserialize() {
    let manifest: Manifest = serde_json::from_value(serde_json::json!({
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v99/manifest.json"
        },
        "nodes": {}
    }))
    .unwrap();
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest uses a future dbt schema version"###);
}

#[test]
fn strict_graph_rejects_future_schema_capability_without_raw_schema() {
    let manifest = Manifest {
        capabilities: ManifestCapabilities {
            future_schema: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest uses a future dbt schema version"###);
}

#[test]
fn strict_graph_selects_first_unsupported_resource_by_unique_id() {
    let node = |unique_id: &str, name: &str, resource_type: &str| {
        serde_json::json!({
            "unique_id": unique_id,
            "name": name,
            "resource_type": resource_type,
            "depends_on": {"nodes": []},
            "config": {},
            "description": null,
            "path": null,
            "original_file_path": null,
            "columns": {},
            "compiled_code": null,
            "database": null,
            "schema": null
        })
    };
    let manifest: Manifest = serde_json::from_value(serde_json::json!({
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json"
        },
        "nodes": {
            "operation.proj.z": node("operation.proj.z", "z", "operation"),
            "operation.proj.a": node("operation.proj.a", "a", "operation")
        },
        "functions": {
            "function.proj.b": {"name": "b"}
        },
        "unit_tests": {
            "unit_test.proj.c": {"name": "c"}
        },
        "future_resources": {
            "future.proj.d": {"resource_type": "future_resource"}
        }
    }))
    .unwrap();
    let error = build_graph_from_parsed_manifest_strict(&manifest).unwrap_err();
    insta::assert_snapshot!(error.to_string(), @r###"manifest resource 'function.proj.b' uses unsupported resource type 'function'"###);
}

#[test]
fn test_manifest_graph_report_propagates_load_diagnostics() {
    let content = br#"{
        "metadata": {
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
            "dbt_version": "1.8.0"
        },
        "nodes": {
            "operation.proj.refresh": {
                "unique_id": "operation.proj.refresh",
                "name": "refresh",
                "resource_type": "operation",
                "depends_on": {"nodes": []},
                "config": {},
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "compiled_code": null,
                "database": null,
                "schema": null
            }
        }
    }"#;
    let report = build_graph_from_load_report(load_manifest_report_from_bytes(
        content,
        std::path::Path::new("manifest.json"),
    ))
    .unwrap();
    assert_eq!(report.graph.node_count(), 0);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType)
    );
    assert!(report.manifest.nodes.contains_key("operation.proj.refresh"));
}
