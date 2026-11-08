use std::collections::HashMap;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::relation::RelationRef;

/// Error kind discriminator for column lineage errors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
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
    /// A specific column has multiple possible bindings.
    ColumnAmbiguous,
    /// A specific column could not be proven due to incomplete semantic information.
    /// This preserves the nearest honest lineage terminal and is non-fatal to CLI callers.
    ColumnIndeterminate,
    /// The lineage backend violated its response contract.
    Internal,
}

/// A structured error from column lineage analysis.
///
/// Uses the same `what`/`why`/`hint` field layout as the project-wide
/// `Diagnostic` type, plus a `kind` discriminator for programmatic handling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ColumnLineageError {
    pub kind: ColumnLineageErrorKind,
    /// Output column the diagnostic is about; `None` for model-level diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
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

impl ColumnLineageError {
    /// Return the output column associated with this diagnostic.
    pub fn column_name(&self) -> Option<&str> {
        self.column.as_deref()
    }

    pub fn is_column_scoped(&self) -> bool {
        self.column.is_some()
    }

    pub fn is_fatal(&self) -> bool {
        !matches!(self.kind, ColumnLineageErrorKind::ColumnIndeterminate)
    }

    /// Re-associate a column-scoped diagnostic with the output column whose
    /// lineage traversal surfaced it. This keeps the structured identity and
    /// the historical human-readable prefix consistent for API consumers.
    pub fn rebase_column(&self, column: &str) -> Self {
        if !self.is_column_scoped() {
            return self.clone();
        }
        let mut rebased = self.clone();
        rebased.column = Some(column.to_string());
        if let Some(rest) = self.what.strip_prefix("column '")
            && let Some((_, detail)) = rest.split_once("':")
        {
            rebased.what = format!("column '{column}':{detail}");
        }
        rebased
    }
}

/// Column lineage result for a single model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelColumnLineage {
    pub model: String,
    /// Number of output columns whose value lineage is `Complete`.
    ///
    /// Columns with a `Partial` lineage are listed in `columns` but are not
    /// counted here, so this is not the length of `columns`. It is also not a
    /// measure of how far tracing reached.
    pub traced_columns: usize,
    /// Total number of columns attempted (0 when model/SQL could not be loaded)
    pub total_columns: usize,
    pub columns: Vec<ColumnLineageEntry>,
    pub errors: Vec<ColumnLineageError>,
}

/// Lineage for a single output column
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnLineageEntry {
    pub column: String,
    pub transformation: TransformationType,
    /// Known sources of the value, and what remains unresolved.
    pub lineage: ValueLineage<ColumnSource>,
}

impl ModelColumnLineage {
    /// Number of listed columns whose value lineage is `Complete`.
    pub fn complete_column_count(&self) -> usize {
        self.columns
            .iter()
            .filter(|entry| entry.lineage.is_complete())
            .count()
    }
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
    pub lineage: ValueLineage<InternalColumnSource>,
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
    /// Number of listed columns whose value lineage is `Complete`.
    pub(crate) fn complete_column_count(&self) -> usize {
        self.columns
            .iter()
            .filter(|entry| entry.lineage.is_complete())
            .count()
    }

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
                    lineage: entry.lineage.map_known(|source| ColumnSource {
                        table: source.relation.render(),
                        column: source.column,
                        model_path: source.model_path,
                    }),
                })
                .collect(),
            errors: normalize_column_lineage_errors(self.errors),
        }
    }
}

/// Collapse parseable per-column diagnostics at a public output boundary.
///
/// A single column can produce the same column-scoped finding through more than
/// one resolver path. Keep the first group's position, selecting the hinted
/// diagnostic when the other diagnostic identity fields are equal.
pub(crate) fn normalize_column_lineage_errors(
    errors: Vec<ColumnLineageError>,
) -> Vec<ColumnLineageError> {
    let mut normalized = Vec::with_capacity(errors.len());
    let mut groups =
        HashMap::<(String, ColumnLineageErrorKind, String, Option<String>), usize>::new();

    for error in errors {
        let Some(column) = error.column_name() else {
            normalized.push(error);
            continue;
        };
        if !error.is_column_scoped() {
            normalized.push(error);
            continue;
        }
        let key = (
            column.to_string(),
            error.kind,
            error.what.clone(),
            error.why.clone(),
        );

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
                    lineage: entry.lineage.map_known(|source| InternalColumnSource {
                        relation: RelationRef::parse(&source.table)
                            .unwrap_or_else(|_| RelationRef::bare(source.table)),
                        column: source.column,
                        model_path: source.model_path,
                    }),
                })
                .collect(),
            errors: public.errors,
        }
    }
}

// --- Value lineage ---

/// Identity of one output column of a dbt model.
///
/// `unique_id` is the stable identity; `model` is the display name used by the
/// rest of the report.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelOutputRef {
    pub unique_id: String,
    pub model: String,
    pub column: String,
}

