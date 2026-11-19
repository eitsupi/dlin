use std::collections::BTreeMap;

use crate::catalog::CatalogContext;
use crate::keys::{candidate_key, relation_key, sort_dedup_by_key, source_key, unresolved_key};
use crate::types::{
    ColumnMapping, ColumnOrigin, Evidence, Origin, SemanticError, StatementType, TableLineage,
    TableRef, TransformKind, Warning, WarningKind,
};
use crate::{
    Candidate, ColumnEvidence, Diagnostic, DiagnosticKind, Dialect, EvidenceRef, Ident, Lineage,
    Location, Output, OutputColumn, OutputName, Relation, RowDependencies, Severity, Shape,
    SourceColumn, SourceEvidence, StatementAnalysis, StatementKind, Transform, UnexpandedStar,
    Unresolved, UnresolvedEvidence,
};

/// The structure evidence of each mapping and the evidence of the output's
/// shape, which come from the models the mappings were evaluated from.
#[derive(Default)]
pub(super) struct Structure {
    /// Parallel to the mappings.
    pub columns: Vec<Evidence>,
    pub shape: Evidence,
}

/// The evidence the catalog entries of `evidence`'s relations carry.
fn references(evidence: &Evidence, catalog: Option<&CatalogContext<'_>>) -> Vec<EvidenceRef> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    let mut references: Vec<EvidenceRef> = evidence
        .relations()
        .filter_map(|table| catalog.entry(table))
        .flat_map(|entry| entry.evidence.into_owned())
        .collect();
    references.sort_by(|left, right| left.0.cmp(&right.0));
    references.dedup();
    references
}

/// Assemble resolved output slots without revisiting the SQL syntax tree.
#[allow(clippy::too_many_arguments)]
pub(super) fn assemble(
    mappings: &[ColumnMapping],
    structure: &Structure,
    tables: &TableLineage,
    target_columns: &[Ident],
    warnings: &[Warning],
    kind: &StatementType,
    has_unresolved_stars: bool,
    dialect: Dialect,
    catalog: Option<&CatalogContext<'_>>,
) -> StatementAnalysis {
    let read_order = tables
        .inputs
        .iter()
        .map(|table| {
            catalog
                .and_then(|catalog| catalog.entry(table))
                .map_or_else(|| plain_relation(table), |entry| entry.relation)
        })
        .collect::<Vec<_>>();
    let mut columns = Vec::new();
    let mut unexpanded = Vec::new();
    let exact_width = mappings.iter().all(|mapping| mapping.output_name.is_some());
    let rename =
        !target_columns.is_empty() && exact_width && target_columns.len() == mappings.len();
    for (index, mapping) in mappings.iter().enumerate() {
        let mut known = Vec::new();
        let mut unresolved = Vec::new();
        for origin in &mapping.sources.origins {
            contribute(origin, catalog, &mut known, &mut unresolved);
        }
        let (lineage, known, unresolved) =
            normalize_lineage(known, unresolved, dialect, &read_order);
        let evidence = ColumnEvidence {
            structure: structure
                .columns
                .get(index)
                .map_or_default(|evidence| references(evidence, catalog)),
            lineage: references(&mapping.sources.all_evidence(), catalog),
            known: known
                .into_iter()
                .map(|(source, evidence)| SourceEvidence {
                    source,
                    evidence: references(&evidence, catalog),
                })
                .filter(|source| !source.evidence.is_empty())
                .collect(),
            unresolved: unresolved
                .into_iter()
                .map(|(unresolved, evidence)| UnresolvedEvidence {
                    unresolved,
                    evidence: references(&evidence, catalog),
                })
                .filter(|unresolved| !unresolved.evidence.is_empty())
                .collect(),
        };
        if let Some(name) = &mapping.output_name {
            columns.push(OutputColumn {
                ordinal: mapping.ordinal,
                name: if rename {
                    OutputName::Explicit(target_columns[index].clone())
                } else {
                    name.clone()
                },
                lineage,
                transform: transform(&mapping.transform),
                evidence,
            });
        } else {
            let mut relations = Vec::new();
            star_relations(&mapping.sources.origins, &mut relations);
            // One interval can reach the same relation through several
            // operand paths. Those paths do not publish additional intervals.
            let mut seen = Vec::new();
            relations.retain(|relation| {
                let key = relation
                    .as_ref()
                    .map(|relation| relation_key(relation, dialect));
                if seen.contains(&key) {
                    false
                } else {
                    seen.push(key);
                    true
                }
            });
            if relations.is_empty() {
                relations.push(None);
            }
            for relation in relations {
                unexpanded.push(UnexpandedStar {
                    relation,
                    after_ordinal: match mapping.star_position {
                        crate::types::StarPosition::Inferred => {
                            columns.last().and_then(|column| column.ordinal)
                        }
                        crate::types::StarPosition::Unproven => None,
                        crate::types::StarPosition::After(after) => Some(after),
                    },
                    lineage: lineage.clone(),
                    evidence: evidence.clone(),
                });
            }
        }
    }
    let mut diagnostics = warnings
        .iter()
        .map(|warning| Diagnostic {
            severity: Severity::Error,
            kind: match warning.kind {
                WarningKind::UnexpectedCycle => DiagnosticKind::UnexpectedCycle,
            },
            location: warning.location.as_ref().map(|loc| Location {
                line: loc.line,
                column: loc.column,
            }),
        })
        .collect::<Vec<_>>();
    if !target_columns.is_empty() && exact_width && target_columns.len() != mappings.len() {
        diagnostics.push(Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::TargetColumnCountMismatch {
                target: target_columns.len(),
                source: mappings.len(),
            },
            location: None,
        });
    }
    let mut names: BTreeMap<String, (Ident, usize, Vec<usize>)> = BTreeMap::new();
    for column in &columns {
        if let OutputName::Explicit(id) | OutputName::Inferred(id) = &column.name {
            let group = names
                .entry(id.column_key(dialect).into_owned())
                .or_insert_with(|| (id.clone(), 0, Vec::new()));
            group.1 += 1;
            if let Some(ordinal) = column.ordinal {
                group.2.push(ordinal);
            }
        }
    }
    for (_, (name, count, ordinals)) in names {
        if count > 1 {
            diagnostics.push(Diagnostic {
                severity: Severity::Warning,
                kind: DiagnosticKind::DuplicateOutputName { name, ordinals },
                location: None,
            });
        }
    }
    let mut read = read_order;
    sort_dedup_by_key(&mut read, |relation| relation_key(relation, dialect));
    StatementAnalysis {
        kind: statement_kind(kind),
        written: tables.output.as_ref().map(plain_relation),
        read,
        output: Output {
            columns,
            shape: if unexpanded.is_empty() {
                Shape::Closed
            } else {
                Shape::Open { unexpanded }
            },
            evidence: references(&structure.shape, catalog),
        },
        has_unresolved_stars,
        diagnostics,
        row_dependencies: RowDependencies::NotCollected,
    }
}

