use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::Manifest;

/// Whether a known top-level resource map was absent, empty, or populated.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, serde::Deserialize, PartialEq, Eq)]
pub enum ResourceMapPresence {
    #[default]
    Absent,
    Empty,
    NonEmpty,
}

/// Observations about the shape and capabilities of a manifest artifact.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManifestCapabilities {
    /// Presence is tracked separately from the typed map, since dbt v12 may
    /// emit an empty map and older artifacts may omit the key entirely.
    pub resource_maps: BTreeMap<String, ResourceMapPresence>,
    /// Top-level keys not understood by this version of dlin.
    pub unknown_top_level_keys: BTreeSet<String>,
    /// True when the artifact declares a schema newer than this parser knows.
    pub future_schema: bool,
}

/// A stable, machine-readable diagnostic produced while loading a manifest.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestDiagnosticKind {
    ParseError,
    MissingSchemaVersion,
    InvalidSchemaVersion,
    FutureSchemaVersion,
    MissingDbtVersion,
    InvalidDbtVersion,
    UnknownTopLevelKey,
}

/// Severity of a manifest load diagnostic.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestDiagnosticSeverity {
    Error,
    Warning,
}

/// Diagnostic details retained by [`ManifestLoadReport`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestDiagnostic {
    pub kind: ManifestDiagnosticKind,
    pub severity: ManifestDiagnosticSeverity,
    pub message: String,
    pub hint: Option<String>,
    /// The raw resource key involved in the diagnostic, when applicable.
    pub raw_resource: Option<String>,
    /// The raw schema URI involved in the diagnostic, when applicable.
    pub schema: Option<String>,
}

/// Result of loading a manifest while retaining non-fatal observations.
#[non_exhaustive]
#[derive(Debug, Default)]
pub struct ManifestLoadReport {
    pub manifest: Option<Manifest>,
    pub diagnostics: Vec<ManifestDiagnostic>,
}

impl ManifestLoadReport {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == ManifestDiagnosticSeverity::Error)
    }

    pub fn warnings(&self) -> impl Iterator<Item = &ManifestDiagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == ManifestDiagnosticSeverity::Warning)
    }
}

const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "metadata",
    "nodes",
    "sources",
    "exposures",
    "semantic_models",
    "metrics",
    "saved_queries",
    "macros",
    "docs",
    "groups",
    "group_map",
    "selectors",
    "parent_map",
    "child_map",
    "unit_tests",
    "functions",
    "disabled",
];

const KNOWN_RESOURCE_MAP_KEYS: &[&str] = &[
    "nodes",
    "sources",
    "exposures",
    "semantic_models",
    "metrics",
    "saved_queries",
    "macros",
    "docs",
    "groups",
    "group_map",
    "selectors",
    "parent_map",
    "child_map",
    "unit_tests",
    "functions",
    "disabled",
];

#[derive(Debug, Default)]
struct ManifestObservations {
    unknown_top_level_keys: BTreeSet<String>,
    resource_maps: BTreeMap<String, ResourceMapPresence>,
}

fn observe_value(value: &Value) -> ManifestObservations {
    let Some(object) = value.as_object() else {
        return ManifestObservations::default();
    };

    ManifestObservations {
        unknown_top_level_keys: object
            .keys()
            .filter(|key| !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()))
            .cloned()
            .collect(),
        resource_maps: KNOWN_RESOURCE_MAP_KEYS
            .iter()
            .map(|key| {
                let presence = resource_map_presence(object, key);
                ((*key).to_string(), presence)
            })
            .collect(),
    }
}

