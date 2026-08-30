use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::de::Error as DeError;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::super::{Manifest, ManifestNode, ManifestResourceType, classify_resource_type};
use super::{KNOWN_RESOURCE_MAP_KEYS, KNOWN_TOP_LEVEL_KEYS, ResourceMapPresence};

#[derive(Debug, Default)]
pub(super) struct ManifestObservations {
    pub(super) unknown_top_level_keys: BTreeSet<String>,
    pub(super) resource_maps: BTreeMap<String, ResourceMapPresence>,
    pub(super) metadata: MetadataObservations,
    pub(super) unsupported_resources: BTreeMap<String, Vec<(String, String)>>,
}

#[derive(Debug, Default)]
pub(super) struct MetadataObservations {
    pub(super) schema: ObservedField,
    pub(super) dbt_version: ObservedField,
}

#[derive(Debug, Default)]
pub(super) enum ObservedField {
    #[default]
    Missing,
    Null,
    String(String),
    Other(&'static str),
}

impl ObservedField {
    pub(super) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub(super) fn display(&self) -> String {
        match self {
            Self::Missing | Self::Null => "null".to_string(),
            Self::String(value) => {
                serde_json::to_string(value).unwrap_or_else(|_| "\"<invalid string>\"".to_string())
            }
            Self::Other(kind) => (*kind).to_string(),
        }
    }
}

fn observed_field(value: Option<&Value>) -> ObservedField {
    match value {
        None => ObservedField::Missing,
        Some(Value::Null) => ObservedField::Null,
        Some(Value::String(value)) => ObservedField::String(value.clone()),
        Some(Value::Bool(_)) => ObservedField::Other("boolean"),
        Some(Value::Number(_)) => ObservedField::Other("number"),
        Some(Value::Object(_)) => ObservedField::Other("object"),
        Some(Value::Array(_)) => ObservedField::Other("array"),
    }
}

fn metadata_observations(value: &Value) -> MetadataObservations {
    let Some(metadata) = value.as_object() else {
        return MetadataObservations::default();
    };
    MetadataObservations {
        schema: observed_field(metadata.get("dbt_schema_version")),
        dbt_version: observed_field(metadata.get("dbt_version")),
    }
}

fn map_presence(length: usize) -> ResourceMapPresence {
    if length == 0 {
        ResourceMapPresence::Empty
    } else {
        ResourceMapPresence::NonEmpty
    }
}

fn inspect_resource_values(
    map_name: &str,
    values: &HashMap<String, Value>,
    observations: &mut ManifestObservations,
) {
    let default_type = match map_name {
        "functions" => Some("function"),
        "unit_tests" => Some("unit_test"),
        _ => None,
    };
    let resources = observations
        .unsupported_resources
        .entry(map_name.to_string())
        .or_default();
    for (unique_id, value) in values {
        let raw_type = value
            .as_object()
            .and_then(|object| object.get("resource_type"))
            .and_then(Value::as_str)
            .or(default_type);
        if let Some(raw_type) = raw_type
            && matches!(
                classify_resource_type(raw_type),
                ManifestResourceType::Unknown(_)
            )
        {
            resources.push((unique_id.clone(), raw_type.to_string()));
        }
    }
}

fn inspect_nodes(values: &HashMap<String, ManifestNode>, observations: &mut ManifestObservations) {
    let resources = observations
        .unsupported_resources
        .entry("nodes".to_string())
        .or_default();
    for (unique_id, node) in values {
        if matches!(
            classify_resource_type(&node.resource_type),
            ManifestResourceType::Unknown(_)
        ) {
            resources.push((unique_id.clone(), node.resource_type.clone()));
        }
    }
}

fn inspect_unknown_map(map_name: &str, value: &Value, observations: &mut ManifestObservations) {
    let Some(values) = value.as_object() else {
        return;
    };
    let resources = observations
        .unsupported_resources
        .entry(map_name.to_string())
        .or_default();
    for (unique_id, value) in values {
        let Some(raw_type) = value
            .as_object()
            .and_then(|object| object.get("resource_type"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if matches!(
            classify_resource_type(raw_type),
            ManifestResourceType::Unknown(_)
        ) {
            resources.push((unique_id.clone(), raw_type.to_string()));
        }
    }
}

struct ManifestVisitor<'a> {
    observations: &'a mut ManifestObservations,
}

pub(super) struct DecodeFailure {
    pub(super) error: String,
    pub(super) observations: ManifestObservations,
}

pub(super) type DecodedManifest = (Manifest, ManifestObservations);
pub(super) type DecodeOutcome = Option<Result<DecodedManifest, DecodeFailure>>;

impl<'de> Visitor<'de> for ManifestVisitor<'_> {
    type Value = Option<Manifest>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("manifest JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut manifest = Manifest::default();
        let observations = &mut *self.observations;
        let mut seen = BTreeSet::new();

        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) && KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) {
                return Err(A::Error::custom(format!("duplicate field `{key}`")));
            }
            match key.as_str() {
                "metadata" => {
                    let value = map.next_value::<Value>()?;
                    observations.metadata = metadata_observations(&value);
                    manifest.metadata = match serde_json::from_value(value) {
                        Ok(value) => value,
                        Err(error) => {
                            return Err(A::Error::custom(error.to_string()));
                        }
                    };
                }
                "nodes" => {
                    manifest.nodes = map.next_value::<HashMap<_, _>>()?;
                    inspect_nodes(&manifest.nodes, observations);
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.nodes.len()));
                }
                "sources" => {
                    manifest.sources = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.sources.len()));
                }
                "exposures" => {
                    manifest.exposures = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.exposures.len()));
                }
                "semantic_models" => {
                    manifest.semantic_models = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.semantic_models.len()));
                }
                "metrics" => {
                    manifest.metrics = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.metrics.len()));
                }
                "saved_queries" => {
                    manifest.saved_queries = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.saved_queries.len()));
                }
                "macros" => {
                    manifest.macros = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.macros.len()));
                }
                "docs" => {
                    manifest.docs = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.docs.len()));
                }
                "groups" => {
                    manifest.groups = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.groups.len()));
                }
                "group_map" => {
                    manifest.group_map = map.next_value::<Option<HashMap<_, _>>>()?;
                    observations.resource_maps.insert(
                        key,
                        map_presence(manifest.group_map.as_ref().map_or(0, HashMap::len)),
                    );
                }
                "selectors" => {
                    manifest.selectors = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key, map_presence(manifest.selectors.len()));
                }
                "parent_map" => {
                    manifest.parent_map = map.next_value::<Option<HashMap<_, _>>>()?;
                    observations.resource_maps.insert(
                        key,
                        map_presence(manifest.parent_map.as_ref().map_or(0, HashMap::len)),
                    );
                }
                "child_map" => {
                    manifest.child_map = map.next_value::<Option<HashMap<_, _>>>()?;
                    observations.resource_maps.insert(
                        key,
                        map_presence(manifest.child_map.as_ref().map_or(0, HashMap::len)),
                    );
                }
                "unit_tests" => {
                    manifest.unit_tests = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key.clone(), map_presence(manifest.unit_tests.len()));
                    inspect_resource_values(&key, &manifest.unit_tests, observations);
                }
                "functions" => {
                    manifest.functions = map.next_value::<HashMap<_, _>>()?;
                    observations
                        .resource_maps
                        .insert(key.clone(), map_presence(manifest.functions.len()));
                    inspect_resource_values(&key, &manifest.functions, observations);
                }
                "disabled" => {
                    manifest.disabled = map.next_value::<Option<HashMap<_, _>>>()?;
                    observations.resource_maps.insert(
                        key,
                        map_presence(manifest.disabled.as_ref().map_or(0, HashMap::len)),
                    );
                }
                _ => {
                    observations.unknown_top_level_keys.insert(key.clone());
                    let value = map.next_value::<Value>()?;
                    inspect_unknown_map(&key, &value, observations);
                    manifest.extra.insert(key, value);
                }
            }
        }
        manifest_field_exhaustiveness_guard(&manifest);
        Ok(Some(manifest))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<de::IgnoredAny>()?.is_some() {}
        Ok(None)
    }

    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_str<E>(self, _: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_string<E>(self, _: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }
}

