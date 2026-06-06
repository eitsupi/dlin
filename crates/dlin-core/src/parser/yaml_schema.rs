use serde::Deserialize;

/// Top-level schema YAML file (can contain sources, models, snapshots, exposures,
/// semantic_models, metrics, saved_queries)
#[derive(Debug, Deserialize, Default)]
pub struct SchemaFile {
    #[serde(default)]
    pub sources: Vec<SourceDefinition>,

    #[serde(default)]
    pub models: Vec<ModelDefinition>,

    #[serde(default)]
    pub snapshots: Vec<SnapshotDefinition>,

    #[serde(default)]
    pub exposures: Vec<ExposureDefinition>,

    #[serde(default)]
    pub semantic_models: Vec<SemanticModelDefinition>,

    #[serde(default)]
    pub metrics: Vec<MetricDefinition>,

    #[serde(default)]
    pub saved_queries: Vec<SavedQueryDefinition>,
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
/// JSON integer and float values are normalized without trailing fractional
/// parts (2.0 → "2"). String values are parsed as i64 when possible (so
/// "2" normalizes to "2" consistently with a YAML integer `2`); otherwise
/// the string is returned as-is. This matches dbt-core's int-or-string
/// version semantics and avoids f64 precision loss on large integers.
fn version_value_to_str(v: &serde_json::Value) -> String {
    if let Some(n) = v.as_i64() {
        return n.to_string();
    }
    if let Some(n) = v.as_u64() {
        return n.to_string();
    }
    if let Some(f) = v.as_f64() {
        // Reached only for JSON floats; serde_json stores integers as i64/u64
        // (handled above), so NaN/Inf cannot arise from valid JSON input.
        // dbt-core uses f32 for version comparison, so f64 is already more
        // precise than the reference implementation.
        return if f.fract() == 0.0 {
            (f as i64).to_string()
        } else {
            f.to_string()
        };
    }
    if let Some(s) = v.as_str() {
        if let Ok(n) = s.parse::<i64>() {
            return n.to_string();
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
        let numerics: Vec<i64> = strs.iter().filter_map(|s| s.parse().ok()).collect();
        if numerics.len() == strs.len() {
            // All versions are integers: use the largest. i64 is intentionally
            // used here — dbt-core itself compares via f32 (losing precision
            // above 2^24 ≈ 16.7M), so i64 is already far more robust than the
            // reference implementation.
            numerics.into_iter().max().map(|n| n.to_string())
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

/// YAML-only snapshot definition (dbt v1.9+).
/// When no `.sql` file exists for the snapshot, the graph node is built from this.
#[derive(Debug, Deserialize, Clone)]
pub struct SnapshotDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Upstream relation, e.g. `ref('model_name')`.
    #[serde(default)]
    pub relation: Option<String>,
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

/// A semantic model definition (dbt Semantic Layer)
#[derive(Debug, Deserialize, Clone)]
pub struct SemanticModelDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// The upstream dbt model as a ref() string, e.g. "ref('orders')"
    #[serde(default)]
    pub model: Option<String>,
    /// Measure names defined by this semantic model (used to resolve metric edges)
    #[serde(default)]
    pub measures: Vec<MeasureDefinition>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MeasureDefinition {
    pub name: String,
}

/// A metric definition (dbt Semantic Layer)
#[derive(Debug, Deserialize, Clone)]
pub struct MetricDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Raw type_params blob — used to extract measure/metric references
    /// without needing to model the full metric type hierarchy.
    #[serde(default)]
    pub type_params: Option<serde_json::Value>,
}

impl MetricDefinition {
    /// Extract the measure name this metric references (Simple metrics only).
    pub fn measure_ref(&self) -> Option<&str> {
        self.type_params
            .as_ref()
            .and_then(|p| p.get("measure"))
            .and_then(|m| m.as_str())
    }

    /// Extract metric names this metric depends on (Ratio/Derived/Conversion/Cumulative).
    pub fn metric_refs(&self) -> Vec<&str> {
        let Some(p) = &self.type_params else {
            return vec![];
        };
        let mut refs = vec![];
        // Ratio: numerator / denominator (string or {name: ...})
        for field in &["numerator", "denominator", "input_metric", "base_metric", "conversion_metric"] {
            if let Some(v) = p.get(field) {
                if let Some(s) = v.as_str() {
                    refs.push(s);
                } else if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                    refs.push(name);
                }
            }
        }
        // Derived: input_metrics: [{name: ...}, ...]
        if let Some(arr) = p.get("input_metrics").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(s) = item.as_str() {
                    refs.push(s);
                } else if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    refs.push(name);
                }
            }
        }
        refs
    }
}