fn resource_map_presence(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> ResourceMapPresence {
    match object.get(key) {
        None => ResourceMapPresence::Absent,
        Some(Value::Null) => ResourceMapPresence::Empty,
        Some(Value::Object(map)) if map.is_empty() => ResourceMapPresence::Empty,
        Some(Value::Object(_)) => ResourceMapPresence::NonEmpty,
        Some(_) => ResourceMapPresence::Empty,
    }
}

/// Decode the compatibility API's manifest with one JSON parse and one owned
/// value. The caller maps the serde error to its legacy domain error.
pub(super) fn load_manifest_compat_from_bytes(
    content: &[u8],
) -> std::result::Result<Manifest, serde_json::Error> {
    let value: Value = serde_json::from_slice(content)?;
    let observations = observe_value(&value);
    let mut manifest = serde_json::from_value(value)?;
    enrich_manifest_observations_inner(&mut manifest, observations);
    Ok(manifest)
}

/// Load a manifest and retain non-fatal compatibility diagnostics.
///
/// File-system failures remain `Err`, while malformed JSON and incompatible
/// JSON shapes are represented as `ParseError` diagnostics in the report.
pub fn load_manifest_report(manifest_path: &Path) -> Result<ManifestLoadReport> {
    let content =
        std::fs::read(manifest_path).map_err(|e| crate::error::DbtLineageError::FileReadError {
            path: manifest_path.to_path_buf(),
            source: e,
        })?;
    Ok(load_manifest_report_from_bytes(&content, manifest_path))
}

/// Parse manifest bytes into a report. This function never panics and returns a
/// parse diagnostic for invalid JSON instead of conflating it with warnings.
pub fn load_manifest_report_from_bytes(content: &[u8], manifest_path: &Path) -> ManifestLoadReport {
    let value: Value = match serde_json::from_slice(content) {
        Ok(value) => value,
        Err(error) => {
            return ManifestLoadReport {
                manifest: None,
                diagnostics: vec![ManifestDiagnostic {
                    kind: ManifestDiagnosticKind::ParseError,
                    severity: ManifestDiagnosticSeverity::Error,
                    message: format!(
                        "failed to parse artifact {}: {error}",
                        manifest_path.display()
                    ),
                    hint: Some("Check that the artifact is valid JSON produced by dbt".to_string()),
                    raw_resource: None,
                    schema: None,
                }],
            };
        }
    };

    let Some(object) = value.as_object() else {
        return ManifestLoadReport {
            manifest: None,
            diagnostics: vec![ManifestDiagnostic {
                kind: ManifestDiagnosticKind::ParseError,
                severity: ManifestDiagnosticSeverity::Error,
                message: "manifest artifact must be a JSON object".to_string(),
                hint: Some("Pass a dbt manifest.json object".to_string()),
                raw_resource: None,
                schema: None,
            }],
        };
    };

    let observations = observe_value(&value);
    let mut diagnostics = Vec::new();
    let metadata = object.get("metadata").and_then(Value::as_object);
    let schema_value = metadata.and_then(|metadata| metadata.get("dbt_schema_version"));
    let raw_schema = metadata
        .and_then(|metadata| metadata.get("dbt_schema_version"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let schema_number = raw_schema.as_deref().and_then(parse_schema_number);

    match (schema_value, raw_schema.as_deref()) {
        (Some(value), None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::InvalidSchemaVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!("dbt_schema_version must be a URI string, got {value}"),
            hint: Some(
                "Expected a URI containing a version segment such as /manifest/v12/manifest.json"
                    .to_string(),
            ),
            raw_resource: Some("metadata.dbt_schema_version".to_string()),
            schema: Some(value.to_string()),
        }),
        (None, None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::MissingSchemaVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: "manifest metadata is missing dbt_schema_version".to_string(),
            hint: Some(
                "Generate the artifact with a supported dbt version when possible".to_string(),
            ),
            raw_resource: Some("metadata.dbt_schema_version".to_string()),
            schema: None,
        }),
        (Some(_), Some(schema)) if schema_number.is_none() => diagnostics
            .push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::InvalidSchemaVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!("invalid dbt_schema_version URI: {schema}"),
            hint: Some(
                "Expected a URI containing a version segment such as /manifest/v12/manifest.json"
                    .to_string(),
            ),
            raw_resource: Some("metadata.dbt_schema_version".to_string()),
            schema: Some(schema.to_string()),
        }),
        (Some(_), Some(schema)) if schema_number.is_some_and(|number| number > 12) => diagnostics
            .push(ManifestDiagnostic {
                kind: ManifestDiagnosticKind::FutureSchemaVersion,
                severity: ManifestDiagnosticSeverity::Warning,
                message: format!("manifest uses a future dbt schema version: {schema}"),
                hint: Some(
                    "Some resource types may not be understood by this version of dlin".to_string(),
                ),
                raw_resource: Some("metadata.dbt_schema_version".to_string()),
                schema: Some(schema.to_string()),
            }),
        (Some(_), Some(_)) => {}
        (None, Some(_)) => {}
    }

    let dbt_version_value = metadata.and_then(|metadata| metadata.get("dbt_version"));
    let raw_dbt_version = metadata
        .and_then(|metadata| metadata.get("dbt_version"))
        .and_then(Value::as_str);
    match (dbt_version_value, raw_dbt_version) {
        (Some(value), None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::InvalidDbtVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!("dbt_version must be a string, got {value}"),
            hint: Some("Expected a version such as 1.8.0 or 1.8.0rc1".to_string()),
            raw_resource: Some("metadata.dbt_version".to_string()),
            schema: raw_schema.clone(),
        }),
        (None, None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::MissingDbtVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: "manifest metadata is missing dbt_version".to_string(),
            hint: Some("Generate the artifact with dbt to include producer metadata".to_string()),
            raw_resource: Some("metadata.dbt_version".to_string()),
            schema: raw_schema.clone(),
        }),
        (Some(_), Some(_)) => {}
        (None, Some(_)) => {}
    }

    let unknown_keys = observations.unknown_top_level_keys.clone();
    for key in &unknown_keys {
        diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::UnknownTopLevelKey,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!("unknown top-level manifest key: {key}"),
            hint: Some(
                "The key is retained in Manifest::extra for forward compatibility".to_string(),
            ),
            raw_resource: Some(key.clone()),
            schema: raw_schema.clone(),
        });
    }

    let mut manifest: Manifest = match serde_json::from_value(value) {
        Ok(manifest) => manifest,
        Err(error) => {
            diagnostics.push(ManifestDiagnostic {
                kind: ManifestDiagnosticKind::ParseError,
                severity: ManifestDiagnosticSeverity::Error,
                message: format!("failed to decode manifest resources: {error}"),
                hint: Some(
                    "Check that known resource maps have the shape emitted by dbt".to_string(),
                ),
                raw_resource: None,
                schema: raw_schema,
            });
            return ManifestLoadReport {
                manifest: None,
                diagnostics,
            };
        }
    };
    manifest.metadata.dbt_schema_version_number = schema_number;
    enrich_manifest_observations_inner(&mut manifest, observations);

    ManifestLoadReport {
        manifest: Some(manifest),
        diagnostics,
    }
}