/// Keep the hand-written visitor synchronized with [`Manifest`]. The
/// exhaustive pattern intentionally omits `..`, so adding a Manifest field
/// requires updating this guard and the visitor together.
fn manifest_field_exhaustiveness_guard(manifest: &Manifest) {
    let Manifest {
        metadata,
        nodes,
        sources,
        exposures,
        semantic_models,
        metrics,
        saved_queries,
        macros,
        docs,
        groups,
        group_map,
        selectors,
        parent_map,
        child_map,
        unit_tests,
        functions,
        disabled,
        extra,
        capabilities,
    } = manifest;
    let _ = (
        metadata,
        nodes,
        sources,
        exposures,
        semantic_models,
        metrics,
        saved_queries,
        macros,
        docs,
        groups,
        group_map,
        selectors,
        parent_map,
        child_map,
        unit_tests,
        functions,
        disabled,
        extra,
        capabilities,
    );
}

pub(super) fn decode_manifest(
    content: &[u8],
) -> std::result::Result<DecodeOutcome, serde_json::Error> {
    let mut observations = ManifestObservations {
        resource_maps: KNOWN_RESOURCE_MAP_KEYS
            .iter()
            .map(|key| ((*key).to_string(), ResourceMapPresence::Absent))
            .collect(),
        ..ManifestObservations::default()
    };
    let mut deserializer = serde_json::Deserializer::from_slice(content);
    let decoded = serde::de::Deserializer::deserialize_any(
        &mut deserializer,
        ManifestVisitor {
            observations: &mut observations,
        },
    );
    let manifest = match decoded {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            deserializer.end()?;
            return Ok(None);
        }
        Err(error) => {
            if error.classify() == serde_json::error::Category::Data {
                return Ok(Some(Err(DecodeFailure {
                    error: error.to_string(),
                    observations,
                })));
            }
            return Err(error);
        }
    };
    deserializer.end()?;
    Ok(Some(Ok((manifest, observations))))
}
