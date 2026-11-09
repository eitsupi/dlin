//! Partial value lineage: known sources are kept next to the unresolved part,
//! through single-model analysis, cross-model tracing, impact exploration and
//! the cache.

use super::*;

const DIALECT: DlinDialect = DlinDialect::Generic;

/// A model definition: name, dependency names, compiled SQL and YAML columns.
type ModelSpec<'a> = (&'a str, &'a [&'a str], Option<&'a str>, &'a [&'a str]);

fn model_id(name: &str) -> String {
    format!("model.proj.{name}")
}

fn manifest(models: &[ModelSpec<'_>]) -> Manifest {
    let nodes: serde_json::Map<String, serde_json::Value> = models
        .iter()
        .map(|(name, deps, sql, yaml)| {
            let columns: serde_json::Map<_, _> = yaml
                .iter()
                .map(|column| ((*column).to_string(), serde_json::json!({"name": column})))
                .collect();
            let deps: Vec<_> = deps.iter().map(|dep| model_id(dep)).collect();
            (
                model_id(name),
                serde_json::json!({
                    "unique_id": model_id(name),
                    "name": name,
                    "resource_type": "model",
                    "depends_on": {"nodes": deps},
                    "compiled_code": sql,
                    "columns": columns,
                }),
            )
        })
        .collect();
    serde_json::from_value(serde_json::json!({"nodes": nodes, "sources": {}})).unwrap()
}

/// `source_model` feeds `partial_model`, whose `total` also depends on the
/// unknown columns of `raw.fees`. `downstream_model` reads `total`.
fn partial_chain_manifest() -> Manifest {
    manifest(&[
        (
            "source_model",
            &[],
            Some("SELECT x, y FROM raw.source_table"),
            &["x", "y"],
        ),
        (
            "partial_model",
            &["source_model"],
            Some(
                "WITH unknown AS (SELECT * FROM raw.fees) \
                 SELECT s.x + unknown.fee AS total \
                 FROM source_model s CROSS JOIN unknown",
            ),
            &["total"],
        ),
        (
            "downstream_model",
            &["partial_model"],
            Some("SELECT total FROM partial_model"),
            &["total"],
        ),
    ])
}

/// The output `column` at SQL position `slot` of `model`.
fn output_ref(model: &str, slot: usize, column: &str) -> ModelOutputRef {
    ModelOutputRef {
        unique_id: model_id(model),
        model: model.to_string(),
        slot: OutputSlotId::new(slot),
        column: column.to_string(),
    }
}

fn entry<'a>(result: &'a ModelColumnLineage, column: &str) -> &'a OutputSlot<ColumnSource> {
    result.slot_named(column)
}

fn known_pairs(entry: &OutputSlot<ColumnSource>) -> Vec<(String, String)> {
    entry
        .lineage
        .known()
        .iter()
        .map(|source| (source.table.clone(), source.column.to_string()))
        .collect()
}

fn impact(
    manifest: &Manifest,
    model: &str,
    column: &str,
    scope: ImpactScope,
) -> ColumnImpactReport {
    compute_column_impact_with_scope(
        manifest,
        model,
        column,
        scope,
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    )
}

fn confirmed_targets(report: &ColumnImpactReport) -> Vec<(&str, &str)> {
    report
        .confirmed
        .iter()
        .map(|impact| (impact.target.model.as_str(), impact.target.column.as_str()))
        .collect()
}

fn value_exploration(report: &ColumnImpactReport) -> &ExplorationStatus {
    &report
        .exploration
        .iter()
        .find(|exploration| exploration.kind == DependencyKind::Value)
        .expect("value exploration is always requested")
        .status
}

fn value_gaps(report: &ColumnImpactReport) -> &[TraceGap] {
    match value_exploration(report) {
        ExplorationStatus::Complete => &[],
        ExplorationStatus::Incomplete { gaps } => gaps,
    }
}

