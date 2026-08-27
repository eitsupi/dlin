use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use super::Manifest;

/// Whether a known top-level resource map was absent, empty, or populated.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize, PartialEq, Eq)]
pub enum ResourceMapPresence {
    #[default]
    Absent,
    Empty,
    NonEmpty,
}

/// Observations about the shape and capabilities of a manifest artifact.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestDiagnosticSeverity {
    Error,
    Warning,
}

/// Diagnostic details retained by [`ManifestLoadReport`].
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

/// Populate compatibility observations for the already decoded manifest.
pub(super) fn enrich_manifest_observations(manifest: &mut Manifest, content: &[u8]) {
    let value = serde_json::from_slice::<Value>(content).ok();
    let top_level_object = value.as_ref().and_then(Value::as_object);
    let unknown_keys = top_level_object
        .map(|object| {
            object
                .keys()
                .filter(|key| !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    enrich_manifest_observations_inner(manifest, unknown_keys, top_level_object);
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
        (Some(_), Some(version)) if !is_valid_dbt_version(version) => {
            diagnostics.push(ManifestDiagnostic {
                kind: ManifestDiagnosticKind::InvalidDbtVersion,
                severity: ManifestDiagnosticSeverity::Warning,
                message: format!("invalid dbt_version string: {version}"),
                hint: Some("Expected a version such as 1.8.0 or 1.8.0rc1".to_string()),
                raw_resource: Some("metadata.dbt_version".to_string()),
                schema: raw_schema.clone(),
            })
        }
        (Some(_), Some(_)) => {}
        (None, Some(_)) => {}
    }

    let unknown_keys = object
        .keys()
        .filter(|key| !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
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

    let mut manifest: Manifest = match serde_json::from_value(value.clone()) {
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
    enrich_manifest_observations_inner(&mut manifest, unknown_keys, Some(object));

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

fn is_valid_dbt_version(version: &str) -> bool {
    let (core, build) = version
        .split_once('+')
        .map_or((version, None), |(core, build)| (core, Some(build)));
    if build.is_some_and(|build| build.is_empty()) {
        return false;
    }
    let (base, hyphen_suffix) = core
        .split_once('-')
        .map_or((core, None), |(base, suffix)| (base, Some(suffix)));
    if hyphen_suffix.is_some_and(|suffix| !is_valid_version_suffix(suffix)) {
        return false;
    }
    let mut components = base.split('.');
    let major = components.next();
    let minor = components.next();
    let patch = components.next();
    if components.next().is_some()
        || !major.is_some_and(is_numeric_component)
        || !minor.is_some_and(is_numeric_component)
    {
        return false;
    }
    let Some(patch) = patch else { return false };
    let digits = patch
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .collect::<String>();
    let suffix = &patch[digits.len()..];
    !digits.is_empty() && is_valid_version_suffix(suffix)
}

fn is_numeric_component(component: &str) -> bool {
    !component.is_empty()
        && component
            .chars()
            .all(|character| character.is_ascii_digit())
}

fn is_valid_version_suffix(suffix: &str) -> bool {
    if suffix.is_empty() {
        return true;
    }
    let prefix = ["dev", "rc", "a", "b"]
        .into_iter()
        .find(|prefix| suffix.starts_with(prefix));
    let Some(prefix) = prefix else { return false };
    let number = &suffix[prefix.len()..];
    number.is_empty() || is_numeric_component(number)
}

fn enrich_manifest_observations_inner(
    manifest: &mut Manifest,
    unknown_top_level_keys: BTreeSet<String>,
    top_level_object: Option<&serde_json::Map<String, Value>>,
) {
    manifest.metadata.dbt_schema_version_number = manifest
        .metadata
        .dbt_schema_version
        .as_deref()
        .and_then(parse_schema_number);
    let mut capabilities = ManifestCapabilities {
        unknown_top_level_keys,
        future_schema: manifest
            .metadata
            .dbt_schema_version_number
            .is_some_and(|number| number > 12),
        ..ManifestCapabilities::default()
    };
    for key in KNOWN_RESOURCE_MAP_KEYS {
        let presence = match *key {
            "nodes" => map_presence(&manifest.nodes, top_level_object, key),
            "sources" => map_presence(&manifest.sources, top_level_object, key),
            "exposures" => map_presence(&manifest.exposures, top_level_object, key),
            "semantic_models" => map_presence(&manifest.semantic_models, top_level_object, key),
            "metrics" => map_presence(&manifest.metrics, top_level_object, key),
            "saved_queries" => map_presence(&manifest.saved_queries, top_level_object, key),
            "macros" => map_presence(&manifest.macros, top_level_object, key),
            "docs" => map_presence(&manifest.docs, top_level_object, key),
            "groups" => map_presence(&manifest.groups, top_level_object, key),
            "group_map" => map_presence(&manifest.group_map, top_level_object, key),
            "selectors" => map_presence(&manifest.selectors, top_level_object, key),
            "parent_map" => map_presence(&manifest.parent_map, top_level_object, key),
            "child_map" => map_presence(&manifest.child_map, top_level_object, key),
            "unit_tests" => map_presence(&manifest.unit_tests, top_level_object, key),
            "functions" => map_presence(&manifest.functions, top_level_object, key),
            "disabled" => map_presence(&manifest.disabled, top_level_object, key),
            _ => ResourceMapPresence::Absent,
        };
        capabilities
            .resource_maps
            .insert((*key).to_string(), presence);
    }
    manifest.capabilities = capabilities;
}

fn map_presence<T>(
    map: &HashMap<String, T>,
    top_level_object: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> ResourceMapPresence {
    if top_level_object.is_some_and(|object| !object.contains_key(key)) {
        return ResourceMapPresence::Absent;
    }
    if map.is_empty() {
        ResourceMapPresence::Empty
    } else {
        ResourceMapPresence::NonEmpty
    }
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
                    "dbt_version": "1.8.garbage"
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
        assert!(!is_valid_dbt_version("1.8"));
        assert!(!is_valid_dbt_version("1.8.garbage"));
        assert!(is_valid_dbt_version("1.8.7rc1"));
        assert!(is_valid_dbt_version("1.8.7+build.1"));
    }
}
