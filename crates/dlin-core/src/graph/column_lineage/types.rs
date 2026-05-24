use serde::{Deserialize, Serialize};

/// Error kind discriminator for column lineage errors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ColumnLineageErrorKind {
    /// The specified model was not found in the dbt manifest.
    ModelNotFound,
    /// The model has no compiled SQL (run `dbt compile` first).
    NoCompiledCode,
    /// Output columns could not be determined (no YAML columns and SQL inference failed).
    ColumnInferenceFailed,
    /// The compiled SQL could not be parsed.
    ParseFailure,
    /// Lineage for a specific column could not be traced.
    ColumnNotFound,
}

/// A structured error from column lineage analysis.
///
/// Uses the same `what`/`why`/`hint` field layout as the project-wide
/// `Diagnostic` type, plus a `kind` discriminator for programmatic handling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColumnLineageError {
    pub kind: ColumnLineageErrorKind,
    pub what: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl std::fmt::Display for ColumnLineageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.what)
    }
}

/// Column lineage result for a single model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelColumnLineage {
    pub model: String,
    /// Number of columns successfully traced
    pub traced_columns: usize,
    /// Total number of columns attempted (0 when model/SQL could not be loaded)
    pub total_columns: usize,
    pub columns: Vec<ColumnLineageEntry>,
    #[serde(default)]
    pub errors: Vec<ColumnLineageError>,
}

/// Lineage for a single output column
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnLineageEntry {
    pub column: String,
    pub transformation: TransformationType,
    pub sources: Vec<ColumnSource>,
}

/// Classification of the transformation applied to produce an output column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TransformationType {
    /// Direct column reference or rename (e.g. `SELECT id AS order_id`)
    Direct,
    /// Aggregate function (e.g. `COUNT(*)`, `SUM(amount)`)
    Aggregation,
    /// Arithmetic or other expression (e.g. `price * quantity`)
    Expression,
    /// Type cast (e.g. `CAST(x AS INT)`)
    Cast,
    /// Conditional expression (e.g. `CASE WHEN ...`)
    Conditional,
    /// Could not classify the transformation
    Unknown,
}

/// A source column reference
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ColumnSource {
    /// Source table/model name as it appears in SQL (e.g. "stg_orders", "`raw`.`orders`")
    pub table: String,
    /// Source column name
    pub column: String,
    /// Cross-model path: (model_name, column_name) pairs for intermediate hops traversed to reach
    /// this source. Ordered from the target model outward toward the leaf source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<(String, String)>,
}
