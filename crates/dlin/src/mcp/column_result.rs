//! Presentation of column analysis for MCP clients.

use std::collections::BTreeSet;
use std::ops::Range;

use anyhow::{Result, bail};
use dlin_core::graph::column_lineage::{
    ColumnImpactReport, ColumnLineageError, ColumnLineageErrorKind, EvidenceId, ExplorationStatus,
    ModelColumnLineage, ModelOutputRef, OutputShape, SchemaEvidenceRecord, TraceBoundaryReason,
};
use serde_json::{Value, json};

use super::{page::Page, tools::excerpt};

#[derive(Clone, Copy, Hash)]
pub(super) enum Detail {
    Summary,
    Full,
}

impl Detail {
    pub(super) fn parse(args: &Value) -> Result<Self> {
        match args.get("detail") {
            None | Some(Value::Null) => Ok(Self::Summary),
            Some(detail) => match detail.as_str() {
                Some("summary") => Ok(Self::Summary),
                Some("full") => Ok(Self::Full),
                _ => bail!("detail must be 'summary' or 'full'"),
            },
        }
    }

    pub(super) fn full(self) -> bool {
        matches!(self, Self::Full)
    }
}

type ColumnQuery<'a> = (u64, &'a str, &'a str, &'a str, Detail);

/// Slice consecutive groups of rows with a single page budget.
struct Rows {
    range: Range<usize>,
    offset: usize,
}

impl Rows {
    fn new(page: &Page) -> Self {
        Self {
            range: page.range(),
            offset: 0,
        }
    }

    fn take<'a, T>(&mut self, values: &'a [T]) -> &'a [T] {
        let len = values.len();
        let start = self.range.start.saturating_sub(self.offset).min(len);
        let end = self.range.end.saturating_sub(self.offset).min(len);
        self.offset += len;
        &values[start..end]
    }
}

#[derive(Default)]
struct Evidence<'a> {
    ids: BTreeSet<&'a EvidenceId>,
}

impl<'a> Evidence<'a> {
    fn add(&mut self, ids: impl IntoIterator<Item = &'a EvidenceId>) {
        self.ids.extend(ids);
    }

    fn project(mut self, ledger: &'a [SchemaEvidenceRecord], full: bool) -> Value {
        let mut pending: Vec<_> = self.ids.iter().copied().collect();
        let mut records = Vec::new();
        while let Some(id) = pending.pop() {
            if let Some(record) = ledger.iter().find(|record| &record.id == id) {
                pending.extend(record.derived_from.iter().filter(|id| self.ids.insert(id)));
                records.push(record);
            }
        }
        if full {
            records.sort_by_key(|record| &record.id);
            json!(records)
        } else {
            let kinds: BTreeSet<_> = records
                .into_iter()
                .map(|record| {
                    (
                        record.origin,
                        record.coverage,
                        record.coverage_basis,
                        record.is_assumption(),
                    )
                })
                .collect();
            json!(kinds.into_iter().map(|(origin, coverage, basis, assumption)| {
                json!({"origin":origin, "coverage":coverage, "basis":basis, "assumption":assumption})
            }).collect::<Vec<_>>())
        }
    }
}

fn errors(page: &[ColumnLineageError], full: bool) -> Value {
    if full {
        json!(page)
    } else {
        json!(
            page.iter()
                .map(|error| json!({
                    "kind":error.kind, "what":excerpt(&error.what, 160),
                }))
                .collect::<Vec<_>>()
        )
    }
}

fn output_ref(output: &ModelOutputRef) -> Value {
    json!({"model":output.model, "column":output.column, "slot":output.slot})
}

