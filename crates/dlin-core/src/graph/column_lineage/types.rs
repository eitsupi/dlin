use std::collections::{BTreeSet, HashMap};
use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::backend::DlinDialect;
use super::ident::Ident;
use super::relation::RelationRef;

/// Hint attached to a diagnostic about a column that may sit behind an
/// unexpanded star.
pub(crate) const OPEN_SHAPE_HINT: &str = "column may be introduced via SELECT * that could not be expanded; \
     define upstream columns in the model YAML to enable full resolution";

/// Error kind discriminator for column lineage errors.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ColumnLineageErrorKind {
    /// The specified model was not found in the dbt manifest.
    ModelNotFound,
    /// A short model name identifies more than one manifest model.
    ModelAmbiguous,
    /// The resource's materialization semantics are not modelled.
    UnsupportedMaterialization,
    /// The model has no compiled SQL (run `dbt compile` first).
    NoCompiledCode,
    /// The compiled SQL could not be parsed or analysed as one query.
    ParseFailure,
    /// Lineage for a specific column could not be traced.
    ColumnNotFound,
    /// A specific column has multiple possible bindings.
    ColumnAmbiguous,
    /// A specific column could not be proven due to incomplete semantic information.
    /// This preserves the nearest honest lineage terminal and is non-fatal to CLI callers.
    ColumnIndeterminate,
    /// A column declared in YAML is not in the enumerated SQL output of a
    /// closed output. Non-fatal.
    DeclaredColumnNotInOutput,
    /// A column declared in YAML is not among the enumerated SQL outputs of an
    /// open output, where an unexpanded star may still provide it. Non-fatal.
    DeclaredColumnNotEnumerated,
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
    /// Display name of the column the diagnostic is about: an output column,
    /// or a column that the YAML declares and the SQL output does not list.
    /// `None` for model-level diagnostics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
    /// The output slot the diagnostic is about, when it is about one slot of
    /// the model's output. It tells columns with the same name apart. A
    /// declared-only column has no slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot: Option<OutputSlotId>,
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
    /// A model-level diagnostic.
    pub(crate) fn model(kind: ColumnLineageErrorKind, what: String) -> Self {
        Self {
            kind,
            column: None,
            slot: None,
            what,
            why: None,
            hint: None,
        }
    }

    /// A diagnostic about one output slot.
    pub(crate) fn for_slot(
        kind: ColumnLineageErrorKind,
        output: &ModelOutputRef,
        detail: &str,
        hint: Option<String>,
    ) -> Self {
        Self {
            kind,
            column: Some(output.column.clone()),
            slot: Some(output.slot),
            what: format!("column '{}': {detail}", output.column),
            why: None,
            hint,
        }
    }

    /// A diagnostic about a column name that is not an output slot.
    pub(crate) fn for_name(
        kind: ColumnLineageErrorKind,
        column: &str,
        detail: &str,
        hint: Option<String>,
    ) -> Self {
        Self {
            kind,
            column: Some(column.to_string()),
            slot: None,
            what: format!("column '{column}': {detail}"),
            why: None,
            hint,
        }
    }

    /// Return the display name of the output column associated with this
    /// diagnostic.
    pub fn column_name(&self) -> Option<&str> {
        self.column.as_deref()
    }

    /// Return the output slot associated with this diagnostic.
    pub fn slot(&self) -> Option<OutputSlotId> {
        self.slot
    }

    pub fn is_column_scoped(&self) -> bool {
        self.column.is_some()
    }

    pub fn is_fatal(&self) -> bool {
        !matches!(
            self.kind,
            ColumnLineageErrorKind::ColumnIndeterminate
                | ColumnLineageErrorKind::DeclaredColumnNotInOutput
                | ColumnLineageErrorKind::DeclaredColumnNotEnumerated
        )
    }

    /// Whether this diagnostic says the model as a whole could not be
    /// analysed. Only such diagnostics are meaningful to the models that read
    /// the failed model.
    pub fn is_model_failure(&self) -> bool {
        !self.is_column_scoped() && self.is_fatal()
    }

    /// Re-associate a slot-scoped diagnostic with the root output whose
    /// lineage traversal surfaced it. This keeps the structured identity and
    /// the human-readable prefix consistent for API consumers.
    pub(crate) fn rebase_to(&self, root: &ModelOutputRef) -> Self {
        if !self.is_column_scoped() {
            return self.clone();
        }
        let mut rebased = self.clone();
        rebased.column = Some(root.column.clone());
        rebased.slot = Some(root.slot);
        if let Some(rest) = self.what.strip_prefix("column '")
            && let Some((_, detail)) = rest.split_once("':")
        {
            rebased.what = format!("column '{}':{detail}", root.column);
        }
        rebased
    }
}