/// One hop of a cross-model path: `(model_name, column_name, transformation)`.
pub type ModelPathHop = (String, String, TransformationType);

/// The sources that supply the value of one output column, and whether that
/// list is the whole story.
///
/// The type is a `Complete` / `Partial` pair with one invariant: `Partial`
/// always has at least one unresolved contribution. The fields are private so
/// the invariant cannot be broken. [`ValueLineage::new`] returns `Complete`
/// when nothing is unresolved, and deserialization rejects a `partial` status
/// without unresolved contributions.
///
/// Serialized form: `{"status":"complete","known":[..]}` or
/// `{"status":"partial","known":[..],"unresolved":[..]}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueLineage<S> {
    known: Vec<S>,
    unresolved: Vec<UnresolvedContribution>,
}

impl<S> ValueLineage<S> {
    /// Lineage whose every contributing source is known.
    pub fn complete(known: Vec<S>) -> Self {
        Self {
            known,
            unresolved: Vec::new(),
        }
    }

    /// Lineage with the given known sources and unresolved contributions.
    /// It is `Complete` when `unresolved` is empty.
    pub fn new(known: Vec<S>, unresolved: Vec<UnresolvedContribution>) -> Self {
        Self { known, unresolved }
    }

    /// Sources that are proven to contribute to the value.
    pub fn known(&self) -> &[S] {
        &self.known
    }

    /// Contributions that could not be resolved. Empty exactly when the
    /// lineage is complete.
    pub fn unresolved(&self) -> &[UnresolvedContribution] {
        &self.unresolved
    }

    pub fn is_complete(&self) -> bool {
        self.unresolved.is_empty()
    }

    pub fn is_partial(&self) -> bool {
        !self.is_complete()
    }

    /// Split into known sources and unresolved contributions.
    pub fn into_parts(self) -> (Vec<S>, Vec<UnresolvedContribution>) {
        (self.known, self.unresolved)
    }

    /// Convert each known source, keeping the unresolved contributions.
    pub fn map_known<T>(self, f: impl FnMut(S) -> T) -> ValueLineage<T> {
        ValueLineage {
            known: self.known.into_iter().map(f).collect(),
            unresolved: self.unresolved,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ValueLineageOut<'a, S> {
    Complete {
        known: &'a [S],
    },
    Partial {
        known: &'a [S],
        unresolved: &'a [UnresolvedContribution],
    },
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ValueLineageIn<S> {
    Complete {
        known: Vec<S>,
    },
    Partial {
        known: Vec<S>,
        unresolved: Vec<UnresolvedContribution>,
    },
}

impl<S: Serialize> Serialize for ValueLineage<S> {
    fn serialize<Z: Serializer>(&self, serializer: Z) -> Result<Z::Ok, Z::Error> {
        if self.is_complete() {
            ValueLineageOut::Complete { known: &self.known }.serialize(serializer)
        } else {
            ValueLineageOut::Partial {
                known: &self.known,
                unresolved: &self.unresolved,
            }
            .serialize(serializer)
        }
    }
}

impl<'de, S: Deserialize<'de>> Deserialize<'de> for ValueLineage<S> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match ValueLineageIn::<S>::deserialize(deserializer)? {
            ValueLineageIn::Complete { known } => Ok(Self::complete(known)),
            ValueLineageIn::Partial { unresolved, .. } if unresolved.is_empty() => {
                Err(D::Error::custom(
                    "a partial value lineage must have at least one unresolved contribution",
                ))
            }
            ValueLineageIn::Partial { known, unresolved } => Ok(Self::new(known, unresolved)),
        }
    }
}

/// A part of an output value whose source could not be determined.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnresolvedContribution {
    pub reason: UnresolvedReason,
    /// The model output where the unresolved part arose.
    pub origin: ModelOutputRef,
    /// Model hops from the root output (exclusive) to `origin` (inclusive),
    /// in the order of [`ColumnSource::model_path`]. Empty when `origin` is
    /// the root output itself.
    pub model_path: Vec<ModelPathHop>,
}

/// Why a contribution is unresolved. This mirrors the engine's reasons and
/// keeps their structured data; it is not a rendered message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnresolvedReason {
    /// Positional correspondence of a set operation cannot be established.
    AlignmentUnknown {
        operation: SetOperationKind,
        possible_sources: Vec<UnresolvedSource>,
        unknown_operands: Vec<SetOperand>,
    },
    /// More schema information could change the contributing columns.
    SchemaDependent {
        what: SchemaDependencyKind,
        relations: Vec<RelationRef>,
    },
    /// An unexpanded star may contribute a column from this relation.
    BehindStar { relation: RelationRef },
    /// A named output may come from a column hidden behind an unexpanded star.
    NamedBehindStar {
        relation: RelationRef,
        column: String,
    },
    /// A column could come from more than one candidate.
    Ambiguous {
        column: String,
        candidates: Vec<UnresolvedCandidate>,
    },
    /// A column could not be bound to a relation in scope.
    Unbound { column: String },
    /// Recursive lineage is represented by its known base sources only.
    Recursive,
    /// A whole-row alias is used where a column source is expected.
    RowValue { alias: String },
    /// The syntax is not modelled by the engine.
    Unsupported { what: String },
}