/// A saved query definition (dbt Semantic Layer)
#[derive(Debug, Deserialize, Clone)]
pub struct SavedQueryDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub query_params: Option<SavedQueryQueryParams>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SavedQueryQueryParams {
    #[serde(default)]
    pub metrics: Vec<String>,
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
        assert!(schema.snapshots.is_empty());
        assert!(schema.exposures.is_empty());
    }

    #[test]
    fn test_parse_yaml_only_snapshots() {
        let yaml = r#"
snapshots:
  - name: snap_orders
    description: Orders snapshot
    relation: ref('stg_orders')
  - name: snap_customers
    relation: ref('stg_customers', version=2)
  - name: snap_no_relation
    description: Snapshot without upstream relation
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.snapshots.len(), 3);
        assert_eq!(schema.snapshots[0].name, "snap_orders");
        assert_eq!(
            schema.snapshots[0].description.as_deref(),
            Some("Orders snapshot")
        );
        assert_eq!(
            schema.snapshots[0].relation.as_deref(),
            Some("ref('stg_orders')")
        );
        assert_eq!(schema.snapshots[1].name, "snap_customers");
        assert_eq!(
            schema.snapshots[1].relation.as_deref(),
            Some("ref('stg_customers', version=2)")
        );
        assert!(schema.snapshots[2].relation.is_none());
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

        // Large integer string must not lose precision through f64 conversion
        // (9007199254740993 = 2^53 + 1, which f64 cannot represent exactly)
        let large_int = VersionSpec {
            v: serde_json::Value::String("9007199254740993".to_string()),
            defined_in: None,
        };
        assert_eq!(large_int.v_str(), "9007199254740993");

        // JSON Number stored as u64 (> i64::MAX) must not lose precision via f64
        let u64_num = VersionSpec {
            v: serde_json::Value::Number(serde_json::Number::from(i64::MAX as u64 + 1)),
            defined_in: None,
        };
        assert_eq!(u64_num.v_str(), (i64::MAX as u64 + 1).to_string());
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

    #[test]
    fn test_parse_semantic_models() {
        let yaml = r#"
semantic_models:
  - name: orders
    description: Order semantic model
    model: ref('orders')
    measures:
      - name: order_total
      - name: order_count
    dimensions:
      - name: ordered_at
        type: time
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.semantic_models.len(), 1);
        let sm = &schema.semantic_models[0];
        assert_eq!(sm.name, "orders");
        assert_eq!(sm.description.as_deref(), Some("Order semantic model"));
        assert_eq!(sm.model.as_deref(), Some("ref('orders')"));
        assert_eq!(sm.measures.len(), 2);
        assert_eq!(sm.measures[0].name, "order_total");
        assert_eq!(sm.measures[1].name, "order_count");
    }

    #[test]
    fn test_parse_metrics_simple() {
        let yaml = r#"
metrics:
  - name: order_total
    label: Order Total
    description: Sum of orders
    type: simple
    type_params:
      measure: order_total
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.metrics.len(), 1);
        let m = &schema.metrics[0];
        assert_eq!(m.name, "order_total");
        assert_eq!(m.label.as_deref(), Some("Order Total"));
        assert_eq!(m.measure_ref(), Some("order_total"));
        assert!(m.metric_refs().is_empty());
    }

    #[test]
    fn test_parse_metrics_ratio() {
        let yaml = r#"
metrics:
  - name: revenue_per_order
    type: ratio
    type_params:
      numerator: revenue
      denominator: orders
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        let m = &schema.metrics[0];
        assert_eq!(m.measure_ref(), None);
        let refs = m.metric_refs();
        assert!(refs.contains(&"revenue"));
        assert!(refs.contains(&"orders"));
    }

    #[test]
    fn test_parse_metrics_derived_with_input_metrics() {
        let yaml = r#"
metrics:
  - name: pct_change
    type: derived
    type_params:
      input_metrics:
        - name: revenue
        - name: orders
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        let m = &schema.metrics[0];
        assert_eq!(m.measure_ref(), None);
        let refs = m.metric_refs();
        assert!(refs.contains(&"revenue"));
        assert!(refs.contains(&"orders"));
    }

    #[test]
    fn test_parse_saved_queries() {
        let yaml = r#"
saved_queries:
  - name: order_metrics
    description: Key order metrics
    query_params:
      metrics:
        - orders
        - order_total
        - food_orders
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.saved_queries.len(), 1);
        let sq = &schema.saved_queries[0];
        assert_eq!(sq.name, "order_metrics");
        assert_eq!(sq.description.as_deref(), Some("Key order metrics"));
        let metrics = sq.query_params.as_ref().unwrap().metrics.as_slice();
        assert_eq!(metrics, &["orders", "order_total", "food_orders"]);
    }

    #[test]
    fn test_parse_full_semantic_layer_yaml() {
        // Simulates a real jaffle-shop style YAML with all three semantic layer blocks
        let yaml = r#"
models:
  - name: orders
    description: Orders table

semantic_models:
  - name: orders
    model: ref('orders')
    measures:
      - name: order_count
      - name: order_total

metrics:
  - name: orders
    type: simple
    type_params:
      measure: order_count
  - name: order_total
    type: simple
    type_params:
      measure: order_total

saved_queries:
  - name: order_kpis
    query_params:
      metrics:
        - orders
        - order_total
"#;
        let schema = parse_schema_file(yaml, None).unwrap();
        assert_eq!(schema.models.len(), 1);
        assert_eq!(schema.semantic_models.len(), 1);
        assert_eq!(schema.metrics.len(), 2);
        assert_eq!(schema.saved_queries.len(), 1);
        assert_eq!(
            schema.saved_queries[0]
                .query_params
                .as_ref()
                .unwrap()
                .metrics
                .len(),
            2
        );
    }
}
