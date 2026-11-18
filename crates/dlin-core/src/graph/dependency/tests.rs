use super::*;
use serde_json::json;

fn site() -> ExtractionSite {
    ExtractionSite {
        line: Some(3),
        expr: Some("kind_x_relation()".to_string()),
    }
}

fn node_site() -> NodeCallSite {
    site().at_node("model.downstream", "models/downstream.sql")
}

fn every_reason() -> Vec<ExtractedDependencyReason> {
    use DependencyReason as R;
    vec![
        R::UnresolvedRef {
            name: "a".into(),
            package: Some("pkg".into()),
            version: Some("2".into()),
            at: site(),
        },
        R::UnresolvedSource {
            source: "raw".into(),
            table: "orders".into(),
            at: site(),
        },
        R::UnresolvedVar {
            name: None,
            at: site(),
        },
        R::UnsupportedDispatch {
            name: Some("kind_x_relation".into()),
            namespace: None,
            at: site(),
        },
        R::UnsupportedMacroNamespace {
            namespace: "dbt_utils".into(),
            macro_name: "star".into(),
            at: site(),
        },
        R::UnresolvedCall {
            callee: Some("unknown_macro".into()),
            at: site(),
        },
        R::UnsupportedJinjaConstruct {
            construct: "filter:custom".into(),
            at: site(),
        },
        R::InvalidDependencyCall {
            function: DependencyFunction::Source,
            at: site(),
        },
        R::AnalysisLimitReached {
            limit: AnalysisLimit::RecursionDepth,
            at: site(),
        },
        R::RuntimeValueUnknown {
            source: RuntimeValueSource::TargetAttribute,
            name: Some("target.name".into()),
            at: site(),
        },
        R::JinjaRenderFailed { at: site() },
        R::MacroReachabilityUnknown {
            macro_name: None,
            at: site(),
        },
        R::UnsupportedDependencyExpression {
            function: Some("source".into()),
            at: site(),
        },
        R::DependencyTargetMissing {
            name: "gone".into(),
            at: site(),
        },
        R::UnsupportedRefPackage {
            package: "pkg".into(),
            name: "a".into(),
            at: site(),
        },
        R::AmbiguousRef {
            name: "a".into(),
            candidates: vec!["model.a".into(), "seed.a".into()],
            at: site(),
        },
        R::NotAssessed {
            scope: NotAssessedScope::SemanticDependencies,
        },
    ]
}

#[test]
fn every_reason_serialises_with_its_code_as_type() {
    for reason in every_reason() {
        let value = serde_json::to_value(&reason).unwrap();
        assert_eq!(value["type"], reason.code(), "{value}");
        let round_trip: ExtractedDependencyReason = serde_json::from_value(value).unwrap();
        assert_eq!(round_trip, reason);
    }
}

#[test]
fn map_site_keeps_every_field_and_replaces_only_the_location() {
    for reason in every_reason() {
        let code = reason.code();
        let had_site = reason.at().is_some();
        let mapped = reason
            .clone()
            .map_site(|at| at.at_node("model.downstream", "models/downstream.sql"));
        assert_eq!(mapped.code(), code);
        assert_eq!(mapped.at().is_some(), had_site);
        if let Some(at) = mapped.at() {
            assert_eq!(at, &node_site());
        }
        // Every field other than `at` survives the conversion.
        let mut before = serde_json::to_value(&reason).unwrap();
        let mut after = serde_json::to_value(&mapped).unwrap();
        before.as_object_mut().unwrap().remove("at");
        after.as_object_mut().unwrap().remove("at");
        assert_eq!(before, after);
    }
}

#[test]
fn node_reason_json_shape() {
    let reason = NodeDependencyReason::UnsupportedMacroNamespace {
        namespace: "dbt_utils".into(),
        macro_name: "generate_surrogate_key".into(),
        at: node_site(),
    };
    assert_eq!(
        serde_json::to_value(&reason).unwrap(),
        json!({
            "type": "unsupported_macro_namespace",
            "namespace": "dbt_utils",
            "macro": "generate_surrogate_key",
            "at": {
                "unique_id": "model.downstream",
                "path": "models/downstream.sql",
                "line": 3,
                "expr": "kind_x_relation()"
            }
        })
    );
}

#[test]
fn not_assessed_has_no_location() {
    let reason = NodeDependencyReason::NotAssessed {
        scope: NotAssessedScope::PhantomDependencies,
    };
    assert_eq!(
        serde_json::to_value(&reason).unwrap(),
        json!({"type": "not_assessed", "scope": "phantom_dependencies"})
    );
    assert!(reason.at().is_none());
}

#[test]
fn phantom_scope_is_never_a_project_scope() {
    assert_eq!(NotAssessedScope::PhantomDependencies.project_scope(), None);
    assert_eq!(
        NotAssessedScope::ManifestDependencies.project_scope(),
        Some(ProjectScope::ManifestDependencies)
    );
    assert_eq!(
        NotAssessedScope::SemanticDependencies.project_scope(),
        Some(ProjectScope::SemanticDependencies)
    );
    // The project scope enum cannot be deserialised into the phantom scope.
    assert!(
        serde_json::from_value::<ProjectGap>(json!({
            "type": "not_assessed",
            "scope": "phantom_dependencies"
        }))
        .is_err()
    );
}

