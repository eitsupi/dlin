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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestDiagnosticKind {
    ParseError,
    MissingSchemaVersion,
    InvalidSchemaVersion,
    FutureSchemaVersion,
    MissingDbtVersion,
    InvalidDbtVersion,
    UnknownTopLevelKey,
    /// A node in the manifest uses a resource kind not represented by the
    /// graph's typed node vocabulary.
    UnsupportedResourceType,
}

impl ManifestDiagnosticKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::MissingSchemaVersion => "missing_schema_version",
            Self::InvalidSchemaVersion => "invalid_schema_version",
            Self::FutureSchemaVersion => "future_schema_version",
            Self::MissingDbtVersion => "missing_dbt_version",
            Self::InvalidDbtVersion => "invalid_dbt_version",
            Self::UnknownTopLevelKey => "unknown_top_level_key",
            Self::UnsupportedResourceType => "unsupported_resource_type",
        }
    }

    /// Whether this diagnostic should be surfaced as a compatibility warning
    /// to command-line and protocol clients. Missing producer metadata is
    /// common in older manifests and remains intentionally quiet.
    pub fn is_user_visible_warning(self) -> bool {
        matches!(
            self,
            Self::InvalidSchemaVersion
                | Self::FutureSchemaVersion
                | Self::InvalidDbtVersion
                | Self::UnknownTopLevelKey
                | Self::UnsupportedResourceType
        )
    }
}

impl std::fmt::Display for ManifestDiagnosticKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Severity of a manifest load diagnostic.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestDiagnosticSeverity {
    Error,
    Warning,
}

impl ManifestDiagnosticSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// Diagnostic details retained by [`ManifestLoadReport`].
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ManifestDiagnostic {
    pub kind: ManifestDiagnosticKind,
    pub severity: ManifestDiagnosticSeverity,
    pub message: String,
    pub hint: Option<String>,
    /// The raw resource key involved in the diagnostic, when applicable.
    pub raw_resource: Option<String>,
    /// The exact resource_type spelling from the artifact, when applicable.
    /// This is kept separately from `raw_resource` (the unique ID/key) so
    /// text, JSON, and MCP clients can preserve the same identity.
    pub raw_type: Option<String>,
    /// The raw schema URI involved in the diagnostic, when applicable.
    pub schema: Option<String>,
}

impl ManifestDiagnostic {
    /// Stable JSON representation shared by CLI warning output and MCP
    /// `warnings` values. Keep the raw resource type separate from the
    /// resource key so clients can reliably group diagnostics.
    pub fn to_warning_json(&self) -> serde_json::Value {
        serde_json::json!({
            "level": self.severity.as_str(),
            "kind": self.kind.as_str(),
            "raw_resource": self.raw_resource,
            "raw_type": self.raw_type,
            "what": self.message,
            "why": serde_json::Value::Null,
            "hint": self.hint,
        })
    }

    /// Stable text representation for human-facing CLI warnings.
    pub fn to_warning_text(&self) -> String {
        let mut text = format!("Warning: [{}] {}", self.kind.as_str(), self.message);
        if let Some(hint) = &self.hint {
            text.push_str(&format!("\n  Hint: {hint}"));
        }
        text
    }
}

/// Result of loading a manifest while retaining non-fatal observations.
#[non_exhaustive]
#[derive(Debug, Default)]
pub struct ManifestLoadReport {
    pub manifest: Option<Manifest>,
    pub diagnostics: Vec<ManifestDiagnostic>,
}

impl ManifestLoadReport {
    /// Create a report from parsed data and the diagnostics collected while
    /// decoding it. This is useful when callers defer graph construction.
    pub fn from_parts(manifest: Option<Manifest>, diagnostics: Vec<ManifestDiagnostic>) -> Self {
        Self {
            manifest,
            diagnostics,
        }
    }

    /// Consume the report without discarding either its parsed manifest or
    /// diagnostics.
    pub fn into_parts(self) -> (Option<Manifest>, Vec<ManifestDiagnostic>) {
        (self.manifest, self.diagnostics)
    }

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

