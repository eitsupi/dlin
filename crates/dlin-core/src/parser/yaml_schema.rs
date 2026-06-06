use serde::Deserialize;

/// Top-level schema YAML file (can contain sources, models, exposures)
#[derive(Debug, Deserialize, Default)]
pub struct SchemaFile {
    #[serde(default)]
    pub sources: Vec<SourceDefinition>,

    #[serde(default)]
    pub models: Vec<ModelDefinition>,

    #[serde(default)]
    pub exposures: Vec<ExposureDefinition>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SourceDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tables: Vec<SourceTable>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SourceTable {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub columns: Vec<ColumnDefinition>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ColumnDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, alias = "data_tests")]
    pub tests: Vec<TestDefinition>,
}

/// Tests can be either a string or a map.
/// Complex variants are deserialized into `serde_json::Value` because serde-saphyr
/// has no intermediate Value type. This is safe for dbt schema files which use
/// JSON-compatible YAML.
#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum TestDefinition {
    Simple(String),
    Complex(serde_json::Value),
}

impl TestDefinition {
    /// Extract the test name from either variant.
    ///
    /// - `Simple("not_null")` → `"not_null"`
    /// - `Complex({"unique": {...}})` → `"unique"`
    /// - `Complex({"name": "custom", "test_name": "accepted_values", ...})` → `"accepted_values"`
    pub fn test_name(&self) -> Option<&str> {
        match self {
            TestDefinition::Simple(s) => Some(s.as_str()),
            TestDefinition::Complex(v) => {
                let obj = v.as_object()?;
                // Alternative format: {"name": "...", "test_name": "accepted_values", ...}
                if let Some(tn) = obj.get("test_name").and_then(|v| v.as_str()) {
                    return Some(tn);
                }
                // Standard format: single-key map like {"unique": {...}}
                // Note: serde_json::Map uses BTreeMap, so keys() is alphabetically ordered.
                // Skip objects that only have meta-keys (name/config/arguments).
                for key in obj.keys() {
                    if !matches!(key.as_str(), "config" | "arguments" | "name") {
                        return Some(key.as_str());
                    }
                }
                None
            }
        }
    }
}

/// Normalize a serde_json::Value version field to a canonical string.
/// Numeric JSON values and string values that parse as numbers are formatted
/// without trailing fractional parts (2.0 → "2"), matching how dbt-core
/// renders version numbers from both YAML integers and YAML strings.
fn version_value_to_str(v: &serde_json::Value) -> String {
    if let Some(n) = v.as_i64() {
        return n.to_string();
    }
    if let Some(f) = v.as_f64() {
        return if f.fract() == 0.0 {
            (f as i64).to_string()
        } else {
            f.to_string()
        };
    }
    if let Some(s) = v.as_str() {
        if let Ok(f) = s.parse::<f64>() {
            return if f.fract() == 0.0 {
                (f as i64).to_string()
            } else {
                f.to_string()
            };
        }
        return s.to_string();
    }
    v.to_string()
}

/// A single entry in the `versions:` list of a model definition.
#[derive(Debug, Deserialize, Clone)]
pub struct VersionSpec {
    pub v: serde_json::Value,
    /// SQL file stem override (defaults to `{model_name}_v{v}`)
    #[serde(default)]
    pub defined_in: Option<String>,
}

impl VersionSpec {
    /// Return the version number formatted as a string (e.g. `"1"`, `"2"`).
    pub fn v_str(&self) -> String {
        version_value_to_str(&self.v)
    }