fn behind_star_of(reason: &UnresolvedReason, relation: &str) -> bool {
    matches!(
        reason,
        UnresolvedReason::BehindStar { relation: found }
            | UnresolvedReason::NamedBehindStar { relation: found, .. }
        if found.to_string() == relation
    )
}

#[test]
fn single_model_partial_keeps_known_sources_and_unresolved_reason() {
    let manifest = partial_chain_manifest();
    let result = compute_column_lineage(
        &manifest,
        "partial_model",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );

    let total = entry(&result, "total");
    assert!(total.lineage.is_partial(), "{total:?}");
    assert_eq!(
        known_pairs(total),
        [("source_model".to_string(), "x".to_string())]
    );
    let [contribution] = total.lineage.unresolved() else {
        panic!("expected one unresolved contribution: {total:?}");
    };
    assert!(
        behind_star_of(&contribution.reason, "raw.fees"),
        "{contribution:?}"
    );
    assert_eq!(contribution.origin, output_ref("partial_model", 0, "total"));
    assert!(contribution.model_path.is_empty());

    // The existing column-scoped diagnostic is still reported.
    assert!(
        result.errors.iter().any(|error| {
            error.kind == ColumnLineageErrorKind::ColumnIndeterminate
                && error.column_name() == Some("total")
        }),
        "{:?}",
        result.errors
    );
    // Only complete columns are counted as traced.
    assert_eq!(result.value_complete_columns, 0);
    assert_eq!(result.listed_columns, 1);
    assert_eq!(result.slots().len(), 1);
}

#[test]
fn partial_with_no_known_source_is_still_listed() {
    let manifest = manifest(&[(
        "only_unknown",
        &[],
        Some("WITH u AS (SELECT * FROM raw.fees) SELECT u.fee AS fee FROM u"),
        &["fee"],
    )]);
    let result = compute_column_lineage(
        &manifest,
        "only_unknown",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );
    let fee = entry(&result, "fee");
    assert!(fee.lineage.known().is_empty());
    assert!(fee.lineage.is_partial());
    assert_eq!(result.value_complete_columns, 0);
}

#[test]
fn cross_model_partial_keeps_terminal_and_unresolved_origin() {
    let manifest = partial_chain_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(result.value_complete_columns, 0);
    assert_eq!(result.listed_columns, 1);
    let total = entry(&result, "total");
    assert!(total.lineage.is_partial(), "{total:?}");
    assert_eq!(
        known_pairs(total),
        [("raw.source_table".to_string(), "x".to_string())]
    );
    let source = &total.lineage.known()[0];
    let hops: Vec<_> = source
        .model_path
        .iter()
        .map(|hop| (hop.output.model.as_str(), hop.output.column.as_str()))
        .collect();
    assert_eq!(hops, [("partial_model", "total"), ("source_model", "x")]);

    let [contribution] = total.lineage.unresolved() else {
        panic!("expected one unresolved contribution: {total:?}");
    };
    assert!(behind_star_of(&contribution.reason, "raw.fees"));
    assert_eq!(contribution.origin, output_ref("partial_model", 0, "total"));
    let path: Vec<_> = contribution
        .model_path
        .iter()
        .map(|hop| (hop.output.model.as_str(), hop.output.column.as_str()))
        .collect();
    assert_eq!(path, [("partial_model", "total")]);
}

#[test]
fn unresolved_contributions_are_sorted_and_deduplicated() {
    // Two outputs of the unknown relation feed one value; each reaches the
    // root through the same upstream output, so they collapse by identity.
    let manifest = manifest(&[
        (
            "partial_model",
            &[],
            Some(
                "WITH unknown AS (SELECT * FROM raw.fees) \
                 SELECT unknown.a + unknown.b AS total FROM unknown",
            ),
            &["total"],
        ),
        (
            "downstream_model",
            &["partial_model"],
            Some("SELECT total, total AS again FROM partial_model"),
            &["total", "again"],
        ),
    ]);
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );
    for column in ["total", "again"] {
        let unresolved = entry(&result, column).lineage.unresolved();
        let mut expected = unresolved.to_vec();
        expected.sort();
        expected.dedup();
        assert_eq!(unresolved, expected, "column {column}");
        assert!(!unresolved.is_empty());
        assert!(
            unresolved
                .iter()
                .all(|c| c.origin == output_ref("partial_model", 0, "total"))
        );
    }
}