    /// Consume the report and return the parsed manifest, preserving the
    /// original diagnostic message when decoding failed.
    pub fn into_manifest(self) -> Result<Manifest> {
        if let Some(diagnostic) = self
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == ManifestDiagnosticSeverity::Error)
        {
            anyhow::bail!("{}", diagnostic.message);
        }
        self.manifest
            .ok_or_else(|| anyhow::anyhow!("manifest could not be loaded"))
    }

    /// Consume the report and fail when parsing failed or an unsupported
    /// resource type was observed.
    pub fn into_manifest_strict(self) -> Result<Manifest> {
        if self.has_errors() {
            return self.into_manifest();
        }
        if let Some(diagnostic) = self.diagnostics.iter().find(|diagnostic| {
            matches!(
                diagnostic.kind,
                ManifestDiagnosticKind::UnsupportedResourceType
                    | ManifestDiagnosticKind::FutureSchemaVersion
            )
        }) {
            anyhow::bail!("{}", diagnostic.message);
        }
        self.into_manifest()
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

fn diagnose_unsupported_resource_map(
    diagnostics: &mut Vec<ManifestDiagnostic>,
    diagnosed_resources: &mut BTreeSet<(String, String)>,
    map_name: &str,
    resource_map: &serde_json::Map<String, Value>,
    default_resource_type: Option<&str>,
    raw_schema: &Option<String>,
) {
    for (unique_id, resource) in resource_map {
        let raw_type = resource
            .get("resource_type")
            .and_then(Value::as_str)
            .or(default_resource_type);
        let Some(raw_type) = raw_type else {
            continue;
        };
        let super::ManifestResourceType::Unknown(raw_type) =
            super::classify_resource_type(raw_type)
        else {
            continue;
        };
        if !diagnosed_resources.insert((map_name.to_string(), unique_id.clone())) {
            continue;
        }
        diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::UnsupportedResourceType,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!(
                "manifest resource '{unique_id}' in '{map_name}' uses unsupported resource type '{raw_type}'"
            ),
            hint: Some(
                "Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results".to_string(),
            ),
            raw_resource: Some(unique_id.clone()),
            raw_type: Some(raw_type),
            schema: raw_schema.clone(),
        });
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
                    raw_type: None,
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
                raw_type: None,
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
    let schema_number = raw_schema.as_deref().and_then(super::parse_schema_number);

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
            raw_type: None,
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
            raw_type: None,
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
            raw_type: None,
            schema: Some(schema.to_string()),
        }),
        (Some(_), Some(schema))
            if schema_number.is_some_and(|number| {
                number > super::CURRENT_SUPPORTED_MANIFEST_SCHEMA_VERSION
            }) =>
        {
            diagnostics.push(ManifestDiagnostic {
                kind: ManifestDiagnosticKind::FutureSchemaVersion,
                severity: ManifestDiagnosticSeverity::Warning,
                message: format!("manifest uses a future dbt schema version: {schema}"),
                hint: Some(
                    "Some resource types may not be understood by this version of dlin".to_string(),
                ),
                raw_resource: Some("metadata.dbt_schema_version".to_string()),
                raw_type: None,
                schema: Some(schema.to_string()),
            })
        }
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
            raw_type: None,
            schema: raw_schema.clone(),
        }),
        (None, None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::MissingDbtVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: "manifest metadata is missing dbt_version".to_string(),
            hint: Some("Generate the artifact with dbt to include producer metadata".to_string()),
            raw_resource: Some("metadata.dbt_version".to_string()),
            raw_type: None,
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
            raw_type: None,
            schema: raw_schema.clone(),
        });
    }

    // Resource entries may occur in the active graph `nodes` map, known but
    // unsupported `functions`/`unit_tests` maps, or a future top-level map.
    // Do not scan every object-valued map: macros/docs/groups/selectors also
    // contain arbitrary `resource_type` fields which are not graph resources.
    let mut diagnosed_resources = BTreeSet::new();
    let mut resource_maps = Vec::new();
    if let Some(resource_map) = object.get("nodes").and_then(Value::as_object) {
        resource_maps.push(("nodes", resource_map));
    }
    if let Some(resource_map) = object.get("functions").and_then(Value::as_object) {
        diagnose_unsupported_resource_map(
            &mut diagnostics,
            &mut diagnosed_resources,
            "functions",
            resource_map,
            Some("function"),
            &raw_schema,
        );
    }
    if let Some(resource_map) = object.get("unit_tests").and_then(Value::as_object) {
        diagnose_unsupported_resource_map(
            &mut diagnostics,
            &mut diagnosed_resources,
            "unit_tests",
            resource_map,
            Some("unit_test"),
            &raw_schema,
        );
    }
    for key in &unknown_keys {
        if let Some(resource_map) = object.get(key).and_then(Value::as_object) {
            resource_maps.push((key.as_str(), resource_map));
        }
    }
    for (map_name, resource_map) in resource_maps {
        diagnose_unsupported_resource_map(
            &mut diagnostics,
            &mut diagnosed_resources,
            map_name,
            resource_map,
            None,
            &raw_schema,
        );
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
                raw_type: None,
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

fn enrich_manifest_observations_inner(manifest: &mut Manifest, observations: ManifestObservations) {
    manifest.metadata.dbt_schema_version_number = manifest
        .metadata
        .dbt_schema_version
        .as_deref()
        .and_then(super::parse_schema_number);
    let capabilities = ManifestCapabilities {
        unknown_top_level_keys: observations.unknown_top_level_keys,
        resource_maps: observations.resource_maps,
        future_schema: super::manifest_has_future_schema(manifest),
        ..ManifestCapabilities::default()
    };
    manifest.capabilities = capabilities;
}

#[cfg(test)]
mod tests {
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
}