#[test]
fn analysis_status_follows_reasons() {
    let complete = DependencyAnalysis::new([]);
    assert_eq!(complete.status(), DependencyAnalysisStatus::Complete);
    assert_eq!(
        serde_json::to_value(&complete).unwrap(),
        json!({"status": "complete", "unresolved": []})
    );

    let reason = NodeDependencyReason::JinjaRenderFailed { at: node_site() };
    let mut partial = DependencyAnalysis::new([reason.clone(), reason.clone()]);
    assert_eq!(partial.status(), DependencyAnalysisStatus::Partial);
    assert_eq!(partial.unresolved().len(), 1, "duplicates are removed");
    partial.extend([reason]);
    assert_eq!(partial.unresolved().len(), 1);

    let value = serde_json::to_value(&partial).unwrap();
    assert_eq!(value["status"], "partial");
    let round_trip: DependencyAnalysis = serde_json::from_value(value).unwrap();
    assert_eq!(round_trip, partial);
}

#[test]
fn analysis_rejects_inconsistent_status() {
    assert!(
        serde_json::from_value::<DependencyAnalysis>(json!({
            "status": "partial",
            "unresolved": []
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<DependencyAnalysis>(json!({
            "status": "complete",
            "unresolved": [{"type": "not_assessed", "scope": "manifest_dependencies"}]
        }))
        .is_err()
    );
}

#[test]
fn analysis_orders_reasons_deterministically() {
    let a = NodeDependencyReason::UnresolvedVar {
        name: Some("a".into()),
        at: node_site(),
    };
    let b = NodeDependencyReason::NotAssessed {
        scope: NotAssessedScope::ManifestDependencies,
    };
    assert_eq!(
        DependencyAnalysis::new([a.clone(), b.clone()]),
        DependencyAnalysis::new([b, a])
    );
}

#[test]
fn project_gap_json_shape() {
    let gap = ProjectGap::InputSkipped {
        location: InputLocation {
            path: "models/schema.yml".into(),
            line: None,
        },
        kind: InputKind::Yaml,
        stage: InputStage::Parse,
        failure: InputFailure::ParseError,
    };
    assert_eq!(
        serde_json::to_value(&gap).unwrap(),
        json!({
            "type": "input_skipped",
            "location": {"path": "models/schema.yml", "line": null},
            "kind": "yaml",
            "stage": "parse",
            "failure": "parse_error"
        })
    );
    assert_eq!(gap.code(), "input_skipped");
}

#[test]
fn config_diagnostic_json_shape_and_level() {
    let diagnostic = ConfigDiagnostic::ConfiguredInputPathMissing {
        paths: vec![SettingPath {
            setting: PathSetting::ModelPaths,
            path: "../shared/models".into(),
        }],
        all_model_paths_missing: true,
    };
    assert_eq!(
        serde_json::to_value(&diagnostic).unwrap(),
        json!({
            "type": "configured_input_path_missing",
            "paths": [{"setting": "model-paths", "path": "../shared/models"}],
            "all_model_paths_missing": true
        })
    );
    assert_eq!(diagnostic.level(), ConfigDiagnosticLevel::Warning);
    let info = ConfigDiagnostic::DefaultInputPathMissing {
        setting: PathSetting::AnalysisPaths,
        path: "analyses".into(),
    };
    assert_eq!(info.level(), ConfigDiagnosticLevel::Info);
    assert_eq!(info.code(), "default_input_path_missing");
}

#[test]
fn input_failure_classifies_io_errors() {
    use std::io::{Error, ErrorKind};
    assert_eq!(
        InputFailure::from_io_error(&Error::from(ErrorKind::PermissionDenied)),
        InputFailure::PermissionDenied
    );
    assert_eq!(
        InputFailure::from_io_error(&Error::from(ErrorKind::NotFound)),
        InputFailure::NotFound
    );
    assert_eq!(
        InputFailure::from_io_error(&Error::from(ErrorKind::InvalidData)),
        InputFailure::IoError
    );
    assert_eq!(
        InputFailure::from_io_error(&Error::from(ErrorKind::Interrupted)),
        InputFailure::IoError
    );
}

#[cfg(unix)]
#[test]
fn relative_display_path_inside_and_outside_base() {
    let base = Path::new("/work/project");
    assert_eq!(
        relative_display_path(Path::new("/work/project/models/a.sql"), base),
        "models/a.sql"
    );
    assert_eq!(
        relative_display_path(Path::new("/work/shared/models"), base),
        "../shared/models"
    );
    assert_eq!(relative_display_path(Path::new("/work/project"), base), ".");
    assert_eq!(
        relative_display_path(Path::new("/other"), base),
        "../../other"
    );
}

#[cfg(windows)]
#[test]
fn relative_display_path_on_windows() {
    let base = Path::new(r"C:\work\project");
    assert_eq!(
        relative_display_path(Path::new(r"C:\work\project\models\a.sql"), base),
        "models/a.sql"
    );
    assert_eq!(
        relative_display_path(Path::new(r"C:\work\shared\models"), base),
        "../shared/models"
    );
    // Another drive or a UNC share has no relative form.
    assert_eq!(
        relative_display_path(Path::new(r"D:\shared\models"), base),
        "D:/shared/models"
    );
    assert_eq!(
        relative_display_path(Path::new(r"\\server\share\models"), base),
        "//server/share/models"
    );
}