/// Column lineage result for a single model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelColumnLineage {
    /// The manifest unique id of the model, or the name that was asked for
    /// when no such model exists.
    pub unique_id: String,
    pub model: String,
    /// The model's output slots in SQL order, or `None` when the SQL could
    /// not be analysed (see `errors`).
    pub output: Option<OutputSlots<ColumnSource>>,
    /// Number of output slots listed in `output`. Unexpanded star segments are
    /// not counted, so this is not the width of the model.
    pub listed_columns: usize,
    /// Listed slots whose value lineage is `Complete`, constants included.
    pub value_complete_columns: usize,
    /// Listed slots whose value lineage is `Partial`.
    pub value_partial_columns: usize,
    /// Resource boundaries encountered while tracing output values.
    pub boundaries: Vec<TraceBoundary>,
    pub errors: Vec<ColumnLineageError>,
}

impl ModelColumnLineage {
    /// Recompute the three counts from `output`. All are 0 without an output.
    pub fn recount(&mut self) {
        let (listed, complete, partial) =
            self.output.as_ref().map_or((0, 0, 0), OutputSlots::counts);
        self.listed_columns = listed;
        self.value_complete_columns = complete;
        self.value_partial_columns = partial;
    }

    /// Keep only the output slots that the requested column names select, and
    /// the diagnostics of those slots.
    ///
    /// Each name is looked up with the dialect's identifier rules. A name that
    /// matches several slots selects none of them and is reported as
    /// ambiguous. A name that matches no slot is reported as not found, or as
    /// indeterminate when an unexpanded star may provide it. Nothing changes
    /// when the model has no output.
    pub fn select_columns(&mut self, requested: &[&str], dialect: DlinDialect) {
        let Some(output) = self.output.as_mut() else {
            return;
        };
        let names: BTreeSet<&str> = requested.iter().copied().collect();
        let mut keep = BTreeSet::new();
        let mut diagnostics = Vec::new();
        for name in names {
            let lookup = output.lookup(&Ident::parse_reference(name), dialect);
            if let SlotLookup::Unique(id) = lookup {
                keep.insert(id);
            }
            diagnostics.extend(output.lookup_error(name, &lookup));
        }
        output.columns.retain(|slot| keep.contains(&slot.id));
        self.boundaries.retain(|boundary| {
            boundary.root.unique_id != self.unique_id || keep.contains(&boundary.root.slot)
        });
        // Keep the model-level diagnostics and those of the selected slots.
        // A declared-only column has no slot, and a requested name that
        // matched nothing is reported by the lookup instead.
        self.errors.retain(|error| match error.slot() {
            Some(slot) => keep.contains(&slot),
            None => !error.is_column_scoped(),
        });
        // A model that failed as a whole already explains why nothing was
        // found; an ambiguous name is still reported.
        let failed = self.errors.iter().any(ColumnLineageError::is_model_failure);
        self.errors.extend(
            diagnostics
                .into_iter()
                .filter(|error| !failed || error.kind == ColumnLineageErrorKind::ColumnAmbiguous),
        );
        self.recount();
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

// --- Output slots ---

/// Position of a slot within one model result: its index in
/// [`OutputSlots::columns`] when the result was built. It is not a persistent
/// identity and is separate from the SQL position of the column.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(transparent)]
pub struct OutputSlotId(usize);

impl OutputSlotId {
    pub fn new(index: usize) -> Self {
        Self(index)
    }

    pub fn index(self) -> usize {
        self.0
    }
}

impl fmt::Display for OutputSlotId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// The name of an output column, as the SQL gave it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(tag = "kind", content = "ident", rename_all = "snake_case")]
pub enum OutputName {
    /// Named by an alias or a target column list.
    Explicit(Ident),
    /// Named after the column or field the expression reads.
    Inferred(Ident),
    /// The SQL gives the expression no name.
    Anonymous,
}

impl OutputName {
    /// The identifier, unless the output is anonymous.
    pub fn ident(&self) -> Option<&Ident> {
        match self {
            Self::Explicit(ident) | Self::Inferred(ident) => Some(ident),
            Self::Anonymous => None,
        }
    }
}

/// Display form of the name. An anonymous output is shown as `?column?`; that
/// text is for display only and never identifies a column.
impl fmt::Display for OutputName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ident() {
            Some(ident) => ident.fmt(f),
            None => f.write_str("?column?"),
        }
    }
}