/// Human-readable form for display only. The structured variant, not this
/// text, is what callers should branch on.
impl std::fmt::Display for UnresolvedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlignmentUnknown {
                operation,
                possible_sources,
                ..
            } => {
                let operator = match operation.operator {
                    SetOperator::Union => "UNION",
                    SetOperator::Intersect => "INTERSECT",
                    SetOperator::Except => "EXCEPT",
                };
                write!(
                    f,
                    "{operator} operand columns cannot be aligned ({} possible source{})",
                    possible_sources.len(),
                    if possible_sources.len() == 1 { "" } else { "s" }
                )
            }
            Self::SchemaDependent { relations, .. } => {
                let relations: Vec<String> = relations.iter().map(ToString::to_string).collect();
                write!(
                    f,
                    "natural join columns depend on the schema of {}",
                    relations.join(", ")
                )
            }
            Self::BehindStar { relation } => {
                write!(f, "may come from an unexpanded star of {relation}")
            }
            Self::NamedBehindStar { relation, column } => {
                write!(
                    f,
                    "'{column}' may come from an unexpanded star of {relation}"
                )
            }
            Self::Ambiguous { column, candidates } => {
                let candidates: Vec<String> = candidates
                    .iter()
                    .map(|candidate| match candidate {
                        UnresolvedCandidate::Relation { relation } => relation.to_string(),
                        UnresolvedCandidate::Cte { name } => format!("cte {name}"),
                        UnresolvedCandidate::Derived { name: Some(name) } => {
                            format!("derived table {name}")
                        }
                        UnresolvedCandidate::Derived { name: None } => "derived table".to_string(),
                    })
                    .collect();
                write!(
                    f,
                    "'{column}' is ambiguous between {}",
                    candidates.join(", ")
                )
            }
            Self::Unbound { column } => write!(f, "'{column}' is not bound to a relation"),
            Self::Recursive => write!(f, "recursive lineage is limited to its base sources"),
            Self::RowValue { alias } => write!(f, "whole-row value '{alias}'"),
            Self::Unsupported { what } => write!(f, "unsupported syntax: {what}"),
        }
    }
}

/// A concrete relation column named by an unresolved reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnresolvedSource {
    pub relation: RelationRef,
    pub column: String,
}

/// A candidate source of an ambiguous column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnresolvedCandidate {
    Relation { relation: RelationRef },
    Cte { name: String },
    Derived { name: Option<String> },
}

/// The kind of a set operation, as reported by an unresolved reason.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SetOperationKind {
    pub operator: SetOperator,
    pub quantifier: SetQuantifier,
    pub correspondence: SetCorrespondence,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SetOperator {
    Union,
    Intersect,
    Except,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SetQuantifier {
    All,
    Distinct,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SetCorrespondence {
    Positional,
    ByName,
}

/// An operand of a set operation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SetOperand {
    Left,
    Right,
}

/// Schema information that an unresolved dependency needs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum SchemaDependencyKind {
    NaturalJoinColumns,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(what: &str, why: Option<&str>, hint: Option<&str>) -> ColumnLineageError {
        let column = what.strip_prefix("column '").and_then(|rest| {
            let end = rest.find("':")?;
            (!rest[..end].is_empty()).then_some(rest[..end].to_string())
        });
        ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnNotFound,
            column,
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
                column: None,
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
                column: None,
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

    #[test]
    fn column_identity_is_structured() {
        let structured = ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnIndeterminate,
            column: Some("col_a".to_string()),
            what: "row value could not be expanded".to_string(),
            why: None,
            hint: None,
        };
        let message_only = ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnNotFound,
            column: None,
            what: "column 'col_a': not found".to_string(),
            why: None,
            hint: None,
        };
        assert_eq!(structured.column_name(), Some("col_a"));
        assert!(structured.is_column_scoped());
        assert!(!structured.is_fatal());
        // The message text is never parsed to recover a column identity.
        assert_eq!(message_only.column_name(), None);
        assert!(!message_only.is_column_scoped());
        assert!(message_only.is_fatal());

        let encoded = serde_json::to_value(&structured).unwrap();
        assert_eq!(encoded["column"], "col_a");
    }

    #[test]
    fn partial_value_lineage_requires_unresolved_contribution() {
        let complete = ValueLineage::<String>::new(vec!["a".to_string()], vec![]);
        assert!(complete.is_complete());
        assert_eq!(
            serde_json::to_value(&complete).unwrap(),
            serde_json::json!({"status": "complete", "known": ["a"]})
        );

        let invalid = serde_json::json!({"status": "partial", "known": ["a"], "unresolved": []});
        assert!(serde_json::from_value::<ValueLineage<String>>(invalid).is_err());
    }
}
