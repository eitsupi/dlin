//! Typed schema evidence: what a column list claims, where it came from and
//! on what basis its coverage is assumed.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::graph::column_lineage::backend::DlinDialect;
use crate::parser::manifest::{ManifestColumn, ManifestContractConfig};

/// Stable identifier of one piece of schema evidence, such as
/// `yaml:source.p.raw.orders` or `sql:model.p.orders`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EvidenceId(pub(crate) String);

impl EvidenceId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EvidenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a column list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaOrigin {
    /// Output columns inferred from the model's SQL.
    SqlOutput,
    /// A schema supplied by a catalog, such as a user-provided schema string.
    CatalogArtifact,
    /// Columns declared in dbt YAML.
    YamlDeclaration,
    /// The header of a seed file in the project.
    SeedFile,
}

/// Why a schema is treated as covering every column of the relation.
///
/// `Enumerated` only says the list came from an enumeration. It does not by
/// itself imply that the coverage is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageBasis {
    /// The source enumerated the columns.
    Enumerated,
    /// A YAML list is treated as exhaustive because the default policy says so.
    AssumedByYamlPolicy,
    /// A dbt model contract with `enforced: true` lists the columns.
    DeclaredEnforcedContract,
}

/// Whether a declared contract is known to apply to the built relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnforcementApplicability {
    /// A declaration is not validation, so applicability is not established.
    Unknown,
}

/// What is known about the enforcement of a declared model contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContractEnforcementEvidence {
    pub declared: bool,
    pub applicability: EnforcementApplicability,
}

/// Whether a schema lists every column of its relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaCoverage {
    /// Unlisted names are absent.
    Exhaustive,
    /// Unlisted names may exist.
    Partial,
}

/// Whether a schema proves the physical order of its columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaOrder {
    Proven,
    Unknown,
}

/// The description of one piece of schema evidence that a result cites:
/// what the schema claims about its relation and on what basis.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SchemaEvidenceRecord {
    pub id: EvidenceId,
    /// The dbt unique id (or relation name) the schema describes.
    pub subject: String,
    pub origin: SchemaOrigin,
    pub coverage: SchemaCoverage,
    pub coverage_basis: CoverageBasis,
    pub order: SchemaOrder,
    pub enforcement: Option<ContractEnforcementEvidence>,
    /// The evidence a schema inferred from SQL relied on for its columns,
    /// their positions and whether it has more.
    pub derived_from: Vec<EvidenceId>,
}

impl SchemaEvidenceRecord {
    /// Whether exhaustive coverage is assumed rather than enumerated.
    pub fn is_assumption(&self) -> bool {
        self.coverage == SchemaCoverage::Exhaustive
            && matches!(
                self.coverage_basis,
                CoverageBasis::AssumedByYamlPolicy | CoverageBasis::DeclaredEnforcedContract
            )
    }
}

/// One column of a schema, in the order the source listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidencedColumn {
    pub(crate) name: sqllineage::Ident,
    /// The raw `quote` setting; unset and `false` stay distinct.
    pub(crate) quote_specified: Option<bool>,
    /// Proven zero-based position, when the source establishes one.
    pub(crate) ordinal: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SchemaEvidence {
    pub(crate) id: EvidenceId,
    /// The dbt unique id (or relation name) the schema describes.
    pub(crate) subject: String,
    pub(crate) origin: SchemaOrigin,
    pub(crate) coverage: sqllineage::SchemaCoverage,
    pub(crate) coverage_basis: CoverageBasis,
    pub(crate) order: sqllineage::SchemaOrder,
    /// Declaration or output order. Duplicates are kept.
    pub(crate) columns: Vec<EvidencedColumn>,
    pub(crate) unknown_segments: Vec<sqllineage::CatalogUnknownSegment>,
    pub(crate) enforcement: Option<ContractEnforcementEvidence>,
    /// The evidence a schema inferred from SQL relied on for which columns it
    /// lists and whether it has more. It is closed under derivation: the
    /// evidence of an upstream SQL schema's own derivation is included.
    pub(crate) derived_from: Vec<EvidenceId>,
    /// The records of `derived_from`, so that a reader of this schema can
    /// describe them. Each has no records of its own; their `derived_from`
    /// ids are all among `derived_from` here.
    pub(crate) derived_records: Vec<SchemaEvidence>,
}

