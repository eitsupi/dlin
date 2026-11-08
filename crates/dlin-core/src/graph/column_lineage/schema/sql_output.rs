//! The output schema of a model's SQL, kept in output order.

use super::evidence::{CoverageBasis, EvidenceId, EvidencedColumn, SchemaEvidence, SchemaOrigin};

/// One output slot of a statement. Duplicate names stay separate slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlOutputColumn {
    /// Zero-based output position, when the engine established it.
    pub(crate) ordinal: Option<usize>,
    pub(crate) name: sqllineage::OutputName,
}

/// The ordered output columns of one analysed statement and whether the list
/// is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqlOutputSchema {
    pub(crate) columns: Vec<SqlOutputColumn>,
    pub(crate) shape: sqllineage::Shape,
}

impl SqlOutputSchema {
    pub(crate) fn from_output(output: &sqllineage::Output) -> Self {
        Self {
            columns: output
                .columns
                .iter()
                .map(|column| SqlOutputColumn {
                    ordinal: column.ordinal,
                    name: column.name.clone(),
                })
                .collect(),
            shape: output.shape.clone(),
        }
    }

    /// The schema as evidence a downstream analysis can rely on.
    ///
    /// A closed shape whose slots are all named is exhaustive, and its order
    /// is proven when every slot has an ordinal. An unexpanded star, or a slot
    /// without a name, cannot be listed: it becomes an unknown segment and the
    /// schema is partial with unknown order. Named columns keep the ordinal the
    /// engine gave them, so a known prefix stays known.
    pub(crate) fn to_evidence(&self, subject: &str) -> SchemaEvidence {
        let mut columns = Vec::new();
        let mut unknown_segments = Vec::new();
        let mut previous_ordinal = None;
        for column in &self.columns {
            match &column.name {
                sqllineage::OutputName::Explicit(name) | sqllineage::OutputName::Inferred(name) => {
                    columns.push(EvidencedColumn {
                        name: name.clone(),
                        quote_specified: Some(name.quote.is_some()),
                        ordinal: column.ordinal,
                    });
                }
                sqllineage::OutputName::Anonymous => {
                    unknown_segments.push(sqllineage::CatalogUnknownSegment {
                        after_ordinal: previous_ordinal,
                    });
                }
            }
            previous_ordinal = column.ordinal;
        }
        if let sqllineage::Shape::Open { unexpanded } = &self.shape {
            unknown_segments.extend(unexpanded.iter().map(|star| {
                sqllineage::CatalogUnknownSegment {
                    after_ordinal: star.after_ordinal,
                }
            }));
        }
        unknown_segments.sort_by_key(|segment| segment.after_ordinal);

        let exhaustive = unknown_segments.is_empty();
        let ordered = exhaustive && columns.iter().all(|column| column.ordinal.is_some());
        SchemaEvidence {
            id: EvidenceId(format!("sql:{subject}")),
            subject: subject.to_owned(),
            origin: SchemaOrigin::SqlOutput,
            coverage: if exhaustive {
                sqllineage::SchemaCoverage::Exhaustive
            } else {
                sqllineage::SchemaCoverage::Partial
            },
            coverage_basis: CoverageBasis::Enumerated,
            order: if ordered {
                sqllineage::SchemaOrder::Proven
            } else {
                sqllineage::SchemaOrder::Unknown
            },
            columns,
            unknown_segments,
            enforcement: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::column_lineage::backend::DlinDialect;
    use crate::graph::column_lineage::backend::sqllineage::{
        analyze_sql, single_lineage_statement,
    };

    fn evidence(sql: &str) -> SchemaEvidence {
        let analysis = analyze_sql(sql, DlinDialect::Generic, None).unwrap();
        let statement = single_lineage_statement(&analysis).unwrap();
        SqlOutputSchema::from_output(&statement.output).to_evidence("model.p.m")
    }

    fn named(evidence: &SchemaEvidence) -> Vec<(&str, Option<usize>)> {
        evidence
            .columns
            .iter()
            .map(|column| (column.name.value.as_str(), column.ordinal))
            .collect()
    }

    #[test]
    fn closed_named_output_is_exhaustive_with_proven_order() {
        let evidence = evidence("SELECT z, a AS b, z FROM t");

        assert_eq!(evidence.id, EvidenceId("sql:model.p.m".into()));
        assert_eq!(evidence.origin, SchemaOrigin::SqlOutput);
        assert_eq!(evidence.coverage_basis, CoverageBasis::Enumerated);
        assert_eq!(evidence.coverage, sqllineage::SchemaCoverage::Exhaustive);
        assert_eq!(evidence.order, sqllineage::SchemaOrder::Proven);
        assert!(evidence.unknown_segments.is_empty());
        assert_eq!(
            named(&evidence),
            [("z", Some(0)), ("b", Some(1)), ("z", Some(2))]
        );
    }

    #[test]
    fn quoted_output_name_keeps_its_quote() {
        let evidence = evidence(r#"SELECT t."Id" FROM t"#);

        assert_eq!(evidence.columns[0].name.value, "Id");
        assert!(evidence.columns[0].name.quote.is_some());
        assert_eq!(evidence.columns[0].quote_specified, Some(true));
    }

    #[test]
    fn unexpanded_star_makes_the_schema_partial_and_keeps_the_prefix() {
        let evidence = evidence("SELECT k, t.* FROM unknown_t t");

        assert_eq!(evidence.coverage, sqllineage::SchemaCoverage::Partial);
        assert_eq!(evidence.order, sqllineage::SchemaOrder::Unknown);
        assert_eq!(named(&evidence), [("k", Some(0))]);
        assert_eq!(
            evidence.unknown_segments,
            [sqllineage::CatalogUnknownSegment {
                after_ordinal: Some(0)
            }]
        );
    }

    #[test]
    fn anonymous_output_is_an_unknown_segment_not_an_invented_name() {
        let evidence = evidence("SELECT a, 1 + 1, b FROM t");

        assert_eq!(evidence.coverage, sqllineage::SchemaCoverage::Partial);
        assert_eq!(evidence.order, sqllineage::SchemaOrder::Unknown);
        assert_eq!(named(&evidence), [("a", Some(0)), ("b", Some(2))]);
        assert_eq!(
            evidence.unknown_segments,
            [sqllineage::CatalogUnknownSegment {
                after_ordinal: Some(0)
            }]
        );
    }

    #[test]
    fn leading_anonymous_output_has_no_known_predecessor() {
        let evidence = evidence("SELECT 1 + 1, a FROM t");

        assert_eq!(named(&evidence), [("a", Some(1))]);
        assert_eq!(
            evidence.unknown_segments,
            [sqllineage::CatalogUnknownSegment {
                after_ordinal: None
            }]
        );
    }
}
