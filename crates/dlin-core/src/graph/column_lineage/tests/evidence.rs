//! Schema evidence in model results: each conclusion cites the evidence it
//! relies on, an SQL output schema keeps the evidence it was derived from, and
//! a result describes what it cites.

use super::*;

const SOURCE: &str = "source.proj.raw.raw_orders";
const YAML_SOURCE: &str = "yaml:source.proj.raw.raw_orders";
const SQL_M: &str = "sql:model.proj.m";

fn id(value: &str) -> EvidenceId {
    EvidenceId(value.to_string())
}

fn ids(values: &[&str]) -> Vec<EvidenceId> {
    values.iter().map(|value| id(value)).collect()
}

/// A source `raw_orders` whose YAML lists `source_columns`, a model `m` with
/// `m_sql` and `m_columns` in YAML, and a model `d` = `SELECT * FROM m`.
fn manifest(
    source_columns: &[&str],
    m_sql: Option<&str>,
    m_columns: &[&str],
    m_contract: Option<bool>,
) -> Manifest {
    let columns = |names: &[&str]| -> serde_json::Value {
        names
            .iter()
            .map(|name| ((*name).to_string(), serde_json::json!({"name": name})))
            .collect::<serde_json::Map<_, _>>()
            .into()
    };
    serde_json::from_value(serde_json::json!({
        "nodes": {
            "model.proj.m": {
                "unique_id": "model.proj.m",
                "name": "m",
                "resource_type": "model",
                "depends_on": {"nodes": [SOURCE]},
                "compiled_code": m_sql,
                "columns": columns(m_columns),
                "config": {
                    "contract": m_contract.map(|enforced| serde_json::json!({"enforced": enforced}))
                }
            },
            "model.proj.d": {
                "unique_id": "model.proj.d",
                "name": "d",
                "resource_type": "model",
                "depends_on": {"nodes": ["model.proj.m"]},
                "compiled_code": "SELECT * FROM m"
            }
        },
        "sources": {
            SOURCE: {
                "unique_id": SOURCE,
                "name": "raw_orders",
                "source_name": "raw",
                "resource_type": "source",
                "columns": columns(source_columns)
            }
        }
    }))
    .unwrap()
}

fn d2() -> Manifest {
    manifest(&["id"], Some("SELECT * FROM raw_orders"), &[], None)
}

fn lineage(manifest: &Manifest, model: &str) -> ModelColumnLineage {
    compute_column_lineage(
        manifest,
        model,
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    )
}

fn cross_model(manifest: &Manifest, model: &str) -> ModelColumnLineage {
    compute_cross_model_column_lineage(
        manifest,
        model,
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    )
}

fn slot<'a>(result: &'a ModelColumnLineage, column: &str) -> &'a OutputSlot<ColumnSource> {
    result
        .slots()
        .iter()
        .find(|slot| slot.column() == column)
        .unwrap_or_else(|| panic!("no column {column}: {result:?}"))
}

fn record<'a>(result: &'a ModelColumnLineage, evidence: &str) -> &'a SchemaEvidenceRecord {
    result
        .evidence
        .iter()
        .find(|record| record.id.as_str() == evidence)
        .unwrap_or_else(|| panic!("{evidence} is not described: {:?}", result.evidence))
}

#[test]
fn d2_a_star_over_a_yaml_only_source_cites_the_assumption() {
    let result = lineage(&d2(), "m");
    let id_slot = slot(&result, "id");
    let source = &id_slot.lineage.known()[0];
    assert_eq!(source.evidence, ids(&[YAML_SOURCE]));
    assert_eq!(id_slot.evidence.structure, ids(&[YAML_SOURCE]));
    assert_eq!(id_slot.evidence.lineage, ids(&[YAML_SOURCE]));
    // The output lists `id` and nothing more because the YAML list is
    // assumed to be exhaustive.
    assert_eq!(
        result.output.as_ref().unwrap().evidence,
        ids(&[YAML_SOURCE])
    );

    let yaml = record(&result, YAML_SOURCE);
    assert_eq!(yaml.origin, SchemaOrigin::YamlDeclaration);
    assert_eq!(yaml.coverage, SchemaCoverage::Exhaustive);
    assert_eq!(yaml.coverage_basis, CoverageBasis::AssumedByYamlPolicy);
    assert_eq!(yaml.order, SchemaOrder::Unknown);
    assert!(yaml.is_assumption());
    assert_eq!(result.evidence.len(), 1);
}

