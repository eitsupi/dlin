use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::Manifest;
use anyhow::Result;

mod decode;
use decode::{ManifestObservations, ObservedField, decode_manifest};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// Decode the compatibility API's manifest in one pass without constructing a
/// generic JSON tree. The decoder retains only the shape information needed to
/// populate compatibility capabilities and diagnostics.
pub(super) fn load_manifest_compat_from_bytes(
    content: &[u8],
) -> std::result::Result<Manifest, serde_json::Error> {
    let decoded = decode_manifest(content)?.ok_or_else(|| {
        <serde_json::Error as serde::de::Error>::custom("manifest artifact must be a JSON object")
    })?;
    let (mut manifest, observations) = decoded
        .map_err(|failure| <serde_json::Error as serde::de::Error>::custom(failure.error))?;
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
    let (mut manifest, mut observations, decode_error) = match decode_manifest(content) {
        Ok(Some(Ok((manifest, observations)))) => (manifest, observations, None),
        Ok(Some(Err(failure))) => (
            Manifest::default(),
            failure.observations,
            Some(failure.error),
        ),
        Ok(None) => {
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
        }
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
    for resources in observations.unsupported_resources.values_mut() {
        resources.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    }

    let mut diagnostics = Vec::new();
    let raw_schema = observations.metadata.schema.as_str().map(ToOwned::to_owned);
    let schema_number = raw_schema.as_deref().and_then(super::parse_schema_number);

    match (&observations.metadata.schema, raw_schema.as_deref()) {
        (ObservedField::Other(_) | ObservedField::Null, None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::InvalidSchemaVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!(
                "dbt_schema_version must be a URI string, got {}",
                observations.metadata.schema.display()
            ),
            hint: Some(
                "Expected a URI containing a version segment such as /manifest/v12/manifest.json"
                    .to_string(),
            ),
            raw_resource: Some("metadata.dbt_schema_version".to_string()),
            raw_type: None,
            schema: Some(observations.metadata.schema.display()),
        }),
        (ObservedField::Missing, None) => diagnostics.push(ManifestDiagnostic {
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
        (ObservedField::String(_), Some(schema)) if schema_number.is_none() => diagnostics
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
        (ObservedField::String(_), Some(schema))
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
        (ObservedField::String(_), Some(_)) => {}
        (ObservedField::Missing, Some(_))
        | (ObservedField::Null, Some(_))
        | (ObservedField::Other(_), Some(_)) => {}
        (ObservedField::String(_), None) => {}
    }

    let raw_dbt_version = observations.metadata.dbt_version.as_str();
    match (&observations.metadata.dbt_version, raw_dbt_version) {
        (ObservedField::Other(_) | ObservedField::Null, None) => {
            diagnostics.push(ManifestDiagnostic {
                kind: ManifestDiagnosticKind::InvalidDbtVersion,
                severity: ManifestDiagnosticSeverity::Warning,
                message: format!(
                    "dbt_version must be a string, got {}",
                    observations.metadata.dbt_version.display()
                ),
                hint: Some("Expected a version such as 1.8.0 or 1.8.0rc1".to_string()),
                raw_resource: Some("metadata.dbt_version".to_string()),
                raw_type: None,
                schema: raw_schema.clone(),
            })
        }
        (ObservedField::Missing, None) => diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::MissingDbtVersion,
            severity: ManifestDiagnosticSeverity::Warning,
            message: "manifest metadata is missing dbt_version".to_string(),
            hint: Some("Generate the artifact with dbt to include producer metadata".to_string()),
            raw_resource: Some("metadata.dbt_version".to_string()),
            raw_type: None,
            schema: raw_schema.clone(),
        }),
        (ObservedField::String(_), Some(_)) => {}
        (ObservedField::Missing, Some(_))
        | (ObservedField::Null, Some(_))
        | (ObservedField::Other(_), Some(_)) => {}
        (ObservedField::String(_), None) => {}
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
    // The decoder inspects only these maps for unsupported resources; generic
    // maps such as macros/docs/groups/selectors are intentionally ignored.
    let mut diagnosed_resources = BTreeSet::new();
    append_unsupported_resource_diagnostics(
        "functions",
        Some("function"),
        &observations,
        &mut diagnostics,
        &mut diagnosed_resources,
        raw_schema.as_deref(),
    );
    append_unsupported_resource_diagnostics(
        "unit_tests",
        Some("unit_test"),
        &observations,
        &mut diagnostics,
        &mut diagnosed_resources,
        raw_schema.as_deref(),
    );

    let mut node_ids = manifest.nodes.keys().collect::<Vec<_>>();
    node_ids.sort_unstable();
    for unique_id in node_ids {
        let node = &manifest.nodes[unique_id];
        if !matches!(
            super::classify_resource_type(&node.resource_type),
            super::ManifestResourceType::Unknown(_)
        ) || !diagnosed_resources.insert(("nodes".to_string(), unique_id.to_string()))
        {
            continue;
        }
        diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::UnsupportedResourceType,
            severity: ManifestDiagnosticSeverity::Warning,
            message: format!(
                "manifest resource '{unique_id}' in 'nodes' uses unsupported resource type '{}'",
                node.resource_type
            ),
            hint: Some(
                "Upgrade dlin when support for this dbt resource type is available; the resource will be omitted from graph results".to_string(),
            ),
            raw_resource: Some(unique_id.to_string()),
            raw_type: Some(node.resource_type.clone()),
            schema: raw_schema.clone(),
        });
    }
    // Keep diagnostics for future resource maps after the active graph's
    // node diagnostics. This preserves the established warning order while
    // still allowing the decoder to avoid retaining those entries separately.
    for key in &unknown_keys {
        append_unsupported_resource_diagnostics(
            key,
            None,
            &observations,
            &mut diagnostics,
            &mut diagnosed_resources,
            raw_schema.as_deref(),
        );
    }
    if let Some(error) = decode_error {
        diagnostics.push(ManifestDiagnostic {
            kind: ManifestDiagnosticKind::ParseError,
            severity: ManifestDiagnosticSeverity::Error,
            message: format!("failed to decode manifest resources: {error}"),
            hint: Some("Check that known resource maps have the shape emitted by dbt".to_string()),
            raw_resource: None,
            raw_type: None,
            schema: raw_schema,
        });
        return ManifestLoadReport {
            manifest: None,
            diagnostics,
        };
    }
    manifest.metadata.dbt_schema_version_number = schema_number;
    enrich_manifest_observations_inner(&mut manifest, observations);

    ManifestLoadReport {
        manifest: Some(manifest),
        diagnostics,
    }
}

fn append_unsupported_resource_diagnostics(
    map_name: &str,
    default_resource_type: Option<&str>,
    observations: &ManifestObservations,
    diagnostics: &mut Vec<ManifestDiagnostic>,
    diagnosed_resources: &mut BTreeSet<(String, String)>,
    schema: Option<&str>,
) {
    for (unique_id, resource_type) in observations
        .unsupported_resources
        .get(map_name)
        .into_iter()
        .flatten()
    {
        let raw_type = if resource_type.is_empty() {
            default_resource_type.unwrap_or(resource_type)
        } else {
            resource_type.as_str()
        };
        if !matches!(
            super::classify_resource_type(raw_type),
            super::ManifestResourceType::Unknown(_)
        ) {
            continue;
        }
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
            raw_type: Some(raw_type.to_string()),
            schema: schema.map(ToOwned::to_owned),
        });
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
mod tests;