#[test]
fn impact_confirms_through_partial_entries_and_reports_the_gap() {
    let manifest = partial_chain_manifest();
    let report = impact(&manifest, "source_model", "x", ImpactScope::ValueOnly);

    assert_eq!(
        confirmed_targets(&report),
        [("downstream_model", "total"), ("partial_model", "total")]
    );
    let downstream = report
        .confirmed
        .iter()
        .find(|impact| impact.target.model == "downstream_model")
        .unwrap();
    assert_eq!(downstream.kind, DependencyKind::Value);
    let hops: Vec<_> = downstream
        .path
        .iter()
        .map(|edge| (edge.to.model.as_str(), edge.to.column.as_str()))
        .collect();
    assert_eq!(
        hops,
        [("partial_model", "total"), ("downstream_model", "total")]
    );
    assert_eq!(
        downstream.path[0].from,
        DependencyRef::Column(output_ref("source_model", 0, "x"))
    );

    // The unresolved part of partial_model.total is not proven unrelated.
    let gaps = value_gaps(&report);
    let [TraceGap::UnresolvedLineage { at, reasons }] = gaps else {
        panic!("expected one unresolved-lineage gap: {gaps:?}");
    };
    assert_eq!(*at, output_ref("partial_model", 0, "total"));
    assert!(
        reasons
            .iter()
            .all(|reason| behind_star_of(reason, "raw.fees"))
    );
    assert!(!reasons.is_empty());
    assert!(!report.proves_no_impact());
    // The column-scoped diagnostic of the confirmed column is kept.
    assert!(
        report.errors.iter().any(|error| {
            error.kind == ColumnLineageErrorKind::ColumnIndeterminate
                && error.column_name() == Some("total")
        }),
        "{:?}",
        report.errors
    );
}

#[test]
fn partial_without_known_edge_is_a_gap_and_not_a_confirmation() {
    // `only_fee` is partial and has no known edge to source_model.y.
    let manifest = manifest(&[
        (
            "source_model",
            &[],
            Some("SELECT x, y FROM raw.source_table"),
            &["x", "y"],
        ),
        (
            "partial_model",
            &["source_model"],
            Some(
                "WITH u AS (SELECT * FROM raw.fees) \
                 SELECT u.fee AS only_fee, s.x AS x \
                 FROM source_model s CROSS JOIN u",
            ),
            &["only_fee", "x"],
        ),
    ]);
    let report = impact(&manifest, "source_model", "y", ImpactScope::ValueOnly);

    assert!(report.confirmed.is_empty(), "{:?}", report.confirmed);
    let gaps = value_gaps(&report);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(matches!(
        &gaps[0],
        TraceGap::UnresolvedLineage { at, .. } if *at == output_ref("partial_model", 0, "only_fee")
    ));
    assert!(!report.proves_no_impact());
}

