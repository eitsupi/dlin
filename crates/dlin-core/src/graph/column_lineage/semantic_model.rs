//! Typed semantic information for a compiled query's output.
//!
//! This module deliberately does not model scopes, relations, or projection
//! expressions.  Those semantics belong to polyglot-sql; this layer only makes
//! the ordered `QueryOutput` explicit for consumers that need names or
//! ordinals.

use std::collections::HashMap;

use polyglot_sql::{DialectType, Expression, MappingSchema, OutputColumn, QueryOutput};

/// A zero-based ordinal in the final query output.
///
/// This is intentionally a distinct type.  In particular, there is no
/// conversion from a projection-list index to this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputOrdinal(usize);

/// One output slot whose ordinal is known to the upstream analyzer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOutput {
    ordinal: OutputOrdinal,
    name: Option<String>,
}

impl ResolvedOutput {
    /// Return the proven output ordinal.
    pub fn ordinal(&self) -> OutputOrdinal {
        self.ordinal
    }

    /// Return the output name when the database/analyzer supplied one.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// Why a name cannot be mapped to one proven output slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedReason {
    /// An unresolved wildcard may contribute an unknown number of slots.
    UnresolvedWildcard,
    /// The output name is known, but its ordinal is not.
    OrdinalUnknown,
    /// An unnamed output may be the requested name, but supplies no name.
    UnnamedOutput,
    /// The upstream output is not complete enough to prove absence.
    OrdinalIncomplete,
}

/// A named output candidate, including a possibly unknown ordinal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputCandidate {
    name: String,
    ordinal: Option<OutputOrdinal>,
}

impl OutputCandidate {
    /// Return the candidate's original output name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the candidate's ordinal when it is known.
    pub fn ordinal(&self) -> Option<OutputOrdinal> {
        self.ordinal
    }
}

/// Result of resolving a requested output name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameResolution {
    Unique(ResolvedOutput),
    Ambiguous(Vec<OutputCandidate>),
    Absent,
    Indeterminate(UnresolvedReason),
}

#[derive(Debug, Clone)]
enum CatalogEntry {
    Resolved,
    NamedWithoutOrdinal,
    Unnamed,
    Wildcard,
}

/// An ordered, typed view of an upstream [`QueryOutput`].
///
/// `ResolvedOutput` values are created only while constructing this catalog;
/// callers cannot manufacture one from an arbitrary integer or projection
/// index. This holds only because the `QueryOutput` fed into construction is
/// itself proven by polyglot-sql: the constructors that accept a raw
/// `QueryOutput` are private to this module, reachable only through
/// [`build_query_model_from_sql`].
#[derive(Debug, Clone)]
pub struct OutputCatalog {
    entries: Vec<CatalogEntry>,
    resolved_outputs: Vec<ResolvedOutput>,
    named: HashMap<String, Vec<OutputCandidate>>,
    has_unresolved_wildcard: bool,
    ordinal_complete: bool,
    dialect: DialectType,
}

impl OutputCatalog {
    /// Resolve a name without guessing across an unknown output region.
    pub fn resolve_name(&self, name: &str) -> NameResolution {
        let normalized =
            polyglot_sql::schema::normalize_name(name, Some(self.dialect), false, true);

        if let Some(candidates) = self.named.get(&normalized) {
            if candidates.len() != 1 {
                return NameResolution::Ambiguous(candidates.clone());
            }

            let candidate = &candidates[0];
            return match candidate.ordinal {
                Some(ordinal) => NameResolution::Unique(ResolvedOutput {
                    ordinal,
                    name: Some(candidate.name.clone()),
                }),
                None => NameResolution::Indeterminate(UnresolvedReason::OrdinalUnknown),
            };
        }

        if self
            .resolved_outputs
            .iter()
            .any(|output| output.name.is_none())
            || self
                .entries
                .iter()
                .any(|entry| matches!(entry, CatalogEntry::Unnamed))
        {
            return NameResolution::Indeterminate(UnresolvedReason::UnnamedOutput);
        }
        if self.has_unresolved_wildcard {
            return NameResolution::Indeterminate(UnresolvedReason::UnresolvedWildcard);
        }
        if !self.ordinal_complete {
            return NameResolution::Indeterminate(UnresolvedReason::OrdinalIncomplete);
        }
        NameResolution::Absent
    }