/// A contribution with the evidence it relies on.
type Evidenced<T> = (T, Evidence);

fn contribute(
    origin: &Origin,
    catalog: Option<&CatalogContext<'_>>,
    known: &mut Vec<Evidenced<SourceColumn>>,
    unresolved: &mut Vec<Evidenced<Unresolved>>,
) {
    let evidence = &origin.evidence;
    let mut unresolved_item = |item| unresolved.push((item, evidence.clone()));
    match &origin.kind {
        ColumnOrigin::Concrete { table, column } => known.push((
            SourceColumn {
                relation: catalog
                    .and_then(|catalog| catalog.entry(table))
                    .map_or_else(|| plain_relation(table), |entry| entry.relation),
                column: catalog
                    .and_then(|catalog| catalog.column(table, column))
                    .unwrap_or_else(|| column.clone()),
            },
            evidence.clone(),
        )),
        ColumnOrigin::Ambiguous { column, candidates } => {
            unresolved_item(if candidates.is_empty() {
                Unresolved::Unbound {
                    column: column.clone(),
                }
            } else {
                Unresolved::Ambiguous {
                    column: column.clone(),
                    candidates: candidates
                        .iter()
                        .map(|table| Candidate::Relation(plain_relation(table)))
                        .collect(),
                }
            });
        }
        ColumnOrigin::UnknownStar => unresolved_item(Unresolved::Unsupported {
            what: "star expansion",
        }),
        ColumnOrigin::Unresolved(item) => unresolved_item(item.clone()),
        ColumnOrigin::Wildcard { table } => unresolved_item(Unresolved::BehindStar {
            relation: plain_relation(table),
        }),
        ColumnOrigin::NamedWildcard { table, column } => {
            unresolved_item(Unresolved::NamedBehindStar {
                relation: plain_relation(table),
                column: column.clone(),
            });
        }
        ColumnOrigin::SourceFree { .. } => {}
        ColumnOrigin::Recursive { base_sources } => {
            for source in base_sources {
                contribute(&source.clone().with(evidence), catalog, known, unresolved);
            }
            unresolved.push((Unresolved::Recursive, evidence.clone()));
        }
    }
}