#[test]
fn complete_chain_is_complete_and_row_is_not_collected() {
    let manifest = manifest(&[
        (
            "source_model",
            &[],
            Some("SELECT x FROM raw.source_table"),
            &["x"],
        ),
        (
            "mid",
            &["source_model"],
            Some("SELECT x FROM source_model"),
            &["x"],
        ),
        ("leaf", &["mid"], Some("SELECT x AS z FROM mid"), &["z"]),
    ]);

    let value = impact(&manifest, "source_model", "x", ImpactScope::ValueOnly);
    assert_eq!(confirmed_targets(&value), [("leaf", "z"), ("mid", "x")]);
    assert_eq!(
        value.exploration,
        [KindExploration {
            kind: DependencyKind::Value,
            status: ExplorationStatus::Complete,
        }]
    );
    assert!(value.errors.is_empty());

    let both = impact(&manifest, "source_model", "x", ImpactScope::ValueAndRow);
    assert_eq!(both.scope, ImpactScope::ValueAndRow);
    assert_eq!(confirmed_targets(&both), confirmed_targets(&value));
    assert_eq!(
        both.exploration,
        [
            KindExploration {
                kind: DependencyKind::Value,
                status: ExplorationStatus::Complete,
            },
            KindExploration {
                kind: DependencyKind::Row,
                status: ExplorationStatus::Incomplete {
                    gaps: vec![TraceGap::NotCollected {
                        kind: DependencyKind::Row
                    }],
                },
            },
        ]
    );
    // No row impact is ever invented.
    assert!(
        both.confirmed
            .iter()
            .all(|impact| impact.kind == DependencyKind::Value)
    );
    assert!(!both.proves_no_impact());
}

#[test]
fn no_dependents_proves_no_impact_only_when_every_requested_kind_is_complete() {
    let manifest = manifest(&[("leaf", &[], Some("SELECT 1 AS x"), &["x"])]);
    let value = impact(&manifest, "leaf", "x", ImpactScope::ValueOnly);
    assert!(value.confirmed.is_empty());
    assert!(value.proves_no_impact());

    let both = impact(&manifest, "leaf", "x", ImpactScope::ValueAndRow);
    assert!(!both.proves_no_impact());
}

#[test]
fn dependent_model_failure_is_an_analysis_gap() {
    let manifest = manifest(&[
        (
            "source_model",
            &[],
            Some("SELECT x FROM raw.source_table"),
            &["x"],
        ),
        (
            "broken",
            &["source_model"],
            Some("SELECT FROM WHERE ("),
            &["x"],
        ),
        ("no_sql", &["source_model"], None, &["x"]),
    ]);
    let report = impact(&manifest, "source_model", "x", ImpactScope::ValueOnly);

    assert!(report.confirmed.is_empty());
    let gaps = value_gaps(&report);
    let failed: Vec<_> = gaps
        .iter()
        .filter_map(|gap| match gap {
            TraceGap::AnalysisFailed { model, error, .. } => Some((model.as_str(), *error)),
            _ => None,
        })
        .collect();
    assert_eq!(
        failed,
        [
            ("broken", ColumnLineageErrorKind::ParseFailure),
            ("no_sql", ColumnLineageErrorKind::NoCompiledCode),
        ]
    );
    assert!(!report.proves_no_impact());
}

#[test]
fn unlisted_column_with_a_diagnostic_is_an_unresolved_column_gap() {
    // `ghost` is declared in YAML but the SQL does not produce it.
    let manifest = manifest(&[
        (
            "source_model",
            &[],
            Some("SELECT x FROM raw.source_table"),
            &["x"],
        ),
        (
            "dependent",
            &["source_model"],
            Some("SELECT x FROM source_model"),
            &["x", "ghost"],
        ),
    ]);
    let report = impact(&manifest, "source_model", "x", ImpactScope::ValueOnly);

    // A column that only the YAML declares is not an output of the dependent
    // model, so it is neither a target nor a gap of the search.
    assert_eq!(confirmed_targets(&report), [("dependent", "x")]);
    assert_eq!(value_exploration(&report), &ExplorationStatus::Complete);
    assert!(report.errors.is_empty(), "{:?}", report.errors);
}