    /// Return the SQL file stem for this version.
    /// Falls back to `{model_name}_v{v}` when `defined_in` is not set.
    pub fn sql_stem(&self, model_name: &str) -> String {
        self.defined_in
            .clone()
            .unwrap_or_else(|| format!("{}_v{}", model_name, self.v_str()))
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct ModelDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub columns: Vec<ColumnDefinition>,
    #[serde(default)]
    pub config: Option<ModelConfig>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Model-level tests (not attached to a specific column)
    #[serde(default, alias = "data_tests")]
    pub tests: Vec<TestDefinition>,
    /// Versioned model definitions (dbt v1.5+)
    #[serde(default)]
    pub versions: Vec<VersionSpec>,
    /// Latest version used when ref('name') is called without version= kwarg
    #[serde(default)]
    pub latest_version: Option<serde_json::Value>,
}

impl ModelDefinition {
    /// Return the version string used for `latest_version`, or derive it from
    /// the `versions` list when `latest_version` is unset.
    ///
    /// Inference mirrors dbt-core: if all versions parse as numbers, use the
    /// largest; otherwise fall back to the lexicographically greatest string.
    pub fn resolved_latest_version_str(&self) -> Option<String> {
        if let Some(lv) = &self.latest_version {
            return Some(version_value_to_str(lv));
        }
        if self.versions.is_empty() {
            return None;
        }
        let strs: Vec<String> = self.versions.iter().map(|v| v.v_str()).collect();
        let numerics: Vec<f64> = strs.iter().filter_map(|s| s.parse().ok()).collect();
        if numerics.len() == strs.len() {
            // Matches dbt-core: convert the max f64 back to a string.
            // version_value_to_str() normalizes all numeric forms (v_str, latest_version),
            // so this always returns the same string used to build the node unique_id.
            numerics.into_iter().reduce(f64::max).map(|n| {
                if n.fract() == 0.0 {
                    (n as i64).to_string()
                } else {
                    n.to_string()
                }
            })
        } else {
            strs.into_iter().max()
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ModelConfig {
    #[serde(default)]
    pub materialized: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExposureDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(rename = "type", default)]
    pub exposure_type: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub maturity: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub owner: Option<ExposureOwner>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExposureOwner {
    pub name: Option<String>,
    pub email: Option<String>,
}

/// Parse a schema YAML file
pub fn parse_schema_file(
    content: &str,
    path: Option<&std::path::Path>,
) -> anyhow::Result<SchemaFile> {
    let location = path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<input>".to_string());
    super::yaml_from_str(content, &location)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sources() {
        let yaml = r#"
sources:
  - name: raw
    description: Raw data from the warehouse
    tables:
      - name: orders
        description: Raw orders table
      - name: customers
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.sources.len(), 1);
        assert_eq!(schema.sources[0].name, "raw");
        assert_eq!(schema.sources[0].tables.len(), 2);
        assert_eq!(schema.sources[0].tables[0].name, "orders");
    }

    #[test]
    fn test_parse_models_with_data_tests() {
        let yaml = r#"
models:
  - name: stg_orders
    description: Staged orders
    columns:
      - name: order_id
        data_tests:
          - not_null
          - unique
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.models.len(), 1);
        assert_eq!(schema.models[0].name, "stg_orders");
        assert_eq!(schema.models[0].columns.len(), 1);
        assert_eq!(schema.models[0].columns[0].tests.len(), 2);
    }

    #[test]
    fn test_parse_models_with_legacy_tests_key() {
        let yaml = r#"
models:
  - name: stg_orders
    columns:
      - name: order_id
        tests:
          - not_null
          - unique
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.models[0].columns[0].tests.len(), 2);
    }

    #[test]
    fn test_parse_data_tests_all_formats() {
        let yaml = r#"
models:
  - name: orders
    columns:
      - name: order_id
        data_tests:
          - not_null
          - unique:
              config:
                where: "order_id > 21"
      - name: status
        data_tests:
          - accepted_values:
              arguments:
                values:
                  - placed
                  - shipped
                  - completed
                  - returned
              config:
                severity: warn
      - name: customer_id
        data_tests:
          - relationships:
              arguments:
                to: ref('customers')
                field: id
          - name: custom_test_name
            test_name: accepted_values
            arguments:
              values:
                - 1
                - 2
                - 3
            config:
              where: "order_date = current_date"
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        let model = &schema.models[0];
        assert_eq!(model.columns.len(), 3);

        // Simple + map with config
        assert_eq!(model.columns[0].tests.len(), 2);
        assert!(
            matches!(model.columns[0].tests[0], TestDefinition::Simple(ref s) if s == "not_null")
        );
        assert!(matches!(
            model.columns[0].tests[1],
            TestDefinition::Complex(_)
        ));

        // accepted_values with arguments + config
        assert_eq!(model.columns[1].tests.len(), 1);
        assert!(matches!(
            model.columns[1].tests[0],
            TestDefinition::Complex(_)
        ));

        // relationships + alternative name/test_name format
        assert_eq!(model.columns[2].tests.len(), 2);
        assert!(matches!(
            model.columns[2].tests[0],
            TestDefinition::Complex(_)
        ));
        assert!(matches!(
            model.columns[2].tests[1],
            TestDefinition::Complex(_)
        ));
    }

    #[test]
    fn test_parse_exposures() {
        let yaml = r#"
exposures:
  - name: weekly_report
    description: Weekly business report
    type: dashboard
    depends_on:
      - ref('orders')
      - ref('customers')
    owner:
      name: Data Team
      email: data@example.com
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.exposures.len(), 1);
        assert_eq!(schema.exposures[0].name, "weekly_report");
        assert_eq!(schema.exposures[0].depends_on.len(), 2);
    }