/// One output column of a model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputSlot<S> {
    pub id: OutputSlotId,
    /// Zero-based SQL position, when the engine proved it. It is `None` for a
    /// column after an unexpanded star.
    pub ordinal: Option<usize>,
    pub name: OutputName,
    pub transformation: TransformationType,
    /// Known sources of the value, and what remains unresolved.
    pub lineage: ValueLineage<S>,
}

/// A star projection that could not be expanded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnexpandedStar {
    /// Relation the star reads, or `None` when it is not known.
    pub relation: Option<RelationRef>,
    /// Ordinal of the last known output column before the star, or `None`
    /// when no earlier column has a known position.
    pub after_ordinal: Option<usize>,
}

/// Whether the listed slots are every output column.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputShape {
    /// Every output column is listed.
    Closed,
    /// Unexpanded stars may contribute columns that are not listed.
    Open { unexpanded: Vec<UnexpandedStar> },
}

/// The output slots of one model, in SQL order, with duplicates and quoting
/// kept.
///
/// `S` is the representation of a source column: the public result and the
/// internal result differ only there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputSlots<S> {
    pub columns: Vec<OutputSlot<S>>,
    pub shape: OutputShape,
}

/// Result of looking a column name up in the slots of a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotLookup {
    /// Exactly one slot has this name.
    Unique(OutputSlotId),
    /// More than one slot has this name. None of them is chosen.
    Ambiguous(Vec<OutputSlotId>),
    /// No listed slot has this name. When the shape is open, an unexpanded
    /// star may still provide it.
    Absent,
}

impl<S> OutputSlots<S> {
    /// The slot with this id.
    pub fn slot(&self, id: OutputSlotId) -> Option<&OutputSlot<S>> {
        self.columns
            .get(id.index())
            .filter(|slot| slot.id == id)
            .or_else(|| self.columns.iter().find(|slot| slot.id == id))
    }

    /// Find the slots named `name`, comparing names by the dialect's column
    /// identifier rules. Anonymous slots never match.
    pub fn lookup(&self, name: &Ident, dialect: DlinDialect) -> SlotLookup {
        let Ok(engine) = dialect.to_engine_dialect() else {
            return SlotLookup::Absent;
        };
        let wanted = name.engine_key(engine);
        let matches: Vec<OutputSlotId> = self
            .columns
            .iter()
            .filter(|slot| {
                slot.name
                    .ident()
                    .is_some_and(|ident| ident.engine_key(engine) == wanted)
            })
            .map(|slot| slot.id)
            .collect();
        match matches.as_slice() {
            [] => SlotLookup::Absent,
            [id] => SlotLookup::Unique(*id),
            _ => SlotLookup::Ambiguous(matches),
        }
    }

