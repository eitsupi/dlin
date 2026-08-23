use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::relation::RelationRef;

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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// Cross-model path: (model_name, column_name, transformation) triples for intermediate hops
    /// traversed to reach this source. Ordered from the target model outward toward the leaf source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<(String, String, TransformationType)>,
}

/// Internal source representation. The public `ColumnSource.table` remains a
/// rendered string for JSON/API compatibility; analysis and cache code keeps
/// the structural relation until the final conversion boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InternalColumnSource {
    pub relation: RelationRef,
    pub column: String,
    pub model_path: Vec<(String, String, TransformationType)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InternalColumnLineageEntry {
    pub column: String,
    pub transformation: TransformationType,
    pub sources: Vec<InternalColumnSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InternalModelColumnLineage {
    pub model: String,
    pub traced_columns: usize,
    pub total_columns: usize,
    pub columns: Vec<InternalColumnLineageEntry>,
    pub errors: Vec<ColumnLineageError>,
}

impl InternalModelColumnLineage {
    pub(crate) fn into_public(self) -> ModelColumnLineage {
        ModelColumnLineage {
            model: self.model,
            traced_columns: self.traced_columns,
            total_columns: self.total_columns,
            columns: self
                .columns
                .into_iter()
                .map(|entry| ColumnLineageEntry {
                    column: entry.column,
                    transformation: entry.transformation,
                    sources: entry
                        .sources
                        .into_iter()
                        .map(|source| ColumnSource {
                            table: source.relation.render(),
                            column: source.column,
                            model_path: source.model_path,
                        })
                        .collect(),
                })
                .collect(),
            errors: normalize_column_lineage_errors(self.errors),
        }
    }
}

/// Collapse parseable per-column diagnostics at a public output boundary.
///
/// A single column can produce the same `ColumnNotFound` finding through more
/// than one resolver path. Keep the first group's position, selecting the
/// hinted diagnostic when the other diagnostic identity fields are equal.
pub(crate) fn normalize_column_lineage_errors(
    errors: Vec<ColumnLineageError>,
) -> Vec<ColumnLineageError> {
    let mut normalized = Vec::with_capacity(errors.len());
    let mut groups = HashMap::<(String, String, Option<String>), usize>::new();

    for error in errors {
        let Some(column) = parse_column_not_found_name(&error) else {
            normalized.push(error);
            continue;
        };
        let key = (column.to_string(), error.what.clone(), error.why.clone());

        if let Some(&index) = groups.get(&key) {
            if diagnostic_precedes(&error, &normalized[index]) {
                normalized[index] = error;
            }
        } else {
            groups.insert(key, normalized.len());
            normalized.push(error);
        }
    }

    normalized
}

fn parse_column_not_found_name(error: &ColumnLineageError) -> Option<&str> {
    if error.kind != ColumnLineageErrorKind::ColumnNotFound || !error.what.starts_with("column '") {
        return None;
    }
    let rest = &error.what[8..];
    let end = rest.find("':")?;
    (!rest[..end].is_empty()).then_some(&rest[..end])
}

fn diagnostic_precedes(candidate: &ColumnLineageError, current: &ColumnLineageError) -> bool {
    match (candidate.hint.is_some(), current.hint.is_some()) {
        (true, false) => true,
        (false, true) => false,
        _ => candidate.hint.as_deref().unwrap_or("") < current.hint.as_deref().unwrap_or(""),
    }
}

impl From<ModelColumnLineage> for InternalModelColumnLineage {
    fn from(public: ModelColumnLineage) -> Self {
        Self {
            model: public.model,
            traced_columns: public.traced_columns,
            total_columns: public.total_columns,
            columns: public
                .columns
                .into_iter()
                .map(|entry| InternalColumnLineageEntry {
                    column: entry.column,
                    transformation: entry.transformation,
                    sources: entry
                        .sources
                        .into_iter()
                        .map(|source| InternalColumnSource {
                            relation: RelationRef::parse(&source.table)
                                .unwrap_or_else(|_| RelationRef::bare(source.table)),
                            column: source.column,
                            model_path: source.model_path,
                        })
                        .collect(),
                })
                .collect(),
            errors: public.errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(what: &str, why: Option<&str>, hint: Option<&str>) -> ColumnLineageError {
        ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnNotFound,
            what: what.to_string(),
            why: why.map(str::to_string),
            hint: hint.map(str::to_string),
        }
    }

    #[test]
    fn normalizes_equivalent_column_errors_and_preserves_distinct_diagnostics() {
        let errors = normalize_column_lineage_errors(vec![
            ColumnLineageError {
                kind: ColumnLineageErrorKind::ParseFailure,
                what: "parse".to_string(),
                why: None,
                hint: None,
            },
            error("column 'dup_col': unresolved", Some("reason"), None),
            error("column 'dup_col': unresolved", Some("reason"), Some("star")),
            error("column 'dup_col': different", Some("reason"), None),
            error("column 'dup_col': unresolved", Some("other reason"), None),
        ]);

        assert_eq!(errors.len(), 4);
        assert_eq!(errors[0].what, "parse");
        assert_eq!(errors[1].what, "column 'dup_col': unresolved");
        assert_eq!(errors[1].hint.as_deref(), Some("star"));
        assert_eq!(errors[2].what, "column 'dup_col': different");
        assert_eq!(errors[3].why.as_deref(), Some("other reason"));
    }

    #[test]
    fn keeps_malformed_and_other_kinds_ungrouped() {
        let errors = normalize_column_lineage_errors(vec![
            error("column 'dup_col': same", None, Some("z")),
            error("column 'dup_col': same", None, Some("a")),
            error("column dup_col: malformed", None, None),
            ColumnLineageError {
                kind: ColumnLineageErrorKind::ParseFailure,
                what: "column 'dup_col': same".to_string(),
                why: None,
                hint: None,
            },
        ]);

        assert_eq!(errors.len(), 3);
        assert_eq!(errors[0].hint.as_deref(), Some("a"));
        assert_eq!(errors[1].what, "column dup_col: malformed");
        assert_eq!(errors[2].kind, ColumnLineageErrorKind::ParseFailure);
    }
}
