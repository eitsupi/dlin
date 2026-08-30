use super::*;
use crate::parser::manifest::parse_schema_number;
use std::path::Path;

use super::super::load_manifest_from_bytes;

#[test]
fn test_manifest_load_report_preserves_metadata_and_capabilities() {
    let content = br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.7rc1"
            },
            "nodes": {},
            "sources": {},
            "functions": {"function.proj.hello": {"name": "hello"}},
            "unit_tests": {},
            "group_map": {},
            "disabled": {},
            "future_resource": {"kept": true}
        }"#;

    let report = load_manifest_report_from_bytes(content, Path::new("manifest.json"));
    assert!(!report.has_errors());
    assert!(report.warnings().next().is_some());
    let manifest = report.manifest.expect("valid manifest");
    assert_eq!(
        manifest.metadata.dbt_schema_version.as_deref(),
        Some("https://schemas.getdbt.com/dbt/manifest/v12/manifest.json")
    );
    assert_eq!(manifest.metadata.dbt_version.as_deref(), Some("1.8.7rc1"));
    assert_eq!(manifest.metadata.dbt_schema_version_number, Some(12));
    assert_eq!(
        manifest.capabilities.resource_maps["nodes"],
        ResourceMapPresence::Empty
    );
    assert_eq!(
        manifest.capabilities.resource_maps["macros"],
        ResourceMapPresence::Absent
    );
    assert_eq!(
        manifest.capabilities.resource_maps["functions"],
        ResourceMapPresence::NonEmpty
    );
    assert_eq!(
        manifest.capabilities.resource_maps["group_map"],
        ResourceMapPresence::Empty
    );
    assert_eq!(
        manifest.capabilities.resource_maps["disabled"],
        ResourceMapPresence::Empty
    );
    assert!(
        manifest
            .capabilities
            .unknown_top_level_keys
            .contains("future_resource")
    );
    assert_eq!(manifest.extra["future_resource"]["kept"], true);
    let compatibility_manifest =
        load_manifest_from_bytes(content, Path::new("manifest.json")).unwrap();
    assert_eq!(
        compatibility_manifest.extra["future_resource"]["kept"],
        true
    );
    assert!(
        compatibility_manifest
            .capabilities
            .unknown_top_level_keys
            .contains("future_resource")
    );
}

#[test]
fn test_manifest_load_report_accepts_explicit_null_maps() {
    let content = br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "2.0.0-alpha.4"
            },
            "nodes": {},
            "disabled": null,
            "parent_map": null,
            "child_map": null,
            "group_map": null
        }"#;

    let report = load_manifest_report_from_bytes(content, Path::new("manifest.json"));
    assert!(!report.has_errors());
    assert_eq!(report.warnings().count(), 0);
    let manifest = report.manifest.expect("valid manifest");
    assert!(manifest.disabled.is_none());
    assert!(manifest.parent_map.is_none());
    assert!(manifest.child_map.is_none());
    assert!(manifest.group_map.is_none());
    for key in ["disabled", "parent_map", "child_map", "group_map"] {
        assert_eq!(
            manifest.capabilities.resource_maps[key],
            ResourceMapPresence::Empty
        );
    }

    let compatibility_manifest =
        load_manifest_from_bytes(content, Path::new("manifest.json")).unwrap();
    assert!(compatibility_manifest.disabled.is_none());
    assert!(compatibility_manifest.parent_map.is_none());
    assert!(compatibility_manifest.child_map.is_none());
    assert!(compatibility_manifest.group_map.is_none());
}

#[test]
fn test_manifest_load_report_distinguishes_missing_empty_and_nonempty_maps() {
    let content = br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "2.0.0-alpha.4"
            },
            "nodes": {},
            "sources": {"source.proj.raw.orders": {
                "unique_id": "source.proj.raw.orders",
                "name": "orders",
                "source_name": "raw",
                "resource_type": "source",
                "description": null,
                "path": null,
                "original_file_path": null,
                "columns": {},
                "database": null,
                "schema": null,
                "identifier": null
            }},
            "group_map": {},
            "disabled": null
        }"#;

    let report = load_manifest_report_from_bytes(content, Path::new("manifest.json"));
    assert!(!report.has_errors());
    let manifest = report.manifest.expect("valid manifest");
    assert_eq!(
        manifest.capabilities.resource_maps["nodes"],
        ResourceMapPresence::Empty
    );
    assert_eq!(
        manifest.capabilities.resource_maps["sources"],
        ResourceMapPresence::NonEmpty
    );
    assert_eq!(
        manifest.capabilities.resource_maps["macros"],
        ResourceMapPresence::Absent
    );
    assert_eq!(
        manifest.capabilities.resource_maps["group_map"],
        ResourceMapPresence::Empty
    );
    assert_eq!(
        manifest.capabilities.resource_maps["disabled"],
        ResourceMapPresence::Empty
    );
}

