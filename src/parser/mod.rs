pub mod cache;
pub mod columns;
pub mod discovery;
pub mod jinja;
pub mod manifest;
pub mod project;
pub mod sql;
#[allow(dead_code)]
pub mod yaml_schema;

use anyhow::Result;
use serde::de::DeserializeOwned;

/// Parse YAML content tolerating duplicate mapping keys (last value wins).
///
/// If duplicate keys are detected, a warning is emitted (suppressible via `--quiet`)
/// and the content is re-parsed with `DuplicateKeyPolicy::LastWins` to match
/// PyYAML's behavior. The result is deserialized via `serde_json::Value` as an
/// intermediate to avoid serde's struct-level duplicate field rejection.
pub fn yaml_from_str<T: DeserializeOwned>(
    content: &str,
    location: &str,
) -> Result<T> {
    let value: serde_json::Value = match serde_saphyr::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            if is_duplicate_key_error(&e) {
                let key = extract_duplicate_key(&e).unwrap_or("unknown");
                crate::warn!(
                    "duplicate YAML key '{}' in {} (using last value)",
                    key,
                    location,
                );
                let options = serde_saphyr::options::Options {
                    duplicate_keys: serde_saphyr::options::DuplicateKeyPolicy::LastWins,
                    ..Default::default()
                };
                serde_saphyr::from_str_with_options(content, options)?
            } else {
                return Err(e.into());
            }
        }
    };
    if value.is_null() {
        // Empty YAML documents deserialize as null; use Default for the target type.
        return Ok(serde_json::from_value(serde_json::Value::Object(Default::default()))?);
    }
    Ok(serde_json::from_value(value)?)
}

/// Check whether a `serde_saphyr::Error` is a duplicate mapping key error.
fn is_duplicate_key_error(e: &serde_saphyr::Error) -> bool {
    match e {
        serde_saphyr::Error::DuplicateMappingKey { .. } => true,
        serde_saphyr::Error::WithSnippet { error, .. } => is_duplicate_key_error(error),
        _ => false,
    }
}

/// Extract the duplicate key name from a `serde_saphyr::Error`, if available.
fn extract_duplicate_key(e: &serde_saphyr::Error) -> Option<&str> {
    match e {
        serde_saphyr::Error::DuplicateMappingKey { key, .. } => key.as_deref(),
        serde_saphyr::Error::WithSnippet { error, .. } => extract_duplicate_key(error),
        _ => None,
    }
}