#[test]
fn a_downstream_model_sees_the_assumption_behind_an_sql_output() {
    let result = lineage(&d2(), "d");
    let id_slot = slot(&result, "id");
    // `d` reads `m`'s SQL output, whose columns rest on the YAML assumption.
    assert_eq!(
        id_slot.lineage.known()[0].evidence,
        ids(&[SQL_M, YAML_SOURCE])
    );
    let sql = record(&result, SQL_M);
    assert_eq!(sql.origin, SchemaOrigin::SqlOutput);
    assert_eq!(sql.coverage_basis, CoverageBasis::Enumerated);
    assert_eq!(sql.derived_from, ids(&[YAML_SOURCE]));
    assert!(!sql.is_assumption());
    assert!(record(&result, YAML_SOURCE).is_assumption());
}

#[test]
fn a_cross_model_trace_keeps_the_evidence_of_every_hop() {
    let result = cross_model(&d2(), "d");
    let id_slot = slot(&result, "id");
    let source = &id_slot.lineage.known()[0];
    assert_eq!(source.table, "raw_orders");
    assert_eq!(source.evidence, ids(&[SQL_M, YAML_SOURCE]));
    assert_eq!(id_slot.evidence.lineage, ids(&[SQL_M, YAML_SOURCE]));
    let described: Vec<&str> = result
        .evidence
        .iter()
        .map(|record| record.id.as_str())
        .collect();
    assert_eq!(described, [SQL_M, YAML_SOURCE]);
}

#[test]
fn an_enforced_contract_has_its_own_basis() {
    // Without compiled SQL, `m`'s declared columns stand in for its output.
    let manifest = manifest(&["id"], None, &["id"], Some(true));
    let result = lineage(&manifest, "d");
    let yaml = record(&result, "yaml:model.proj.m");
    assert_eq!(yaml.coverage_basis, CoverageBasis::DeclaredEnforcedContract);
    assert_eq!(
        yaml.enforcement,
        Some(ContractEnforcementEvidence {
            declared: true,
            applicability: EnforcementApplicability::Unknown,
        })
    );
    assert_eq!(
        slot(&result, "id").lineage.known()[0].evidence,
        ids(&["yaml:model.proj.m"])
    );
}

#[test]
fn an_sql_output_is_not_derived_from_what_only_its_values_relied_on() {
    let mut manifest = manifest(
        &["x"],
        Some("SELECT x AS k FROM raw_orders CROSS JOIN other"),
        &[],
        None,
    );
    let other = "source.proj.raw.other";
    let source = serde_json::from_value(serde_json::json!({
        "unique_id": other,
        "name": "other",
        "source_name": "raw",
        "resource_type": "source",
        "columns": {"y": {"name": "y"}}
    }))
    .unwrap();
    manifest.sources.insert(other.to_string(), source);
    manifest
        .nodes
        .get_mut("model.proj.m")
        .unwrap()
        .depends_on
        .nodes
        .push(other.to_string());

    // `x` is bound to raw_orders because `other` has no `x`.
    let m = lineage(&manifest, "m");
    assert_eq!(
        slot(&m, "k").evidence.lineage,
        ids(&["yaml:source.proj.raw.other", YAML_SOURCE])
    );
    // But `m`'s output is one column named `k` whatever the sources list.
    let d = lineage(&manifest, "d");
    assert!(record(&d, SQL_M).derived_from.is_empty());
    assert_eq!(slot(&d, "k").lineage.known()[0].evidence, ids(&[SQL_M]));
}

#[test]
fn selecting_columns_keeps_only_the_evidence_they_cite() {
    let mut result = cross_model(
        &manifest(
            &["id"],
            Some("SELECT id, 1 AS one FROM raw_orders"),
            &[],
            None,
        ),
        "d",
    );
    result.select_columns(&["one"], DlinDialect::Generic);
    // `one` is a constant, so nothing about raw_orders' columns decides it,
    // while d's star over m still relies on m's SQL output.
    assert_eq!(slot(&result, "one").evidence.structure, ids(&[SQL_M]));
    let described: Vec<&str> = result
        .evidence
        .iter()
        .map(|record| record.id.as_str())
        .collect();
    assert_eq!(described, [SQL_M]);
}