pub(super) fn upstream(
    args: &Value,
    (manifest_hash, requested_model, column, direction, detail): ColumnQuery<'_>,
    report: &ModelColumnLineage,
) -> Result<Value> {
    let full = detail.full();
    let output = report.output.as_ref();
    let slots = output
        .map(|output| output.columns.as_slice())
        .unwrap_or_default();
    let stars = output.and_then(|output| match &output.shape {
        OutputShape::Open { unexpanded } => Some(unexpanded.as_slice()),
        OutputShape::Closed => None,
    });
    let known_count: usize = slots.iter().map(|slot| slot.lineage.known().len()).sum();
    let unresolved_count: usize = slots
        .iter()
        .map(|slot| slot.lineage.unresolved().len())
        .sum();
    let total = known_count
        + unresolved_count
        + report.boundaries.len()
        + report.errors.len()
        + stars.map_or(0, |stars| stars.len());
    let page = Page::new(
        args,
        20,
        total,
        manifest_hash,
        (
            "get_column_lineage",
            requested_model,
            column,
            direction,
            detail,
        ),
    )?;
    let mut rows = Rows::new(&page);
    let mut evidence = Evidence::default();
    if let Some(output) = output {
        evidence.add(&output.evidence);
    }
    let mut known = Vec::new();
    let mut unresolved = Vec::new();
    for slot in slots {
        let slot_known = rows.take(slot.lineage.known());
        let slot_unresolved = rows.take(slot.lineage.unresolved());
        evidence.add(&slot.evidence.structure);
        let all_slot_values_on_page = slot_known.len() == slot.lineage.known().len()
            && slot_unresolved.len() == slot.lineage.unresolved().len();
        if all_slot_values_on_page {
            evidence.add(&slot.evidence.lineage);
        }
        for source in slot_known {
            evidence.add(&source.evidence);
            known.push(if full {
                json!({"slot":slot.id, "source":source})
            } else {
                json!({"slot":slot.id, "table":source.table, "column":source.column.to_string()})
            });
        }
        for entry in slot_unresolved {
            evidence.add(&entry.evidence);
            unresolved.push(if full {
                json!({"slot":slot.id, "contribution":entry})
            } else {
                json!({"slot":slot.id, "reason":compact_reason(serde_json::to_value(&entry.reason)?)})
            });
        }
    }
    let boundaries = rows.take(&report.boundaries);
    for boundary in boundaries {
        evidence.add(&boundary.evidence);
    }
    let boundary_page = if full {
        json!(boundaries)
    } else {
        json!(boundaries.iter().map(|boundary| json!({
            "slot":boundary.root.slot, "reason":boundary.reason, "resource":boundary.resource.name,
            "column":boundary.resource.column.as_ref().map(ToString::to_string),
        })).collect::<Vec<_>>())
    };
    let error_page = errors(rows.take(&report.errors), full);
    let star_page = rows.take(stars.unwrap_or_default());
    let status = if report
        .errors
        .iter()
        .any(|error| error.kind == ColumnLineageErrorKind::ModelNotFound)
    {
        "not_found"
    } else if report
        .errors
        .iter()
        .any(|error| error.kind == ColumnLineageErrorKind::ModelAmbiguous)
    {
        "ambiguous"
    } else if output.is_none() {
        "analysis_failed"
    } else if slots.is_empty() {
        if report
            .errors
            .iter()
            .any(|error| error.kind == ColumnLineageErrorKind::ColumnAmbiguous)
        {
            "ambiguous"
        } else if stars.is_some() {
            "partial"
        } else {
            "not_found"
        }
    } else if unresolved_count > 0 {
        "partial"
    } else {
        "complete"
    };
    let incomplete_boundary = report.boundaries.iter().any(|boundary| {
        !matches!(
            boundary.reason,
            TraceBoundaryReason::ExternalRelation | TraceBoundaryReason::SourceFree
        )
    });
    let note = match status {
        "partial" => Some("Some value sources could not be verified."),
        "complete" if known_count == 0 => Some("The value has no upstream source."),
        _ => None,
    };
    let mut value = json!({
        "model": report.model, "column":column, "status":status,
        "known_count":known_count, "unresolved_count":unresolved_count,
        "known":known, "unresolved":unresolved, "boundaries":boundary_page,
        "incomplete_boundary":incomplete_boundary,
        "errors":error_page, "evidence":evidence.project(&report.evidence, full), "page":page.metadata(),
    });
    if let Some(note) = note {
        value["note"] = json!(note);
    }
    if full {
        value["unique_id"] = json!(report.unique_id);
        value["slots"] = json!(
            slots
                .iter()
                .map(|slot| json!({
                    "id":slot.id, "name":slot.name, "ordinal":slot.ordinal,
                    "transformation":slot.transformation, "structure_evidence":slot.evidence.structure,
                }))
                .collect::<Vec<_>>()
        );
        value["unexpanded"] = json!(star_page);
    } else if let Some(stars) = stars {
        value["unexpanded_count"] = json!(stars.len());
        value["unexpanded_on_page"] = json!(star_page.len());
    }
    Ok(value)
}