    /// The diagnostic for a lookup of `name` that did not find exactly one
    /// slot: ambiguous when several slots have the name, not found when the
    /// shape is closed, and indeterminate when an unexpanded star may provide
    /// it. `None` when the lookup found a slot.
    pub(crate) fn lookup_error(
        &self,
        name: &str,
        lookup: &SlotLookup,
    ) -> Option<ColumnLineageError> {
        match lookup {
            SlotLookup::Unique(_) => None,
            SlotLookup::Ambiguous(ids) => Some(ColumnLineageError::for_name(
                ColumnLineageErrorKind::ColumnAmbiguous,
                name,
                &format!(
                    "{} output columns have this name; it does not select one of them",
                    ids.len()
                ),
                None,
            )),
            SlotLookup::Absent if self.shape == OutputShape::Closed => {
                Some(ColumnLineageError::for_name(
                    ColumnLineageErrorKind::ColumnNotFound,
                    name,
                    "not found in model output",
                    None,
                ))
            }
            SlotLookup::Absent => Some(ColumnLineageError::for_name(
                ColumnLineageErrorKind::ColumnIndeterminate,
                name,
                "not among the enumerated model outputs",
                Some(OPEN_SHAPE_HINT.to_string()),
            )),
        }
    }

    /// Listed, value-complete and value-partial slot counts.
    fn counts(&self) -> (usize, usize, usize) {
        let complete = self
            .columns
            .iter()
            .filter(|slot| slot.lineage.is_complete())
            .count();
        (self.columns.len(), complete, self.columns.len() - complete)
    }
}

/// A source column reference
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ColumnSource {
    /// Source table/model name as it appears in SQL (e.g. "stg_orders", "`raw`.`orders`")
    pub table: String,
    /// Source column name, with the quote style it was written with.
    pub column: Ident,
    /// Cross-model path: the model outputs traversed to reach this source, as
    /// hops ordered from the target model outward toward the leaf source.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_path: Vec<ModelPathHop>,
}

/// Internal source representation. The public `ColumnSource.table` is a
/// rendered string; analysis and cache code keeps the structural relation
/// until the final conversion boundary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct InternalColumnSource {
    pub relation: RelationRef,
    pub column: Ident,
    pub model_path: Vec<ModelPathHop>,
}

/// The internal form of a model result.
///
/// Unknown fields are rejected so that a cache entry written in an older
/// layout fails to parse instead of reading as a result without output.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct InternalModelColumnLineage {
    pub unique_id: String,
    pub model: String,
    pub output: Option<OutputSlots<InternalColumnSource>>,
    pub boundaries: Vec<TraceBoundary>,
    pub errors: Vec<ColumnLineageError>,
}

impl InternalModelColumnLineage {
    /// A result for a model whose SQL was not analysed.
    pub(crate) fn failed(unique_id: &str, model: &str, error: ColumnLineageError) -> Self {
        Self {
            unique_id: unique_id.to_string(),
            model: model.to_string(),
            output: None,
            boundaries: Vec::new(),
            errors: vec![error],
        }
    }

    pub(crate) fn into_public(self) -> ModelColumnLineage {
        let mut public = ModelColumnLineage {
            unique_id: self.unique_id,
            model: self.model,
            output: self.output.map(|output| OutputSlots {
                columns: output
                    .columns
                    .into_iter()
                    .map(|slot| OutputSlot {
                        id: slot.id,
                        ordinal: slot.ordinal,
                        name: slot.name,
                        transformation: slot.transformation,
                        lineage: slot.lineage.map_known(|source| ColumnSource {
                            table: source.relation.render(),
                            column: source.column,
                            model_path: source.model_path,
                        }),
                    })
                    .collect(),
                shape: output.shape,
            }),
            boundaries: self.boundaries,
            listed_columns: 0,
            value_complete_columns: 0,
            value_partial_columns: 0,
            errors: normalize_column_lineage_errors(self.errors),
        };
        public.recount();
        public
    }
}