fn parse_schema_number(schema: &str) -> Option<u32> {
    let segments = schema.split('/').collect::<Vec<_>>();
    let (resource, version) = match segments.as_slice() {
        [.., resource, version, filename]
            if *resource == "manifest" && *filename == "manifest.json" =>
        {
            (*resource, *version)
        }
        [.., resource, version_json] if *resource == "manifest" => {
            (*resource, version_json.strip_suffix(".json")?)
        }
        _ => return None,
    };
    if resource != "manifest" {
        return None;
    }
    version
        .strip_prefix('v')
        .filter(|number| {
            !number.is_empty() && number.chars().all(|character| character.is_ascii_digit())
        })
        .and_then(|number| number.parse().ok())
}

fn enrich_manifest_observations_inner(manifest: &mut Manifest, observations: ManifestObservations) {
    manifest.metadata.dbt_schema_version_number = manifest
        .metadata
        .dbt_schema_version
        .as_deref()
        .and_then(parse_schema_number);
    let capabilities = ManifestCapabilities {
        unknown_top_level_keys: observations.unknown_top_level_keys,
        resource_maps: observations.resource_maps,
        future_schema: manifest
            .metadata
            .dbt_schema_version_number
            .is_some_and(|number| number > 12),
        ..ManifestCapabilities::default()
    };
    manifest.capabilities = capabilities;
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let missing =
            load_manifest_report_from_bytes(br#"{"nodes": {}}"#, Path::new("manifest.json"));
        assert!(missing.diagnostics.iter().any(|diagnostic| diagnostic.kind
            == ManifestDiagnosticKind::MissingSchemaVersion
            && diagnostic.severity == ManifestDiagnosticSeverity::Warning));
        assert!(missing.diagnostics.iter().any(|diagnostic| diagnostic.kind
            == ManifestDiagnosticKind::MissingDbtVersion
            && diagnostic.severity == ManifestDiagnosticSeverity::Warning));

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
}
