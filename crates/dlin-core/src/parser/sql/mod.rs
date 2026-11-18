pub(crate) mod recovery;

/// A reference to another dbt model via ref()
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RefCall {
    /// Optional package name (for cross-project refs)
    pub package: Option<String>,
    /// Model name
    pub name: String,
    /// Version from ref('name', version=N) or ref('name', version='alpha').
    /// Stored as a string to support both integer and non-integer versions.
    #[serde(default)]
    pub version: Option<String>,
}

/// A reference to a dbt source via source()
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SourceCall {
    /// Source name
    pub source_name: String,
    /// Table name within the source
    pub table_name: String,
}

/// Normalize a version string to a canonical form, matching the normalization
/// applied to YAML string version values in version_value_to_str().
/// Integer strings (including zero-padded) are normalized: "02" → "2".
/// Non-integer strings (including "2.0") are returned as-is.
/// Using i64 only (no f64 fallback) keeps this consistent with the YAML string
/// path so that ref(version='2.0') resolves to the same ID as `v: "2.0"`.
pub(super) fn normalize_version_str(s: &str) -> String {
    if let Ok(n) = s.parse::<i64>() {
        return n.to_string();
    }
    s.to_string()
}

/// Parsed config block from SQL
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SqlConfig {
    pub materialized: Option<String>,
    pub tags: Vec<String>,
}

#[cfg(test)]
mod tests;