impl SchemaEvidence {
    /// Whether both schemas make the same claim about the relation: same
    /// coverage and order, the same column identifiers with the same
    /// ordinals, and the same unknown segments. Evidence identity, provenance
    /// and the display order of the columns are not part of the claim.
    pub(crate) fn same_claim_as(&self, other: &Self) -> bool {
        fn column_keys(schema: &SchemaEvidence) -> Vec<(&sqllineage::Ident, Option<usize>)> {
            let mut keys: Vec<_> = schema
                .columns
                .iter()
                .map(|column| (&column.name, column.ordinal))
                .collect();
            keys.sort_unstable();
            keys
        }
        fn segment_keys(schema: &SchemaEvidence) -> Vec<Option<usize>> {
            let mut keys: Vec<_> = schema
                .unknown_segments
                .iter()
                .map(|segment| segment.after_ordinal)
                .collect();
            keys.sort_unstable();
            keys
        }
        self.coverage == other.coverage
            && self.order == other.order
            && column_keys(self) == column_keys(other)
            && segment_keys(self) == segment_keys(other)
    }

    /// Whether one schema only adds a proven order to what the other already
    /// lists. This holds when both are exhaustive without unknown segments,
    /// each names its columns uniquely, both name exactly the same identifiers,
    /// and one has unknown order with no ordinals while the other has proven
    /// order with ordinals forming exactly `0..n`. The relation is symmetric.
    pub(crate) fn is_order_refinement_pair(&self, other: &Self) -> bool {
        fn sorted_names(schema: &SchemaEvidence) -> Option<Vec<&sqllineage::Ident>> {
            let mut names: Vec<_> = schema.columns.iter().map(|column| &column.name).collect();
            names.sort_unstable();
            names
                .windows(2)
                .all(|pair| pair[0] != pair[1])
                .then_some(names)
        }
        fn is_unordered(schema: &SchemaEvidence) -> bool {
            schema.order == sqllineage::SchemaOrder::Unknown
                && schema.columns.iter().all(|column| column.ordinal.is_none())
        }
        fn is_fully_ordered(schema: &SchemaEvidence) -> bool {
            if schema.order != sqllineage::SchemaOrder::Proven {
                return false;
            }
            let mut ordinals: Vec<_> = schema.columns.iter().map(|column| column.ordinal).collect();
            ordinals.sort_unstable();
            ordinals
                .iter()
                .enumerate()
                .all(|(position, ordinal)| *ordinal == Some(position))
        }
        let exhaustive = |schema: &SchemaEvidence| {
            schema.coverage == sqllineage::SchemaCoverage::Exhaustive
                && schema.unknown_segments.is_empty()
        };
        if !exhaustive(self) || !exhaustive(other) {
            return false;
        }
        let (unordered, ordered) = match (is_unordered(self), is_unordered(other)) {
            (true, false) => (self, other),
            (false, true) => (other, self),
            _ => return false,
        };
        let names = sorted_names(unordered);
        is_fully_ordered(ordered) && names.is_some() && names == sorted_names(ordered)
    }

