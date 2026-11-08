//! Typed schema evidence: what a column list claims, where it came from and
//! on what basis its coverage is assumed.

use indexmap::IndexMap;

use crate::graph::column_lineage::backend::DlinDialect;
use crate::parser::manifest::{ManifestColumn, ManifestContractConfig};

/// Stable identifier of one piece of schema evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceId(pub(crate) String);

/// Where a column list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchemaOrigin {
    /// Output columns inferred from the model's SQL.
    SqlOutput,
    /// A schema supplied by a catalog, such as a user-provided schema string.
    CatalogArtifact,
    /// Columns declared in dbt YAML.
    YamlDeclaration,
}

/// Why a schema is treated as covering every column of the relation.
///
/// `Enumerated` only says the list came from an enumeration. It does not by
/// itself imply that the coverage is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoverageBasis {
    Enumerated,
    /// A YAML list is treated as exhaustive because the default policy says so.
    AssumedByYamlPolicy,
    /// A dbt model contract with `enforced: true` lists the columns.
    DeclaredEnforcedContract,
}

/// Whether a declared contract is known to apply to the built relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnforcementApplicability {
    /// A declaration is not validation, so applicability is not established.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ContractEnforcementEvidence {
    pub(crate) declared: bool,
    pub(crate) applicability: EnforcementApplicability,
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
}

impl SchemaEvidence {
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
        }
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