    #[test]
    fn test_parse_duplicate_mapping_keys() {
        // Duplicate mapping keys (same key at same level) should be tolerated
        // with last-value-wins, matching PyYAML behavior.
        let yaml = r#"
sources:
  - name: raw
    tables:
      - name: orders
sources:
  - name: other
    tables:
      - name: users
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        // Last value wins: "other" source replaces "raw"
        assert_eq!(schema.sources.len(), 1);
        assert_eq!(schema.sources[0].name, "other");
    }

    #[test]
    fn test_empty_file() {
        let yaml = "";
        let schema = parse_schema_file(yaml, None).unwrap();
        assert!(schema.sources.is_empty());
        assert!(schema.models.is_empty());
        assert!(schema.exposures.is_empty());
    }

    #[test]
    fn test_parse_versioned_model() {
        let yaml = r#"
models:
  - name: my_model
    description: A versioned model
    latest_version: 2
    versions:
      - v: 1
      - v: 2
        defined_in: my_model_custom
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.models.len(), 1);
        let m = &schema.models[0];
        assert_eq!(m.name, "my_model");
        assert_eq!(m.versions.len(), 2);
        assert_eq!(m.versions[0].v_str(), "1");
        assert_eq!(m.versions[0].sql_stem("my_model"), "my_model_v1");
        assert_eq!(m.versions[1].v_str(), "2");
        assert_eq!(m.versions[1].sql_stem("my_model"), "my_model_custom");
        assert_eq!(m.resolved_latest_version_str().as_deref(), Some("2"));
    }

    #[test]
    fn test_versioned_model_infers_latest_from_max_v() {
        let yaml = r#"
models:
  - name: orders
    versions:
      - v: 1
      - v: 3
      - v: 2
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        let m = &schema.models[0];
        assert_eq!(m.resolved_latest_version_str().as_deref(), Some("3"));
    }

    #[test]
    fn test_versioned_model_infers_latest_from_quoted_v() {
        let yaml = r#"
models:
  - name: orders
    versions:
      - v: "1"
      - v: "3"
      - v: "2"
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        let m = &schema.models[0];
        assert_eq!(m.resolved_latest_version_str().as_deref(), Some("3"));
    }

    #[test]
    fn test_v_str_normalizes_quoted_numeric() {
        // v: "2" (quoted string) must produce the same ID as v: 2 (YAML integer)
        // so that v_str() and resolved_latest_version_str() stay consistent.
        let quoted = VersionSpec {
            v: serde_json::Value::String("2".to_string()),
            defined_in: None,
        };
        assert_eq!(quoted.v_str(), "2");

        // Quoted integer larger than 1 also normalizes correctly
        let quoted_large = VersionSpec {
            v: serde_json::Value::String("10".to_string()),
            defined_in: None,
        };
        assert_eq!(quoted_large.v_str(), "10");

        // Non-numeric string is returned as-is
        let non_numeric = VersionSpec {
            v: serde_json::Value::String("alpha".to_string()),
            defined_in: None,
        };
        assert_eq!(non_numeric.v_str(), "alpha");
    }

    #[test]
    fn test_unversioned_model_has_empty_versions() {
        let yaml = r#"
models:
  - name: plain_model
    description: Not versioned
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        let m = &schema.models[0];
        assert!(m.versions.is_empty());
        assert!(m.latest_version.is_none());
        assert!(m.resolved_latest_version_str().is_none());
    }

    #[test]
    fn test_test_name_extraction() {
        // Simple string test
        let simple = TestDefinition::Simple("not_null".to_string());
        assert_eq!(simple.test_name(), Some("not_null"));

        // Complex single-key map: {"unique": {"config": ...}}
        let complex_single = TestDefinition::Complex(serde_json::json!({
            "unique": {"config": {"where": "id > 0"}}
        }));
        assert_eq!(complex_single.test_name(), Some("unique"));

        // Complex with test_name field: {"name": "custom", "test_name": "accepted_values", ...}
        let complex_named = TestDefinition::Complex(serde_json::json!({
            "name": "custom_test_name",
            "test_name": "accepted_values",
            "arguments": {"values": [1, 2]}
        }));
        assert_eq!(complex_named.test_name(), Some("accepted_values"));

        // Complex relationships test
        let relationships = TestDefinition::Complex(serde_json::json!({
            "relationships": {"arguments": {"to": "ref('customers')", "field": "id"}}
        }));
        assert_eq!(relationships.test_name(), Some("relationships"));

        // Edge case: {"name": "something"} without test_name should return None
        let name_only = TestDefinition::Complex(serde_json::json!({"name": "something"}));
        assert_eq!(name_only.test_name(), None);
    }
}