#[test]
fn test_manifest_load_report_distinguishes_future_schema_and_parse_error() {
    let content = br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v99/manifest.json",
                "dbt_version": "1.9.0"
            },
            "nodes": {}
        }"#;
    let report = load_manifest_report_from_bytes(content, Path::new("manifest.json"));
    assert!(!report.has_errors());
    assert!(report.diagnostics.iter().any(|diagnostic| diagnostic.kind
        == ManifestDiagnosticKind::FutureSchemaVersion
        && diagnostic.severity == ManifestDiagnosticSeverity::Warning));
    assert!(
        report
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::FutureSchemaVersion)
            .unwrap()
            .kind
            .is_user_visible_warning()
    );
    assert!(report.into_manifest_strict().is_err());

    let invalid = load_manifest_report_from_bytes(b"{", Path::new("manifest.json"));
    assert!(invalid.manifest.is_none());
    assert!(invalid.diagnostics.iter().any(|diagnostic| diagnostic.kind
        == ManifestDiagnosticKind::ParseError
        && diagnostic.severity == ManifestDiagnosticSeverity::Error));
    assert!(
        !invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::MissingSchemaVersion)
    );
}

#[test]
fn test_manifest_load_report_rejects_duplicate_nodes() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {},
            "nodes": {}
        }"#,
        Path::new("manifest.json"),
    );
    assert!(report.manifest.is_none());
    assert_eq!(
        report
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::ParseError)
            .count(),
        1
    );
}

#[test]
fn test_manifest_load_report_preserves_metadata_diagnostics_on_decode_error() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": 1.8
            },
            "nodes": []
        }"#,
        Path::new("manifest.json"),
    );
    assert!(report.manifest.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.kind == ManifestDiagnosticKind::InvalidDbtVersion })
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::ParseError)
    );
}

#[test]
fn test_manifest_load_report_preserves_metadata_diagnostics_on_node_decode_error() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": 1.8
            },
            "nodes": {"model.proj.bad": {}}
        }"#,
        Path::new("manifest.json"),
    );
    assert!(report.manifest.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.kind == ManifestDiagnosticKind::InvalidDbtVersion })
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::ParseError)
    );
}

#[test]
fn test_manifest_load_report_keeps_missing_metadata_diagnostics_on_data_error() {
    let report = load_manifest_report_from_bytes(br#"{"nodes": []}"#, Path::new("manifest.json"));
    assert!(report.manifest.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.kind == ManifestDiagnosticKind::MissingSchemaVersion })
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.kind == ManifestDiagnosticKind::MissingDbtVersion })
    );
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::ParseError)
    );
}

#[test]
fn test_manifest_load_report_does_not_treat_truncated_json_as_data_error() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            }"#,
        Path::new("manifest.json"),
    );
    assert!(report.manifest.is_none());
    assert_eq!(report.diagnostics.len(), 1);
    assert_eq!(
        report.diagnostics[0].kind,
        ManifestDiagnosticKind::ParseError
    );
}

#[test]
fn test_parse_schema_number_requires_manifest_uri_suffix() {
    assert_eq!(
        parse_schema_number("https://schemas.getdbt.com/dbt/manifest/v12.json"),
        Some(12)
    );
    assert_eq!(
        parse_schema_number("https://example.test/v12/other.json"),
        None
    );
    assert_eq!(parse_schema_number("v12/manifest.json"), None);
}

#[test]
fn test_manifest_load_report_diagnoses_missing_and_invalid_versions() {
    let missing = load_manifest_report_from_bytes(br#"{"nodes": {}}"#, Path::new("manifest.json"));
    assert!(missing.diagnostics.iter().any(|diagnostic| diagnostic.kind
        == ManifestDiagnosticKind::MissingSchemaVersion
        && diagnostic.severity == ManifestDiagnosticSeverity::Warning));
    assert!(missing.diagnostics.iter().any(|diagnostic| diagnostic.kind
        == ManifestDiagnosticKind::MissingDbtVersion
        && diagnostic.severity == ManifestDiagnosticSeverity::Warning));
    assert!(!ManifestDiagnosticKind::MissingSchemaVersion.is_user_visible_warning());
    assert!(!ManifestDiagnosticKind::MissingDbtVersion.is_user_visible_warning());

    let invalid = load_manifest_report_from_bytes(
        br#"{
                "metadata": {
                    "dbt_schema_version": "not-a-schema",
                    "dbt_version": 1.8
                },
                "nodes": {}
            }"#,
        Path::new("manifest.json"),
    );
    assert!(!invalid.has_errors());
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::InvalidSchemaVersion)
    );
    assert!(
        invalid
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::InvalidDbtVersion)
    );

    let prerelease = load_manifest_report_from_bytes(
            br#"{
                "metadata": {
                    "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                    "dbt_version": "2.0.0-alpha.4"
                },
                "nodes": {}
            }"#,
            Path::new("manifest.json"),
        );
    assert!(!prerelease.has_errors());
    assert_eq!(prerelease.warnings().count(), 0);
}