#[test]
fn model_not_found_is_an_incomplete_exploration() {
    let manifest = manifest(&[("only", &[], Some("SELECT 1 AS x"), &["x"])]);
    let report = impact(&manifest, "missing", "x", ImpactScope::ValueOnly);
    assert!(!report.proves_no_impact());
    assert!(matches!(
        value_gaps(&report),
        [TraceGap::AnalysisFailed {
            error: ColumnLineageErrorKind::ModelNotFound,
            ..
        }]
    ));
}

#[test]
fn impact_gaps_and_confirmations_are_deterministic() {
    let manifest = partial_chain_manifest();
    let first = impact(&manifest, "source_model", "x", ImpactScope::ValueAndRow);
    for _ in 0..5 {
        assert_eq!(
            first,
            impact(&manifest, "source_model", "x", ImpactScope::ValueAndRow)
        );
    }
    let mut sorted = first.confirmed.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(first.confirmed, sorted);
}

#[test]
fn partial_value_lineage_does_not_change_output_listing() {
    // Whether an output is partial changes its lineage only: not the listed
    // names, their count, nor the columns a `SELECT *` of the model provides.
    let manifest = manifest(&[
        (
            "partial_model",
            &[],
            Some("WITH u AS (SELECT * FROM raw.fees) SELECT u.fee AS a, 1 AS b FROM u"),
            &[],
        ),
        (
            "star_reader",
            &["partial_model"],
            Some("SELECT * FROM partial_model"),
            &[],
        ),
    ]);
    let partial = compute_column_lineage(
        &manifest,
        "partial_model",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );
    assert_eq!(partial.names(), ["a", "b"]);
    assert_eq!(partial.listed_columns, 2);
    assert!(entry(&partial, "a").lineage.is_partial());
    assert!(entry(&partial, "b").lineage.is_complete());
    assert_eq!(partial.value_complete_columns, 1);

    let reader = compute_column_lineage(
        &manifest,
        "star_reader",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );
    assert_eq!(reader.names(), ["a", "b"]);
    assert_eq!(reader.listed_columns, 2);
}

#[test]
fn partial_entry_round_trips_through_the_cache() {
    let manifest = partial_chain_manifest();
    let tmp = tempfile::tempdir().unwrap();

    let mut cache = ColumnLineageCache::load(tmp.path(), None);
    let computed =
        compute_cross_model_column_lineage(&manifest, "downstream_model", DIALECT, &mut cache);
    cache.save();
    let total = entry(&computed, "total");
    assert!(total.lineage.is_partial());

    // A fresh process reads the typed entries back from disk.
    let mut reloaded = ColumnLineageCache::load(tmp.path(), None);
    let from_cache =
        compute_cross_model_column_lineage(&manifest, "downstream_model", DIALECT, &mut reloaded);
    assert_eq!(
        serde_json::to_value(&computed).unwrap(),
        serde_json::to_value(&from_cache).unwrap()
    );
    let cached_total = entry(&from_cache, "total");
    assert_eq!(cached_total.lineage, total.lineage);
    let [contribution] = cached_total.lineage.unresolved() else {
        panic!("{cached_total:?}");
    };
    assert!(behind_star_of(&contribution.reason, "raw.fees"));
    assert_eq!(contribution.origin, output_ref("partial_model", 0, "total"));
    assert_eq!(contribution.model_path.len(), 1);

    // The stored single-model entry is the typed internal value, not a copy.
    let digest = SemanticDigestCache::default()
        .digest_for_node(&manifest, &manifest.nodes[&model_id("partial_model")]);
    let stored = reloaded
        .get(&model_id("partial_model"), DIALECT, digest.value)
        .expect("partial_model is cached");
    let stored_total = stored
        .output
        .as_ref()
        .unwrap()
        .columns
        .iter()
        .find(|entry| entry.name.to_string() == "total")
        .unwrap();
    assert!(stored_total.lineage.is_partial());
    assert_eq!(stored_total.lineage.known().len(), 1);
    assert_eq!(stored_total.lineage.unresolved().len(), 1);
}