    /// Return all slots with a known ordinal, preserving query output order.
    pub fn resolved_outputs(&self) -> &[ResolvedOutput] {
        &self.resolved_outputs
    }

    /// Return names only when the complete output order is proven.
    pub(crate) fn proven_ordered_named_outputs(&self) -> Option<Vec<String>> {
        if !self.ordinal_complete
            || self.has_unresolved_wildcard
            || self.entries.len() != self.resolved_outputs.len()
        {
            return None;
        }

        let mut names = Vec::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            if !matches!(entry, CatalogEntry::Resolved) {
                return None;
            }
            let output = &self.resolved_outputs[index];
            if output.ordinal != OutputOrdinal(index) {
                return None;
            }
            let name = output.name.as_ref()?;
            names.push(name.clone());
        }

        let mut normalized = names
            .iter()
            .map(|name| polyglot_sql::schema::normalize_name(name, Some(self.dialect), false, true))
            .collect::<Vec<_>>();
        normalized.sort_unstable();
        normalized.dedup();
        (normalized.len() == names.len()).then_some(names)
    }
}

/// Build an output catalog from polyglot-sql's ordered output description.
///
/// Not exposed outside this module: a caller could otherwise hand-build a
/// `QueryOutput` with arbitrary ordinals and receive back `ResolvedOutput`
/// values this module treats as proven. Ordinals must come from polyglot-sql
/// via [`build_query_model_from_sql`].
fn build_output_catalog(query: QueryOutput, dialect: DialectType) -> OutputCatalog {
    let mut entries = Vec::with_capacity(query.columns.len());
    let mut resolved_outputs = Vec::new();
    let mut named: HashMap<String, Vec<OutputCandidate>> = HashMap::new();
    let mut has_unresolved_wildcard = false;

    for column in query.columns {
        match column {
            OutputColumn::Named { name, ordinal } => {
                let ordinal = ordinal.map(OutputOrdinal);
                let candidate = OutputCandidate {
                    name: name.clone(),
                    ordinal,
                };
                let normalized =
                    polyglot_sql::schema::normalize_name(&name, Some(dialect), false, true);
                named.entry(normalized).or_default().push(candidate.clone());

                if let Some(ordinal) = ordinal {
                    let output = ResolvedOutput {
                        ordinal,
                        name: Some(name),
                    };
                    resolved_outputs.push(output.clone());
                    entries.push(CatalogEntry::Resolved);
                } else {
                    entries.push(CatalogEntry::NamedWithoutOrdinal);
                }
            }
            OutputColumn::Unnamed { ordinal } => {
                if let Some(ordinal) = ordinal.map(OutputOrdinal) {
                    let output = ResolvedOutput {
                        ordinal,
                        name: None,
                    };
                    resolved_outputs.push(output.clone());
                    entries.push(CatalogEntry::Resolved);
                } else {
                    entries.push(CatalogEntry::Unnamed);
                }
            }
            OutputColumn::Wildcard { .. } => {
                has_unresolved_wildcard = true;
                entries.push(CatalogEntry::Wildcard);
            }
        }
    }

    OutputCatalog {
        entries,
        resolved_outputs,
        named,
        has_unresolved_wildcard,
        ordinal_complete: query.ordinal_complete,
        dialect,
    }
}

/// A query plus its typed output semantics.
#[derive(Debug, Clone)]
pub struct QueryModel {
    pub expression: Expression,
    pub outputs: OutputCatalog,
    pub ordinal_schema: Option<MappingSchema>,
    pub dialect: DialectType,
}