/// A stop point reached while tracing an output through manifest resources.
/// `at` is always the real output slot that contains the relation reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceBoundary {
    /// Root result slot whose trace reached this boundary.
    pub root: ModelOutputRef,
    /// Output slot that contains the relation reference (the root for direct
    /// boundaries such as a constant output).
    pub at: ModelOutputRef,
    pub reason: TraceBoundaryReason,
    pub resource: TraceResource,
    pub model_path: Vec<ModelPathHop>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TraceBoundaryReason {
    ExternalRelation,
    SourceFree,
    AnalysisFailed,
    Cycle,
    MissingDefinition,
    AmbiguousModel,
    UnsupportedMaterialization,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TraceResourceKind {
    Model,
    Source,
    Seed,
    Snapshot,
    PhysicalRelation,
}

/// Fixed tracing policy. Results record the policy used for classification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum TraceBoundaryPolicy {
    SnapshotUnsupportedSeedExternal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceResourceCandidate {
    pub unique_id: String,
    pub name: String,
    pub kind: TraceResourceKind,
    pub relation: RelationRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceResource {
    pub unique_id: Option<String>,
    pub name: String,
    pub kind: TraceResourceKind,
    pub relation: RelationRef,
    pub column: Option<Ident>,
    pub candidates: Vec<TraceResourceCandidate>,
    pub policy: TraceBoundaryPolicy,
}

/// Collapse parseable per-column diagnostics at a public output boundary.
///
/// A single column can produce the same column-scoped finding through more than
/// one resolver path. Keep the first group's position, selecting the hinted
/// diagnostic when the other diagnostic identity fields are equal.
pub(crate) fn normalize_column_lineage_errors(
    errors: Vec<ColumnLineageError>,
) -> Vec<ColumnLineageError> {
    type GroupKey = (
        Option<OutputSlotId>,
        String,
        ColumnLineageErrorKind,
        String,
        Option<String>,
    );
    let mut normalized = Vec::with_capacity(errors.len());
    let mut groups = HashMap::<GroupKey, usize>::new();

    for error in errors {
        let Some(column) = error.column_name() else {
            normalized.push(error);
            continue;
        };
        let key = (
            error.slot(),
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

// --- Value lineage ---

/// Identity of one output slot of a dbt model.
///
/// `unique_id` and `slot` are the identity; `model` and `column` are the
/// display names used by the rest of the report. Two slots with the same
/// display name differ in `slot`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelOutputRef {
    pub unique_id: String,
    pub model: String,
    pub slot: OutputSlotId,
    pub column: String,
}

/// One hop of a cross-model path: the model output that was traversed and the
/// transformation that produced it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModelPathHop {
    pub output: ModelOutputRef,
    pub transformation: TransformationType,
}

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
        column: Ident,
    },
    /// A column could come from more than one candidate.
    Ambiguous {
        column: Ident,
        candidates: Vec<UnresolvedCandidate>,
    },
    /// A column read from another model matches more than one of that model's
    /// output columns, so none of them is chosen.
    AmbiguousOutput {
        relation: RelationRef,
        column: Ident,
        candidates: Vec<ModelOutputRef>,
    },
    /// A closed model output does not contain the referenced column.
    AbsentOutput {
        relation: RelationRef,
        column: Ident,
    },
    /// A column could not be bound to a relation in scope.
    Unbound { column: Ident },
    /// Recursive lineage is represented by its known base sources only.
    Recursive,
    /// A whole-row alias is used where a column source is expected.
    RowValue { alias: String },
    /// The syntax is not modelled by the engine.
    Unsupported { what: String },
    /// Tracing stopped at a manifest resource boundary that is incomplete.
    TraceBoundary {
        reason: TraceBoundaryReason,
        resource: TraceResource,
    },
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
            Self::AmbiguousOutput {
                relation,
                column,
                candidates,
            } => write!(
                f,
                "{column} matches {} output columns of {relation}",
                candidates.len()
            ),
            Self::AbsentOutput { relation, column } => {
                write!(f, "'{column}' is not an output of {relation}")
            }
            Self::Unbound { column } => write!(f, "'{column}' is not bound to a relation"),
            Self::Recursive => write!(f, "recursive lineage is limited to its base sources"),
            Self::RowValue { alias } => write!(f, "whole-row value '{alias}'"),
            Self::Unsupported { what } => write!(f, "unsupported syntax: {what}"),
            Self::TraceBoundary { reason, resource } => {
                write!(f, "{reason:?} at {}", resource.name)
            }
        }
    }
}

/// A concrete relation column named by an unresolved reason.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnresolvedSource {
    pub relation: RelationRef,
    pub column: Ident,
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
            slot: None,
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
                slot: None,
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
                slot: None,
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
            slot: None,
            what: "row value could not be expanded".to_string(),
            why: None,
            hint: None,
        };
        let message_only = ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnNotFound,
            column: None,
            slot: None,
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