    /// An ordered, exhaustive enumeration with proven ordinals `0..n`.
    fn enumerated(
        id: String,
        subject: &str,
        origin: SchemaOrigin,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            id: EvidenceId(id),
            subject: subject.to_owned(),
            origin,
            coverage: sqllineage::SchemaCoverage::Exhaustive,
            coverage_basis: CoverageBasis::Enumerated,
            order: sqllineage::SchemaOrder::Proven,
            columns: names
                .into_iter()
                .enumerate()
                .map(|(ordinal, name)| EvidencedColumn {
                    name: sqllineage::Ident::unquoted(name),
                    quote_specified: None,
                    ordinal: Some(ordinal),
                })
                .collect(),
            unknown_segments: Vec::new(),
            enforcement: None,
            derived_from: Vec::new(),
            derived_records: Vec::new(),
        }
    }

    /// The public description of this schema.
    pub(crate) fn record(&self) -> SchemaEvidenceRecord {
        SchemaEvidenceRecord {
            id: self.id.clone(),
            subject: self.subject.clone(),
            origin: self.origin,
            coverage: match self.coverage {
                sqllineage::SchemaCoverage::Exhaustive => SchemaCoverage::Exhaustive,
                sqllineage::SchemaCoverage::Partial => SchemaCoverage::Partial,
            },
            coverage_basis: self.coverage_basis,
            order: match self.order {
                sqllineage::SchemaOrder::Proven => SchemaOrder::Proven,
                sqllineage::SchemaOrder::Unknown => SchemaOrder::Unknown,
            },
            enforcement: self.enforcement,
            derived_from: self.derived_from.clone(),
        }
    }

    /// Every evidence id a reader of this schema relies on: its own and
    /// those it was derived from.
    pub(crate) fn reliance(&self) -> impl Iterator<Item = &EvidenceId> {
        std::iter::once(&self.id).chain(&self.derived_from)
    }

    /// This record and the records it was derived from, each without
    /// records of its own.
    pub(crate) fn records(&self) -> impl Iterator<Item = SchemaEvidence> + '_ {
        std::iter::once(Self {
            derived_records: Vec::new(),
            ..self.clone()
        })
        .chain(self.derived_records.iter().cloned())
    }

    /// The header of a seed file is the seed's exhaustive, ordered column
    /// list as the project defines it.
    pub(crate) fn seed_file(
        subject: &str,
        names: impl IntoIterator<Item = String>,
        quote_columns: Option<bool>,
        dialect: DlinDialect,
    ) -> Self {
        let mut evidence = Self::enumerated(
            format!("seed_file:{subject}"),
            subject,
            SchemaOrigin::SeedFile,
            names,
        );
        for column in &mut evidence.columns {
            column.quote_specified = quote_columns;
            if quote_columns == Some(true) {
                column.name =
                    sqllineage::Ident::quoted(&column.name.value, quote_for_dialect(dialect));
            }
        }
        evidence
    }

    /// A user-typed schema string is an explicit, ordered enumeration.
    pub(crate) fn user_enumeration(
        relation_name: &str,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        Self::enumerated(
            format!("user:{relation_name}"),
            relation_name,
            SchemaOrigin::CatalogArtifact,
            names,
        )
    }
}

fn quote_for_dialect(dialect: DlinDialect) -> sqllineage::Quote {
    match dialect {
        DlinDialect::BigQuery => sqllineage::Quote::Backtick,
        _ => sqllineage::Quote::Double,
    }
}