#[test]
fn every_unresolved_reason_round_trips_with_its_tag() {
    let relation = RelationRef::parse("raw.fees").unwrap();
    let reasons = [
        (
            "alignment_unknown",
            UnresolvedReason::AlignmentUnknown {
                operation: SetOperationKind {
                    operator: SetOperator::Union,
                    quantifier: SetQuantifier::All,
                    correspondence: SetCorrespondence::Positional,
                },
                possible_sources: vec![UnresolvedSource {
                    relation: relation.clone(),
                    column: Ident::unquoted("a"),
                }],
                unknown_operands: vec![SetOperand::Right],
            },
        ),
        (
            "schema_dependent",
            UnresolvedReason::SchemaDependent {
                what: SchemaDependencyKind::NaturalJoinColumns,
                relations: vec![relation.clone()],
            },
        ),
        (
            "behind_star",
            UnresolvedReason::BehindStar {
                relation: relation.clone(),
            },
        ),
        (
            "named_behind_star",
            UnresolvedReason::NamedBehindStar {
                relation: relation.clone(),
                column: Ident::unquoted("fee"),
            },
        ),
        (
            "ambiguous",
            UnresolvedReason::Ambiguous {
                column: Ident::unquoted("id"),
                candidates: vec![
                    UnresolvedCandidate::Relation {
                        relation: relation.clone(),
                    },
                    UnresolvedCandidate::Cte {
                        name: "c".to_string(),
                    },
                    UnresolvedCandidate::Derived { name: None },
                ],
            },
        ),
        (
            "unbound",
            UnresolvedReason::Unbound {
                column: Ident::unquoted("id"),
            },
        ),
        ("recursive", UnresolvedReason::Recursive),
        (
            "row_value",
            UnresolvedReason::RowValue {
                alias: "t".to_string(),
            },
        ),
        (
            "unsupported",
            UnresolvedReason::Unsupported {
                what: "pivot".to_string(),
            },
        ),
    ];
    for (tag, reason) in reasons {
        let json = serde_json::to_value(&reason).unwrap();
        assert_eq!(json["kind"], tag);
        let decoded: UnresolvedReason = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, reason);
    }
}

#[test]
fn public_json_shapes_complete_and_partial_lineage() {
    let manifest = partial_chain_manifest();
    let partial = compute_column_lineage(
        &manifest,
        "partial_model",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_value(&partial).unwrap();
    let slot = &json["output"]["columns"][0];
    let lineage = &slot["lineage"];
    assert_eq!(lineage["status"], "partial");
    assert_eq!(lineage["known"][0]["column"]["value"], "x");
    assert_eq!(slot["id"], 0);
    assert_eq!(slot["name"]["kind"], "explicit");
    assert_eq!(
        lineage["unresolved"][0]["reason"]["kind"],
        "named_behind_star"
    );
    assert_eq!(lineage["unresolved"][0]["origin"]["model"], "partial_model");
    assert!(slot.get("sources").is_none());
    assert_eq!(json["listed_columns"], 1);
    assert_eq!(json["value_complete_columns"], 0);
    assert_eq!(json["value_partial_columns"], 1);

    let complete = compute_column_lineage(
        &manifest,
        "source_model",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );
    let json = serde_json::to_value(&complete).unwrap();
    let lineage = &json["output"]["columns"][0]["lineage"];
    assert_eq!(lineage["status"], "complete");
    assert!(lineage.get("unresolved").is_none());

    let report = impact(&manifest, "source_model", "x", ImpactScope::ValueOnly);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["scope"], "value_only");
    assert_eq!(json["confirmed"][0]["kind"], "value");
    assert_eq!(json["exploration"][0]["kind"], "value");
    assert_eq!(json["exploration"][0]["status"], "incomplete");
    assert_eq!(
        json["exploration"][0]["gaps"][0]["type"],
        "unresolved_lineage"
    );
    assert!(json.get("impacted_columns").is_none());
}