#[test]
fn i4_cached_results_keep_their_evidence_and_are_not_reused_after_it_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let compute = |manifest: &Manifest| {
        let mut cache = ColumnLineageCache::load(tmp.path(), None);
        let result = compute_column_lineage(manifest, "d", DlinDialect::Generic, &mut cache);
        cache.save();
        result
    };

    // A model without SQL whose declared columns stand in for its output.
    let assumed = manifest(&["id"], None, &["id"], None);
    let stored = compute(&assumed);
    let reloaded = compute(&assumed);
    assert_eq!(stored, reloaded);
    assert_eq!(
        record(&reloaded, "yaml:model.proj.m").coverage_basis,
        CoverageBasis::AssumedByYamlPolicy
    );

    // Only the contract changes: the same evidence id and columns, another
    // basis. The cached result must not be reused.
    let enforced = manifest(&["id"], None, &["id"], Some(true));
    assert_eq!(
        record(&compute(&enforced), "yaml:model.proj.m").coverage_basis,
        CoverageBasis::DeclaredEnforcedContract
    );

    // The upstream SQL changes the derivation of `m`'s output.
    let with_sql = manifest(&["id"], Some("SELECT * FROM raw_orders"), &["id"], None);
    let derived = compute(&with_sql);
    assert_eq!(record(&derived, SQL_M).derived_from, ids(&[YAML_SOURCE]));
    let constant = manifest(&["id"], Some("SELECT 1 AS id"), &["id"], None);
    assert!(record(&compute(&constant), SQL_M).derived_from.is_empty());
}

#[test]
fn an_unexpanded_star_adds_only_its_structure_to_the_shape() {
    // A star over a struct field reads the column `rec` of `raw_orders`,
    // which relies on its schema, but no schema says which fields `rec` has.
    let manifest = manifest(
        &["rec"],
        Some("SELECT raw_orders.rec.* FROM raw_orders"),
        &[],
        None,
    );
    let result = compute_column_lineage(
        &manifest,
        "m",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );
    let output = result.output.as_ref().expect("m is analysed");
    assert!(
        matches!(output.shape, OutputShape::Open { .. }),
        "{output:?}"
    );
    assert!(output.evidence.is_empty(), "{:?}", output.evidence);
}

/// `manifest` with a second YAML source `other` listing `columns`, which `m`
/// also depends on.
fn with_other_source(mut manifest: Manifest, columns: &[&str]) -> Manifest {
    let other = "source.proj.raw.other";
    let columns: serde_json::Map<_, _> = columns
        .iter()
        .map(|name| ((*name).to_string(), serde_json::json!({"name": name})))
        .collect();
    let source = serde_json::from_value(serde_json::json!({
        "unique_id": other,
        "name": "other",
        "source_name": "raw",
        "resource_type": "source",
        "columns": columns
    }))
    .unwrap();
    manifest.sources.insert(other.to_string(), source);
    manifest
        .nodes
        .get_mut("model.proj.m")
        .unwrap()
        .depends_on
        .nodes
        .push(other.to_string());
    manifest
}

#[test]
fn a_source_reached_along_several_references_keeps_the_evidence_of_each() {
    let manifest = with_other_source(
        manifest(
            &["id"],
            Some("SELECT o.id + id AS total FROM raw_orders o CROSS JOIN other"),
            &[],
            None,
        ),
        &["y"],
    );
    // The qualified reference relies on raw_orders alone; the unqualified one
    // also on `other` having no `id`. The merged source keeps both.
    let result = cross_model(
        &{
            let mut manifest = manifest;
            manifest
                .nodes
                .get_mut("model.proj.d")
                .unwrap()
                .compiled_code = Some("SELECT total FROM m".to_string());
            manifest
        },
        "d",
    );
    let total = slot(&result, "total");
    assert_eq!(total.lineage.known().len(), 1);
    assert_eq!(
        total.lineage.known()[0].evidence,
        ids(&[
            "sql:model.proj.m",
            "yaml:source.proj.raw.other",
            YAML_SOURCE
        ])
    );
}