/// Assemble a query model from an expression and its upstream output result.
///
/// Not exposed outside this module for the same reason as
/// [`build_output_catalog`]: `query` must come from polyglot-sql, not be
/// assembled by the caller. Use [`build_query_model_from_sql`] instead.
fn build_query_model(
    expression: Expression,
    query: QueryOutput,
    ordinal_schema: Option<MappingSchema>,
    dialect: DialectType,
) -> QueryModel {
    QueryModel {
        expression,
        outputs: build_output_catalog(query, dialect),
        ordinal_schema,
        dialect,
    }
}

/// Parse a compiled query and ask polyglot-sql for its ordered output.
pub fn build_query_model_from_sql(
    sql: &str,
    dialect: DialectType,
    ordinal_schema: Option<MappingSchema>,
) -> Result<QueryModel, polyglot_sql::Error> {
    let expression = polyglot_sql::parse_one(sql, dialect)?;
    let query = match ordinal_schema.as_ref() {
        Some(schema) => polyglot_sql::lineage::output_columns_with_schema(
            &expression,
            Some(schema as &dyn polyglot_sql::Schema),
            Some(dialect),
        )?,
        None => polyglot_sql::lineage::output_columns(&expression, Some(dialect))?,
    };
    Ok(build_query_model(
        expression,
        query,
        ordinal_schema,
        dialect,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(sql: &str) -> QueryModel {
        build_query_model_from_sql(sql, DialectType::Generic, None).unwrap()
    }

    #[test]
    fn ordinal_is_known_before_an_unresolved_star_only() {
        let outputs = &model("SELECT id, *, tail FROM unknown_source").outputs;

        assert!(matches!(
            outputs.resolve_name("id"),
            NameResolution::Unique(ResolvedOutput {
                ordinal: OutputOrdinal(0),
                ..
            })
        ));
        assert_eq!(
            outputs.resolve_name("tail"),
            NameResolution::Indeterminate(UnresolvedReason::OrdinalUnknown)
        );
        assert_eq!(
            outputs.resolve_name("new_name"),
            NameResolution::Indeterminate(UnresolvedReason::UnresolvedWildcard)
        );
    }

    #[test]
    fn duplicate_names_are_ambiguous_after_normalization() {
        let outputs = &model("SELECT a.id, b.id FROM a JOIN b ON a.id = b.id").outputs;

        let NameResolution::Ambiguous(candidates) = outputs.resolve_name("ID") else {
            panic!("duplicate output names must not be guessed")
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].ordinal, Some(OutputOrdinal(0)));
        assert_eq!(candidates[1].ordinal, Some(OutputOrdinal(1)));
    }

    #[test]
    fn unnamed_output_keeps_its_ordinal_without_a_yaml_name() {
        let outputs = &model("SELECT fee * 2 FROM payments").outputs;
        assert_eq!(outputs.resolved_outputs().len(), 1);
        assert_eq!(outputs.resolved_outputs()[0].ordinal, OutputOrdinal(0));
        assert_eq!(outputs.resolved_outputs()[0].name, None);
        assert_eq!(
            outputs.resolve_name("calculated_fee"),
            NameResolution::Indeterminate(UnresolvedReason::UnnamedOutput)
        );
    }

    #[test]
    fn set_output_uses_only_the_leftmost_operand_names() {
        let outputs =
            &model("SELECT left_id FROM left_table UNION SELECT right_id FROM right_table").outputs;

        assert!(matches!(
            outputs.resolve_name("left_id"),
            NameResolution::Unique(ResolvedOutput {
                ordinal: OutputOrdinal(0),
                ..
            })
        ));
        assert_eq!(outputs.resolve_name("right_id"), NameResolution::Absent);
    }

    #[test]
    fn complete_empty_catalog_reports_absence() {
        let outputs = build_output_catalog(
            QueryOutput {
                columns: vec![],
                ordinal_complete: true,
            },
            DialectType::Generic,
        );
        assert_eq!(outputs.resolve_name("anything"), NameResolution::Absent);
    }
}