/// Keep the kind and useful context while bounding lists of candidates.
fn compact_reason(mut reason: Value) -> Value {
    if reason["kind"] == "trace_boundary" {
        // The boundary rows carry the detailed stop point.
        return json!({"kind":"trace_boundary", "resource":reason["resource"]["name"]});
    }
    if let Some(fields) = reason.as_object_mut() {
        let mut omitted = 0;
        for value in fields.values_mut() {
            if let Some(items) = value.as_array_mut() {
                omitted += items.len().saturating_sub(3);
                items.truncate(3);
            }
        }
        if omitted > 0 {
            fields.insert("omitted_items".into(), json!(omitted));
        }
    }
    reason
}

pub(super) fn downstream(
    args: &Value,
    (manifest_hash, model, column, direction, detail): ColumnQuery<'_>,
    report: &ColumnImpactReport,
) -> Result<Value> {
    let full = detail.full();
    let gap_count: usize = report
        .exploration
        .iter()
        .map(|entry| match &entry.status {
            ExplorationStatus::Complete => 0,
            ExplorationStatus::Incomplete { gaps } => gaps.len(),
        })
        .sum();
    let page = Page::new(
        args,
        20,
        report.confirmed.len() + report.possible.len() + gap_count + report.errors.len(),
        manifest_hash,
        ("get_column_lineage", model, column, direction, detail),
    )?;
    let mut rows = Rows::new(&page);
    let mut evidence = Evidence::default();
    let confirmed = rows.take(&report.confirmed);
    let possible = rows.take(&report.possible);
    for impact in confirmed {
        evidence.add(&impact.evidence);
    }
    for impact in possible {
        evidence.add(&impact.evidence);
    }
    let confirmed = if full {
        json!(confirmed)
    } else {
        json!(
            confirmed
                .iter()
                .map(|impact| output_ref(&impact.target))
                .collect::<Vec<_>>()
        )
    };
    let possible = if full {
        json!(possible)
    } else {
        json!(
            possible
                .iter()
                .map(|impact| output_ref(&impact.target))
                .collect::<Vec<_>>()
        )
    };
    let exploration: Vec<_> = report.exploration.iter().map(|entry| {
        evidence.add(&entry.evidence);
        let mut value = match &entry.status {
            ExplorationStatus::Complete => json!({"kind":entry.kind, "status":"complete"}),
            ExplorationStatus::Incomplete { gaps } => {
                let selected = rows.take(gaps);
                let selected = if full { json!(selected) } else {
                    json!(selected.iter().map(|gap| {
                        let gap = json!(gap);
                        let mut summary = json!({"type":gap["type"]});
                        for key in ["model", "column", "error"] {
                            if let Some(field) = gap.get(key) {
                                summary[key] = field.clone();
                            }
                        }
                        if let Some(at) = gap.get("at") {
                            summary["at"] = json!({"model":at["model"], "column":at["column"]});
                        }
                        summary
                    }).collect::<Vec<_>>())
                };
                json!({"kind":entry.kind, "status":"incomplete", "gap_count":gaps.len(), "gaps":selected})
            }
        };
        if full && !entry.evidence.is_empty() {
            value["evidence"] = json!(entry.evidence);
        }
        value
    }).collect();
    let mut value = json!({
        "model":report.model, "column":report.column, "scope":report.scope,
        "input":report.input, "confirmed":confirmed, "possible":possible,
        "confirmed_count":report.confirmed.len(), "possible_count":report.possible.len(),
        "exploration":exploration, "no_impact":report.proves_no_impact(),
        "errors":errors(rows.take(&report.errors), full),
        "evidence":evidence.project(&report.evidence, full), "page":page.metadata(),
    });
    if gap_count > 0 {
        value["note"] = json!("Incomplete exploration may omit impacts.");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_evidence_contains_cited_and_derived_records_only() {
        let record = |id, origin, derived_from| -> SchemaEvidenceRecord {
            serde_json::from_value(json!({
                "id":id, "subject":id, "origin":origin, "derived_from":derived_from,
                "coverage":"exhaustive", "coverage_basis":"enumerated", "order":"proven",
                "enforcement":null,
            }))
            .unwrap()
        };
        let ledger = [
            record("sql", "sql_output", vec!["seed"]),
            record("seed", "seed_file", vec![]),
            record("unrelated", "yaml_declaration", vec![]),
        ];
        let mut evidence = Evidence::default();
        evidence.add([&ledger[0].id]);
        let summary = evidence.project(&ledger, false);
        let origins: Vec<_> = summary
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["origin"].as_str().unwrap())
            .collect();
        assert_eq!(origins, ["sql_output", "seed_file"]);
    }
}