fn star_relations(origins: &[Origin], result: &mut Vec<Option<Relation>>) {
    for origin in origins {
        match &origin.kind {
            ColumnOrigin::Wildcard { table } => result.push(Some(plain_relation(table))),
            ColumnOrigin::Recursive { base_sources } => star_relations(base_sources, result),
            _ => {}
        }
    }
}

/// The lineage of an output, with the evidence of each contribution that is
/// left after duplicates are merged. A contribution reached along several
/// paths relies on the evidence of each of them.
fn normalize_lineage(
    mut known: Vec<Evidenced<SourceColumn>>,
    unresolved: Vec<Evidenced<Unresolved>>,
    dialect: Dialect,
    read_order: &[Relation],
) -> (
    Lineage,
    Vec<Evidenced<SourceColumn>>,
    Vec<Evidenced<Unresolved>>,
) {
    // Keep the first spelling encountered along this output's contribution
    // paths. Catalog-resolved sources already carry canonical column names.
    for (source, _) in &mut known {
        retain_first_relation_spelling(&mut source.relation, read_order, dialect);
    }
    // A stable sort keeps the first spelling of equal keys in front.
    known.sort_by(|(left, _), (right, _)| {
        source_key(left, dialect).cmp(&source_key(right, dialect))
    });
    let mut unique_known: Vec<Evidenced<SourceColumn>> = Vec::new();
    for (source, evidence) in known {
        match unique_known.last_mut() {
            Some((seen, seen_evidence))
                if source_key(seen, dialect) == source_key(&source, dialect) =>
            {
                seen_evidence.extend(&evidence);
            }
            _ => unique_known.push((source, evidence)),
        }
    }
    let mut unique: Vec<Evidenced<Unresolved>> = Vec::new();
    for (mut item, evidence) in unresolved {
        normalize_unresolved(&mut item, read_order, dialect);
        match unique
            .iter_mut()
            .find(|(seen, _)| unresolved_key(seen, dialect) == unresolved_key(&item, dialect))
        {
            Some((_, seen_evidence)) => seen_evidence.extend(&evidence),
            None => unique.push((item, evidence)),
        }
    }
    let sources = unique_known
        .iter()
        .map(|(source, _)| source.clone())
        .collect();
    let lineage = if unique.is_empty() {
        Lineage::Complete(sources)
    } else {
        Lineage::Partial {
            known: sources,
            unresolved: unique.iter().map(|(item, _)| item.clone()).collect(),
        }
    };
    (lineage, unique_known, unique)
}

fn retain_first_relation_spelling(
    relation: &mut Relation,
    read_order: &[Relation],
    dialect: Dialect,
) {
    if let Some(first) = read_order
        .iter()
        .find(|first| relation_key(first, dialect) == relation_key(relation, dialect))
    {
        relation.clone_from(first);
    }
}

fn normalize_unresolved(unresolved: &mut Unresolved, read_order: &[Relation], dialect: Dialect) {
    match unresolved {
        Unresolved::AlignmentUnknown {
            possible_sources,
            unknown_operands,
            ..
        } => {
            for source in possible_sources.iter_mut() {
                retain_first_relation_spelling(&mut source.relation, read_order, dialect);
            }
            sort_dedup_by_key(possible_sources, |source| source_key(source, dialect));
            unknown_operands.sort_unstable();
            unknown_operands.dedup();
        }
        Unresolved::BehindStar { relation } | Unresolved::NamedBehindStar { relation, .. } => {
            retain_first_relation_spelling(relation, read_order, dialect);
        }
        Unresolved::SchemaDependent { relations, .. } => {
            for relation in relations.iter_mut() {
                retain_first_relation_spelling(relation, read_order, dialect);
            }
            sort_dedup_by_key(relations, |relation| relation_key(relation, dialect));
        }
        Unresolved::Ambiguous { candidates, .. } => {
            for candidate in candidates.iter_mut() {
                if let crate::Candidate::Relation(relation) = candidate {
                    retain_first_relation_spelling(relation, read_order, dialect);
                }
            }
            sort_dedup_by_key(candidates, |candidate| candidate_key(candidate, dialect));
        }
        Unresolved::Unverified {
            present_candidates,
            possible_candidates,
            ..
        } => {
            for candidates in [&mut *present_candidates, &mut *possible_candidates] {
                for candidate in candidates.iter_mut() {
                    match candidate {
                        crate::Candidate::Relation(relation) => {
                            retain_first_relation_spelling(relation, read_order, dialect);
                        }
                        crate::Candidate::ArrayElement(element) => {
                            for source in &mut element.array_sources {
                                retain_first_relation_spelling(
                                    &mut source.relation,
                                    read_order,
                                    dialect,
                                );
                            }
                            sort_dedup_by_key(&mut element.array_sources, |source| {
                                source_key(source, dialect)
                            });
                        }
                        crate::Candidate::Cte(_) | crate::Candidate::Derived(_) => {}
                    }
                }
                sort_dedup_by_key(candidates, |candidate| candidate_key(candidate, dialect));
            }
        }
        _ => {}
    }
}