#[test]
fn an_unresolved_part_found_upstream_keeps_its_evidence_and_the_path_to_it() {
    // `elsewhere` has no schema, so `x` may come from it as well.
    let mut manifest = manifest(
        &["x"],
        Some("SELECT x FROM raw_orders CROSS JOIN elsewhere"),
        &[],
        None,
    );
    manifest
        .nodes
        .get_mut("model.proj.d")
        .unwrap()
        .compiled_code = Some("SELECT x FROM m".to_string());
    let m = lineage(&manifest, "m");
    let upstream = &slot(&m, "x").lineage.unresolved()[0];
    assert!(
        matches!(upstream.reason, UnresolvedReason::Unverified { .. }),
        "{upstream:?}"
    );
    assert_eq!(upstream.evidence, ids(&[YAML_SOURCE]));

    let result = cross_model(&manifest, "d");
    let x = slot(&result, "x");
    let traced = x
        .lineage
        .unresolved()
        .iter()
        .find(|contribution| contribution.reason == upstream.reason)
        .unwrap_or_else(|| panic!("the upstream ambiguity is kept: {x:?}"));
    assert_eq!(traced.evidence, ids(&[SQL_M, YAML_SOURCE]));
    assert!(x.evidence.lineage.contains(&id(YAML_SOURCE)));
}

// ── Impact ──────────────────────────────────────────────────────────────────

/// Models `(name, dependencies, sql)` over YAML sources `(name, columns)` of
/// the source `raw`. A dependency names a model or a source.
fn project(models: &[(&str, &[&str], &str)], sources: &[(&str, &[&str])]) -> Manifest {
    let source_id = |name: &str| format!("source.proj.raw.{name}");
    let is_source = |name: &str| sources.iter().any(|(source, _)| *source == name);
    let nodes: serde_json::Map<_, _> = models
        .iter()
        .map(|(name, dependencies, sql)| {
            let dependencies: Vec<String> = dependencies
                .iter()
                .map(|dependency| {
                    if is_source(dependency) {
                        source_id(dependency)
                    } else {
                        format!("model.proj.{dependency}")
                    }
                })
                .collect();
            (
                format!("model.proj.{name}"),
                serde_json::json!({
                    "unique_id": format!("model.proj.{name}"),
                    "name": name,
                    "resource_type": "model",
                    "depends_on": {"nodes": dependencies},
                    "compiled_code": sql,
                }),
            )
        })
        .collect();
    let sources: serde_json::Map<_, _> = sources
        .iter()
        .map(|(name, columns)| {
            let columns: serde_json::Map<_, _> = columns
                .iter()
                .map(|column| ((*column).to_string(), serde_json::json!({"name": column})))
                .collect();
            (
                source_id(name),
                serde_json::json!({
                    "unique_id": source_id(name),
                    "name": name,
                    "source_name": "raw",
                    "resource_type": "source",
                    "columns": columns,
                }),
            )
        })
        .collect();
    serde_json::from_value(serde_json::json!({"nodes": nodes, "sources": sources})).unwrap()
}

fn impact(manifest: &Manifest, model: &str, column: &str) -> ColumnImpactReport {
    compute_column_impact(
        manifest,
        model,
        column,
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    )
}

fn value_exploration(report: &ColumnImpactReport) -> &KindExploration {
    report
        .exploration
        .iter()
        .find(|exploration| exploration.kind == DependencyKind::Value)
        .expect("value is always explored")
}

fn described(report: &ColumnImpactReport) -> Vec<&str> {
    report
        .evidence
        .iter()
        .map(|record| record.id.as_str())
        .collect()
}

#[test]
fn d2_impact_and_no_impact_cite_the_assumption() {
    // `m` lists raw_orders' YAML columns through a star, and `d` reads `id`.
    let manifest = project(
        &[
            ("m", &["raw_orders"], "SELECT * FROM raw_orders"),
            ("d", &["m"], "SELECT id FROM m"),
        ],
        &[("raw_orders", &["id", "amount"])],
    );

    let report = impact(&manifest, "m", "id");
    assert_eq!(report.confirmed.len(), 1, "{report:?}");
    // `d.id` reads `m.id` because `m`'s output, derived from the YAML list,
    // has one column named `id`.
    assert_eq!(report.confirmed[0].evidence, ids(&[SQL_M, YAML_SOURCE]));
    assert_eq!(
        value_exploration(&report).status,
        ExplorationStatus::Complete
    );
    assert_eq!(
        value_exploration(&report).evidence,
        ids(&[SQL_M, YAML_SOURCE])
    );
    assert_eq!(report.no_impact_evidence(), None);
    assert_eq!(described(&report), [SQL_M, YAML_SOURCE]);
    assert!(
        report
            .evidence
            .iter()
            .any(|record| record.id.as_str() == YAML_SOURCE && record.is_assumption())
    );

    // No output of `d` reads `amount`. That rests on the YAML list being
    // taken as exhaustive: `m` has no other column that `d.id` could be.
    let report = impact(&manifest, "m", "amount");
    assert!(report.proves_no_impact(), "{report:?}");
    assert_eq!(
        report.no_impact_evidence(),
        Some(ids(&[SQL_M, YAML_SOURCE]))
    );
    assert_eq!(described(&report), [SQL_M, YAML_SOURCE]);
}