/// Evidence for the columns a model or source declares in YAML.
///
/// The list is assumed to be exhaustive. A model contract with
/// `enforced: true` gives the coverage its own basis. The YAML order is kept
/// for display only, so the schema order is `Unknown` and no ordinal is set.
pub(crate) fn yaml_schema_evidence(
    subject: &str,
    columns: &IndexMap<String, ManifestColumn>,
    contract: Option<&ManifestContractConfig>,
    dialect: DlinDialect,
) -> Option<SchemaEvidence> {
    if columns.is_empty() {
        return None;
    }
    let enforced = contract.is_some_and(|contract| contract.enforced);
    let quote = quote_for_dialect(dialect);
    Some(SchemaEvidence {
        id: EvidenceId(format!("yaml:{subject}")),
        subject: subject.to_owned(),
        origin: SchemaOrigin::YamlDeclaration,
        coverage: sqllineage::SchemaCoverage::Exhaustive,
        coverage_basis: if enforced {
            CoverageBasis::DeclaredEnforcedContract
        } else {
            CoverageBasis::AssumedByYamlPolicy
        },
        order: sqllineage::SchemaOrder::Unknown,
        columns: columns
            .iter()
            .map(|(name, column)| EvidencedColumn {
                name: if column.quote == Some(true) {
                    sqllineage::Ident::quoted(name.clone(), quote)
                } else {
                    sqllineage::Ident::unquoted(name.clone())
                },
                quote_specified: column.quote,
                ordinal: None,
            })
            .collect(),
        unknown_segments: Vec::new(),
        enforcement: enforced.then_some(ContractEnforcementEvidence {
            declared: true,
            applicability: EnforcementApplicability::Unknown,
        }),
        derived_from: Vec::new(),
        derived_records: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_columns_preserve_explicit_quoting_and_header_order() {
        for quote_columns in [None, Some(false), Some(true)] {
            let evidence = SchemaEvidence::seed_file(
                "seed.project.categories",
                ["EventSource".to_owned(), "category".to_owned()],
                quote_columns,
                DlinDialect::Snowflake,
            );
            assert_eq!(evidence.columns[0].ordinal, Some(0));
            assert_eq!(evidence.columns[1].ordinal, Some(1));
            let column = &evidence.columns[0];
            assert_eq!(column.quote_specified, quote_columns);
            let expected = if quote_columns == Some(true) {
                sqllineage::Ident::quoted("EventSource", sqllineage::Quote::Double)
            } else {
                sqllineage::Ident::unquoted("EventSource")
            };
            assert_eq!(column.name, expected);
        }
    }

    fn columns(names: &[&str]) -> IndexMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                        quote: None,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn yaml_evidence_keeps_declaration_order_without_ordinals() {
        let evidence =
            yaml_schema_evidence("m", &columns(&["z", "a"]), None, DlinDialect::Generic).unwrap();

        assert_eq!(evidence.origin, SchemaOrigin::YamlDeclaration);
        assert_eq!(evidence.coverage, sqllineage::SchemaCoverage::Exhaustive);
        assert_eq!(evidence.coverage_basis, CoverageBasis::AssumedByYamlPolicy);
        assert_eq!(evidence.order, sqllineage::SchemaOrder::Unknown);
        assert_eq!(evidence.enforcement, None);
        let names: Vec<_> = evidence
            .columns
            .iter()
            .map(|c| c.name.value.as_str())
            .collect();
        assert_eq!(names, ["z", "a"]);
        assert!(evidence.columns.iter().all(|c| c.ordinal.is_none()));
    }

    #[test]
    fn enforced_contract_has_its_own_basis_but_no_proven_order() {
        let contract = ManifestContractConfig { enforced: true };
        let evidence =
            yaml_schema_evidence("m", &columns(&["a"]), Some(&contract), DlinDialect::Generic)
                .unwrap();

        assert_eq!(
            evidence.coverage_basis,
            CoverageBasis::DeclaredEnforcedContract
        );
        assert_eq!(evidence.order, sqllineage::SchemaOrder::Unknown);
        assert_eq!(
            evidence.enforcement,
            Some(ContractEnforcementEvidence {
                declared: true,
                applicability: EnforcementApplicability::Unknown,
            })
        );

        let not_enforced = ManifestContractConfig { enforced: false };
        let evidence = yaml_schema_evidence(
            "m",
            &columns(&["a"]),
            Some(&not_enforced),
            DlinDialect::Generic,
        )
        .unwrap();
        assert_eq!(evidence.coverage_basis, CoverageBasis::AssumedByYamlPolicy);
        assert_eq!(evidence.enforcement, None);
    }

    #[test]
    fn empty_declaration_yields_no_evidence() {
        assert_eq!(
            yaml_schema_evidence("m", &columns(&[]), None, DlinDialect::Generic),
            None
        );
    }
}