#[test]
fn test_unknown_resource_diagnostic_has_shared_warning_identity() {
    let report = load_manifest_report_from_bytes(
            br#"{
                "metadata": {
                    "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                    "dbt_version": "1.8.0"
                },
                "nodes": {
                    "operation.proj.refresh": {
                        "unique_id": "operation.proj.refresh",
                        "name": "refresh",
                        "resource_type": "operation"
                    }
                }
            }"#,
            Path::new("manifest.json"),
        );
    let diagnostic = report
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType)
        .expect("unsupported resource diagnostic");
    assert_eq!(diagnostic.kind.as_str(), "unsupported_resource_type");
    assert_eq!(
        diagnostic.raw_resource.as_deref(),
        Some("operation.proj.refresh")
    );
    assert_eq!(diagnostic.raw_type.as_deref(), Some("operation"));
    let json = diagnostic.to_warning_json();
    assert_eq!(json["kind"], "unsupported_resource_type");
    assert_eq!(json["raw_type"], "operation");
    assert!(json["why"].is_null());
    assert_eq!(json["hint"], diagnostic.hint.as_deref().unwrap());
    insta::assert_snapshot!(diagnostic.to_warning_text(), @r###"
Warning: [unsupported_resource_type] manifest resource 'operation.proj.refresh' in 'nodes' uses unsupported resource type 'operation'
  Hint: Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results
"###);
    assert!(report.into_manifest_strict().is_err());
}

#[test]
fn test_manifest_load_report_keeps_nodes_diagnostic_before_later_decode_error() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {
                "operation.proj.refresh": {
                    "unique_id": "operation.proj.refresh",
                    "name": "refresh",
                    "resource_type": "operation"
                }
            },
            "sources": []
        }"#,
        Path::new("manifest.json"),
    );
    assert!(report.manifest.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("operation.proj.refresh")
    }));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::ParseError)
    );
}

#[test]
fn test_manifest_load_report_keeps_prior_node_diagnostic_on_later_node_error() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {
                "operation.proj.refresh": {
                    "unique_id": "operation.proj.refresh",
                    "name": "refresh",
                    "resource_type": "operation"
                },
                "model.proj.bad": {}
            }
        }"#,
        Path::new("manifest.json"),
    );
    assert!(report.manifest.is_none());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("operation.proj.refresh")
    }));
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == ManifestDiagnosticKind::ParseError)
    );
}

#[test]
fn test_manifest_load_report_nested_duplicate_node_key_uses_last_value() {
    let report = load_manifest_report_from_bytes(
        br#"{
            "metadata": {
                "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                "dbt_version": "1.8.0"
            },
            "nodes": {
                "model.proj.same": {
                    "unique_id": "model.proj.same",
                    "name": "same",
                    "resource_type": "operation"
                },
                "model.proj.same": {
                    "unique_id": "model.proj.same",
                    "name": "same",
                    "resource_type": "model"
                }
            }
        }"#,
        Path::new("manifest.json"),
    );
    assert!(!report.has_errors());
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("model.proj.same")
    }));
    assert_eq!(report.manifest.expect("manifest").nodes.len(), 1);
}

#[test]
fn test_known_macro_map_is_not_classified_as_graph_resource() {
    let report = load_manifest_report_from_bytes(
            br#"{
                "metadata": {
                    "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
                    "dbt_version": "1.8.0"
                },
                "nodes": {},
                "macros": {"macro.proj.helper": {"resource_type": "macro"}},
                "functions": {"function.proj.helper": {"name": "helper"}},
                "unit_tests": {
                    "unit_test.proj.check": {"name": "check"},
                    "unit_test.proj.known": {"resource_type": "model"}
                },
                "future_resources": {
                    "future.proj.item": {"resource_type": "future_resource"}
                }
            }"#,
            Path::new("manifest.json"),
        );
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("macro.proj.helper")
    }));
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("function.proj.helper")
            && diagnostic.raw_type.as_deref() == Some("function")
    }));
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("unit_test.proj.check")
            && diagnostic.raw_type.as_deref() == Some("unit_test")
    }));
    assert!(!report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("unit_test.proj.known")
    }));
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.kind == ManifestDiagnosticKind::UnsupportedResourceType
            && diagnostic.raw_resource.as_deref() == Some("future.proj.item")
            && diagnostic.raw_type.as_deref() == Some("future_resource")
    }));
}