#[test]
fn an_impact_path_does_not_cite_what_only_another_input_relied_on() {
    let manifest = project(
        &[
            ("a", &["raw_orders"], "SELECT * FROM raw_orders"),
            ("b", &["other"], "SELECT * FROM other"),
            (
                "d",
                &["a", "b"],
                "SELECT a.x + b.y AS s FROM a CROSS JOIN b",
            ),
        ],
        &[("raw_orders", &["x"]), ("other", &["y"])],
    );
    let report = impact(&manifest, "a", "x");
    assert_eq!(report.confirmed.len(), 1, "{report:?}");
    // The qualified reference `a.x` relies on `a` alone.
    assert_eq!(
        report.confirmed[0].evidence,
        ids(&["sql:model.proj.a", YAML_SOURCE])
    );
    // Concluding that `d.s` is all of `d` that reads the input relies on
    // the lineage of `d.s` as a whole, `b.y` included.
    assert_eq!(
        value_exploration(&report).evidence,
        ids(&[
            "sql:model.proj.a",
            "sql:model.proj.b",
            "yaml:source.proj.raw.other",
            YAML_SOURCE
        ])
    );
}

#[test]
fn a_possible_impact_cites_what_left_its_edge_unresolved() {
    // `elsewhere` has no schema, so `x` may come from it or from `a`.
    let manifest = project(
        &[
            ("a", &["raw_orders"], "SELECT * FROM raw_orders"),
            ("d", &["a"], "SELECT x FROM a CROSS JOIN elsewhere"),
            ("e", &["d"], "SELECT x FROM d"),
        ],
        &[("raw_orders", &["x"])],
    );
    let report = impact(&manifest, "a", "x");
    assert!(report.confirmed.is_empty(), "{report:?}");
    let possible: Vec<(&str, &[EvidenceId])> = report
        .possible
        .iter()
        .map(|impact| (impact.target.model.as_str(), impact.evidence.as_slice()))
        .collect();
    assert_eq!(
        possible,
        [
            ("d", ids(&["sql:model.proj.a", YAML_SOURCE]).as_slice()),
            // The known edge from `d` adds its own evidence to the path.
            (
                "e",
                ids(&["sql:model.proj.a", "sql:model.proj.d", YAML_SOURCE]).as_slice()
            ),
        ]
    );
    assert!(matches!(
        value_exploration(&report).status,
        ExplorationStatus::Incomplete { .. }
    ));
    assert_eq!(report.no_impact_evidence(), None);
}

#[test]
fn an_impact_path_cites_what_placed_its_target() {
    // `d.target` is the second output only because `b.*` is one column wide,
    // which rests on the YAML list of `raw_b`.
    let manifest = project(
        &[
            ("a", &[], "SELECT 1 AS id"),
            ("b", &["raw_b"], "SELECT * FROM raw_b"),
            (
                "d",
                &["a", "b"],
                "SELECT b.*, a.id AS target FROM b CROSS JOIN a",
            ),
        ],
        &[("raw_b", &["y"])],
    );
    let report = impact(&manifest, "a", "id");
    assert_eq!(report.confirmed.len(), 1, "{report:?}");
    let confirmed = &report.confirmed[0];
    assert_eq!(confirmed.target.column, "target");
    assert_eq!(
        confirmed.evidence,
        ids(&[
            "sql:model.proj.a",
            "sql:model.proj.b",
            "yaml:source.proj.raw.raw_b"
        ])
    );
}