pub(crate) fn semantic_failure(
    kind: StatementKind,
    written: Option<Relation>,
    mut read: Vec<Relation>,
    error: &SemanticError,
    mut diagnostics: Vec<Diagnostic>,
    dialect: Dialect,
) -> StatementAnalysis {
    let diagnostic = match error {
        SemanticError::SetOperationArityMismatch { left, right } => {
            DiagnosticKind::SetOperationArityMismatch {
                left: *left,
                right: *right,
            }
        }
        SemanticError::PipeOperators => DiagnosticKind::UnsupportedSyntax {
            what: "pipe operators",
        },
        SemanticError::UnpivotExpression => DiagnosticKind::UnsupportedSyntax {
            what: "UNPIVOT on an expression",
        },
        SemanticError::ValuesRowArityMismatch { first, row } => {
            DiagnosticKind::ValuesRowArityMismatch {
                first: *first,
                row: *row,
            }
        }
    };
    sort_dedup_by_key(&mut read, |relation| relation_key(relation, dialect));
    diagnostics.insert(
        0,
        Diagnostic {
            severity: Severity::Error,
            kind: diagnostic,
            location: None,
        },
    );
    StatementAnalysis {
        kind,
        written,
        read,
        output: Output {
            columns: Vec::new(),
            shape: Shape::Closed,
            evidence: Vec::new(),
        },
        has_unresolved_stars: false,
        diagnostics,
        row_dependencies: RowDependencies::NotCollected,
    }
}

pub(crate) fn statement_kind(kind: &StatementType) -> StatementKind {
    match kind {
        StatementType::Query => StatementKind::Query,
        StatementType::Insert => StatementKind::Insert,
        StatementType::CreateTable => StatementKind::CreateTableAs,
        StatementType::Update => StatementKind::Update,
        StatementType::Delete => StatementKind::Delete,
        StatementType::Merge => StatementKind::Merge,
        StatementType::Other => StatementKind::Other,
    }
}

fn transform(transform: &TransformKind) -> Transform {
    match transform {
        TransformKind::Direct => Transform::Direct,
        TransformKind::Expression => Transform::Expression,
        TransformKind::Aggregation => Transform::Aggregation,
        TransformKind::Conditional => Transform::Conditional,
        TransformKind::Window => Transform::Window,
        TransformKind::Unknown => Transform::Unknown,
    }
}

pub(crate) fn plain_relation(table: &TableRef) -> Relation {
    Relation {
        catalog: table.catalog.clone(),
        schema: table.schema.clone(),
        namespace: table.namespace,
        name: table.table.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Quote, SetCorrespondence, SetOperand, SetOperationKind, SetOperator, SetQuantifier,
    };

    #[test]
    fn alignment_candidates_follow_dialect_keys_and_first_relation_spelling() {
        let relation = |name| Relation {
            catalog: None,
            schema: None,
            namespace: None,
            name: Ident::unquoted(name),
        };
        let candidate = |table, column| SourceColumn {
            relation: relation(table),
            column,
        };
        let mut reason = Unresolved::AlignmentUnknown {
            operation: SetOperationKind {
                operator: SetOperator::Union,
                quantifier: SetQuantifier::All,
                correspondence: SetCorrespondence::Positional,
            },
            possible_sources: vec![
                candidate("t", Ident::unquoted("X")),
                candidate("T", Ident::unquoted("x")),
                candidate("t", Ident::quoted("x", Quote::Double)),
            ],
            unknown_operands: vec![SetOperand::Right, SetOperand::Left, SetOperand::Right],
        };
        normalize_unresolved(&mut reason, &[relation("T")], Dialect::Snowflake);
        let Unresolved::AlignmentUnknown {
            possible_sources,
            unknown_operands,
            ..
        } = reason
        else {
            unreachable!();
        };
        assert_eq!(possible_sources.len(), 2);
        assert!(
            possible_sources
                .iter()
                .all(|source| source.relation == relation("T"))
        );
        assert_eq!(possible_sources[0].column, Ident::unquoted("X"));
        assert_eq!(
            possible_sources[1].column,
            Ident::quoted("x", Quote::Double)
        );
        assert_eq!(unknown_operands, [SetOperand::Left, SetOperand::Right]);
    }
}
