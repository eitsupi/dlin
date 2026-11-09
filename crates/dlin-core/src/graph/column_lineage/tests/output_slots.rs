//! Output slots: the order, duplicates, quoting and anonymous columns of a
//! model's SQL output, and how other models, impact searches, the column
//! selection and the cache treat them.

use super::*;

const CACHE_DIALECT: DlinDialect = DlinDialect::Generic;

fn model_id(name: &str) -> String {
    format!("model.proj.{name}")
}

/// A project of models given as `(name, dependency names, compiled SQL)`.
/// Models have no YAML columns, so their outputs are what the SQL says.
fn project(models: &[(&str, &[&str], &str)]) -> Manifest {
    let nodes: serde_json::Map<String, serde_json::Value> = models
        .iter()
        .map(|(name, deps, sql)| {
            let deps: Vec<_> = deps.iter().map(|dep| model_id(dep)).collect();
            (
                model_id(name),
                serde_json::json!({
                    "unique_id": model_id(name),
                    "name": name,
                    "resource_type": "model",
                    "depends_on": {"nodes": deps},
                    "compiled_code": sql,
                    "columns": {},
                }),
            )
        })
        .collect();
    serde_json::from_value(serde_json::json!({"nodes": nodes, "sources": {}})).unwrap()
}

fn single(manifest: &Manifest, model: &str, dialect: DlinDialect) -> ModelColumnLineage {
    compute_column_lineage(
        manifest,
        model,
        dialect,
        &mut ColumnLineageCache::disabled(),
    )
}

fn cross(manifest: &Manifest, model: &str, dialect: DlinDialect) -> ModelColumnLineage {
    compute_cross_model_column_lineage(
        manifest,
        model,
        dialect,
        &mut ColumnLineageCache::disabled(),
    )
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

fn terminals(slot: &OutputSlot<ColumnSource>) -> Vec<(String, String)> {
    let mut pairs: Vec<_> = slot
        .lineage
        .known()
        .iter()
        .map(|source| (source.table.clone(), source.column.to_string()))
        .collect();
    pairs.sort();
    pairs
}

fn pair(table: &str, column: &str) -> (String, String) {
    (table.to_string(), column.to_string())
}

fn value_gaps(report: &ColumnImpactReport) -> Vec<&TraceGap> {
    report
        .gaps()
        .into_iter()
        .filter(|(kind, _)| *kind == DependencyKind::Value)
        .map(|(_, gap)| gap)
        .collect()
}

// --- The slots and the schema provided downstream agree ---

#[test]
fn lineage_output_and_downstream_schema_read_the_engine_output_the_same_way() {
    let dialect = DlinDialect::PostgreSQL;
    let manifest = project(&[(
        "m",
        &[],
        r#"SELECT z, 1 + 1, a AS x, b AS "X", c AS x, u.* FROM raw.t, unknown_u u"#,
    )]);
    let result = single(&manifest, "m", dialect);
    let knowledge = schema::SchemaContext::new(&manifest, dialect)
        .knowledge_of(&model_id("m"))
        .unwrap();
    let evidence = knowledge.adopted_evidence().unwrap();

    // The schema lists the named slots, in the order and with the ordinals of
    // the lineage output, spelled the same way.
    let from_lineage: Vec<_> = result
        .slots()
        .iter()
        .filter_map(|slot| {
            slot.name
                .ident()
                .map(|ident| (ident.to_string(), slot.ordinal))
        })
        .collect();
    let from_schema: Vec<_> = evidence
        .columns
        .iter()
        .map(|column| (Ident::from_engine(&column.name).to_string(), column.ordinal))
        .collect();
    assert_eq!(from_lineage, from_schema);
    assert_eq!(
        from_schema,
        [
            ("z".to_string(), Some(0)),
            ("x".to_string(), Some(2)),
            (r#""X""#.to_string(), Some(3)),
            ("x".to_string(), Some(4)),
        ]
    );

    // What the schema cannot name is what the lineage output has as an
    // anonymous slot or as an unexpanded star.
    let anonymous = result
        .slots()
        .iter()
        .filter(|slot| slot.name == OutputName::Anonymous)
        .count();
    let stars = match &result.output.as_ref().unwrap().shape {
        OutputShape::Open { unexpanded } => unexpanded.len(),
        OutputShape::Closed => 0,
    };
    assert_eq!(anonymous, 1);
    assert_eq!(stars, 1);
    assert_eq!(evidence.unknown_segments.len(), anonymous + stars);
}

// --- Order, duplicates and anonymous columns ---

#[test]
fn outputs_follow_sql_projection_order_with_stable_slot_ids() {
    let manifest = project(&[("m", &[], "SELECT z, a, m FROM raw.t")]);
    let result = single(&manifest, "m", DlinDialect::Generic);

    assert_eq!(result.names(), ["z", "a", "m"]);
    let ids: Vec<_> = result.slots().iter().map(|slot| slot.id.index()).collect();
    let ordinals: Vec<_> = result.slots().iter().map(|slot| slot.ordinal).collect();
    assert_eq!(ids, [0, 1, 2]);
    assert_eq!(ordinals, [Some(0), Some(1), Some(2)]);
    assert_eq!(result.output.as_ref().unwrap().shape, OutputShape::Closed);
    assert_counts(&result, 3, 3, 0);
}

#[test]
fn duplicate_outputs_are_two_slots_with_their_own_lineage() {
    let manifest = project(&[("m", &[], "SELECT a AS x, b AS x FROM raw.t")]);
    let result = single(&manifest, "m", DlinDialect::Generic);

    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.names(), ["x", "x"]);
    assert_eq!(terminals(&result.slots()[0]), [pair("raw.t", "a")]);
    assert_eq!(terminals(&result.slots()[1]), [pair("raw.t", "b")]);
    assert_ne!(result.slots()[0].id, result.slots()[1].id);
    assert_counts(&result, 2, 2, 0);
}

#[test]
fn anonymous_output_is_its_own_slot_without_a_name() {
    let manifest = project(&[("m", &[], "SELECT 1, a FROM raw.t")]);
    let result = single(&manifest, "m", DlinDialect::Generic);

    let slots = result.slots();
    assert_eq!(slots[0].name, OutputName::Anonymous);
    assert_eq!(slots[0].ordinal, Some(0));
    assert!(slots[0].lineage.known().is_empty() && slots[0].lineage.is_complete());
    assert_eq!(slots[1].name.to_string(), "a");
    assert_eq!(slots[1].ordinal, Some(1));
    // The display text of the anonymous slot is not an identity.
    let output = result.output.as_ref().unwrap();
    assert_eq!(
        output.lookup(&Ident::unquoted("?column?"), DlinDialect::Generic),
        SlotLookup::Absent
    );
    assert_counts(&result, 2, 2, 0);
}

// --- Cross-model tracing by slot ---

#[test]
fn naming_a_duplicated_output_is_ambiguous_and_not_the_first_slot() {
    let manifest = project(&[
        ("up", &[], "SELECT a AS x, b AS x, c FROM raw.t"),
        ("down", &["up"], "SELECT x, c FROM up"),
    ]);
    let result = cross(&manifest, "down", DlinDialect::Generic);

    // The engine already refuses to pick one of the two outputs of `up`: the
    // reference is partial with an ambiguity reason and no source of `a` or
    // `b` is reported.
    let x = &result.slots()[0];
    assert!(x.lineage.is_partial());
    assert!(x.lineage.known().is_empty());
    assert!(matches!(
        x.lineage.unresolved(),
        [UnresolvedContribution {
            reason: UnresolvedReason::Ambiguous { .. },
            ..
        }]
    ));

    // An unrelated column of the same model is still traced through its slot.
    let c = &result.slots()[1];
    assert!(c.lineage.is_complete());
    assert_eq!(terminals(c), [pair("raw.t", "c")]);
    assert_eq!(c.lineage.known()[0].model_path.len(), 1);
    assert_eq!(c.lineage.known()[0].model_path[0].output.slot.index(), 2);
}

#[test]
fn star_over_duplicated_outputs_keeps_the_source_and_records_the_ambiguity() {
    let manifest = project(&[
        ("up", &[], "SELECT a AS x, b AS x, c FROM raw.t"),
        ("down", &["up"], "SELECT * FROM up"),
    ]);
    let result = cross(&manifest, "down", DlinDialect::Generic);

    assert_eq!(result.names(), ["x", "x", "c"]);
    for x in &result.slots()[..2] {
        // Neither output of `up` is chosen. The source stays at `up`, and the
        // output is partial with a reason that lists both candidates.
        assert!(x.lineage.is_partial());
        assert_eq!(terminals(x), [pair("up", "x")]);
        let [contribution] = x.lineage.unresolved() else {
            panic!("expected one unresolved contribution: {x:?}");
        };
        let UnresolvedReason::AmbiguousOutput {
            relation,
            column,
            candidates,
        } = &contribution.reason
        else {
            panic!("unexpected reason: {:?}", contribution.reason);
        };
        assert_eq!(relation.to_string(), "up");
        assert_eq!(*column, Ident::unquoted("x"));
        let slots: Vec<_> = candidates.iter().map(|c| c.slot.index()).collect();
        assert_eq!(slots, [0, 1]);
        assert!(candidates.iter().all(|c| c.model == "up"));
        // The unresolved part arose in the reading model, at the root output.
        assert_eq!(contribution.origin.model, "down");
        assert_eq!(contribution.origin.slot, x.id);
        assert!(contribution.model_path.is_empty());
    }
    assert_counts(&result, 3, 1, 2);
    assert_eq!(terminals(&result.slots()[2]), [pair("raw.t", "c")]);
}

#[test]
fn following_a_slot_keeps_the_slot_that_the_name_identified() {
    // `b` is the second slot of `up` and the only one named `y`.
    let manifest = project(&[
        ("up", &[], "SELECT a AS x, b AS y FROM raw.t"),
        ("down", &["up"], "SELECT y FROM up"),
    ]);
    let result = cross(&manifest, "down", DlinDialect::Generic);

    let y = &result.slots()[0];
    assert_eq!(terminals(y), [pair("raw.t", "b")]);
    let hop = &y.lineage.known()[0].model_path[0];
    assert_eq!(hop.output.unique_id, model_id("up"));
    assert_eq!(hop.output.slot.index(), 1);
    assert_eq!(hop.output.column, "y");
}

/// The order that a model proves is kept when another model reads it, so a
/// positional set operation over models aligns like the inline-query form.
#[test]
fn positional_set_operation_over_models_aligns_by_the_proven_slot_order() {
    for dialect in [DlinDialect::PostgreSQL, DlinDialect::Generic] {
        let manifest = project(&[
            ("first", &[], "SELECT z, a FROM raw_first"),
            ("second", &[], "SELECT p, q FROM raw_second"),
            (
                "combined",
                &["first", "second"],
                "SELECT * FROM first UNION ALL SELECT p, q FROM second",
            ),
        ]);
        let result = cross(&manifest, "combined", dialect);

        assert!(result.errors.is_empty(), "{dialect:?}: {:?}", result.errors);
        assert_eq!(result.names(), ["z", "a"], "{dialect:?}");
        assert_eq!(
            terminals(&result.slots()[0]),
            [pair("raw_first", "z"), pair("raw_second", "p")],
            "{dialect:?}"
        );
        assert_eq!(
            terminals(&result.slots()[1]),
            [pair("raw_first", "a"), pair("raw_second", "q")],
            "{dialect:?}"
        );
        assert_counts(&result, 2, 2, 0);

        // The inline-query form gives the same alignment.
        let inline = project(&[(
            "combined",
            &[],
            "SELECT z, a FROM raw_first UNION ALL SELECT p, q FROM raw_second",
        )]);
        let inline = single(&inline, "combined", dialect);
        assert_eq!(inline.names(), result.names());
        for (inline_slot, slot) in inline.slots().iter().zip(result.slots()) {
            assert_eq!(terminals(inline_slot), terminals(slot), "{dialect:?}");
        }
    }
}

/// A CTE and a separate model of the same query give the same terminal
/// sources and completeness. Model path and evidence kind are not compared.
#[test]
fn cte_form_and_split_model_form_give_the_same_terminals() {
    let dialect = DlinDialect::PostgreSQL;
    let cte = project(&[(
        "d",
        &[],
        "WITH m AS (SELECT z, a FROM raw.t) SELECT m.z FROM m",
    )]);
    let split = project(&[
        ("m", &[], "SELECT z, a FROM raw.t"),
        ("d", &["m"], "SELECT m.z FROM m"),
    ]);
    let cte = cross(&cte, "d", dialect);
    let split = cross(&split, "d", dialect);

    // Both forms are well defined: one output with known sources.
    assert!(cte.errors.is_empty() && split.errors.is_empty());
    assert_eq!(cte.names(), ["z"]);
    assert_eq!(split.names(), ["z"]);
    assert!(!cte.slots()[0].lineage.known().is_empty());
    assert_eq!(terminals(&cte.slots()[0]), [pair("raw.t", "z")]);
    assert_eq!(terminals(&split.slots()[0]), terminals(&cte.slots()[0]));
    assert!(cte.slots()[0].lineage.is_complete());
    assert!(split.slots()[0].lineage.is_complete());
    // The split form passes through the model `m`; the CTE form does not.
    assert!(cte.slots()[0].lineage.known()[0].model_path.is_empty());
    assert_eq!(split.slots()[0].lineage.known()[0].model_path.len(), 1);
}

// --- Quoting: lookups follow each dialect's identifier rules ---

/// Three outputs spelled `"Id"`, `id` and `"ID"`.
fn mixed_case_outputs(dialect: DlinDialect) -> ModelColumnLineage {
    let manifest = project(&[(
        "m",
        &[],
        r#"SELECT a AS "Id", b AS id, c AS "ID" FROM raw.t"#,
    )]);
    single(&manifest, "m", dialect)
}

fn lookup(result: &ModelColumnLineage, name: &str, dialect: DlinDialect) -> SlotLookup {
    result
        .output
        .as_ref()
        .unwrap()
        .lookup(&Ident::parse_reference(name), dialect)
}

fn unique(slot: usize) -> SlotLookup {
    SlotLookup::Unique(OutputSlotId::new(slot))
}

fn ambiguous(slots: &[usize]) -> SlotLookup {
    SlotLookup::Ambiguous(slots.iter().copied().map(OutputSlotId::new).collect())
}

#[test]
fn postgresql_lookup_folds_unquoted_names_to_lower_case() {
    let dialect = DlinDialect::PostgreSQL;
    let result = mixed_case_outputs(dialect);
    assert_eq!(result.names(), [r#""Id""#, "id", r#""ID""#]);

    // An unquoted reference is folded to lower case: it finds the unquoted
    // `id` whatever its own case, and never a quoted mixed-case name.
    assert_eq!(lookup(&result, "id", dialect), unique(1));
    assert_eq!(lookup(&result, "ID", dialect), unique(1));
    assert_eq!(lookup(&result, "Id", dialect), unique(1));
    // A quoted reference matches the exact spelling only.
    assert_eq!(lookup(&result, r#""Id""#, dialect), unique(0));
    assert_eq!(lookup(&result, r#""ID""#, dialect), unique(2));
    assert_eq!(lookup(&result, r#""id""#, dialect), unique(1));
    assert_eq!(lookup(&result, r#""iD""#, dialect), SlotLookup::Absent);
}

#[test]
fn snowflake_lookup_folds_unquoted_names_to_upper_case() {
    let dialect = DlinDialect::Snowflake;
    let result = mixed_case_outputs(dialect);

    // The unquoted `id` and the quoted `"ID"` are the same Snowflake column,
    // so any unquoted spelling of it is ambiguous between the two.
    assert_eq!(lookup(&result, "id", dialect), ambiguous(&[1, 2]));
    assert_eq!(lookup(&result, "Id", dialect), ambiguous(&[1, 2]));
    assert_eq!(lookup(&result, r#""ID""#, dialect), ambiguous(&[1, 2]));
    // The quoted mixed-case name is a different column, found exactly.
    assert_eq!(lookup(&result, r#""Id""#, dialect), unique(0));
    // A quoted lower-case reference does not match the folded upper-case one.
    assert_eq!(lookup(&result, r#""id""#, dialect), SlotLookup::Absent);
}

#[test]
fn bigquery_lookup_ignores_case_and_quoting() {
    let dialect = DlinDialect::BigQuery;
    let manifest = project(&[("m", &[], "SELECT a AS `Id`, b AS other FROM raw.t")]);
    let result = single(&manifest, "m", dialect);
    assert_eq!(result.names(), ["`Id`", "other"]);

    for reference in ["id", "ID", "Id", "`id`", "`ID`"] {
        assert_eq!(
            lookup(&result, reference, dialect),
            unique(0),
            "{reference}"
        );
    }
    assert_eq!(lookup(&result, "OTHER", dialect), unique(1));

    // Names that differ only in case or quoting are the same column.
    let result = mixed_case_outputs(dialect);
    for reference in ["id", "Id", r#""ID""#] {
        assert_eq!(
            lookup(&result, reference, dialect),
            ambiguous(&[0, 1, 2]),
            "{reference}"
        );
    }
}

#[test]
fn cross_model_tracing_matches_quoted_and_unquoted_names_by_the_dialect() {
    // (dialect, upstream alias, downstream reference, traced through `up`)
    let cases = [
        (DlinDialect::PostgreSQL, r#""Id""#, r#""Id""#, true),
        (DlinDialect::PostgreSQL, "id", "ID", true),
        (DlinDialect::PostgreSQL, r#""Id""#, "id", false),
        (DlinDialect::Snowflake, r#""ID""#, "id", true),
        (DlinDialect::Snowflake, "id", r#""ID""#, true),
        (DlinDialect::Snowflake, r#""id""#, "id", false),
        (DlinDialect::BigQuery, "`Id`", "id", true),
        (DlinDialect::BigQuery, "id", "`ID`", true),
    ];
    for (dialect, alias, reference, traced) in cases {
        let manifest = project(&[
            ("up", &[], &format!("SELECT a AS {alias} FROM raw.t")),
            ("down", &["up"], &format!("SELECT {reference} AS r FROM up")),
        ]);
        let result = cross(&manifest, "down", dialect);
        let r = &result.slots()[0];
        let context = format!("{dialect:?}: {alias} read as {reference}: {r:?}");
        if traced {
            assert_eq!(terminals(r), [pair("raw.t", "a")], "{context}");
            assert_eq!(r.lineage.known()[0].model_path.len(), 1, "{context}");
        } else {
            // The name does not identify an output of `up`, so the source
            // stays at `up` instead of being matched by spelling.
            assert!(
                r.lineage
                    .known()
                    .iter()
                    .all(|source| source.model_path.is_empty()),
                "{context}"
            );
            assert!(
                r.lineage.known().iter().all(|s| s.table == "up"),
                "{context}"
            );
        }
    }
}

// --- Impact distinguishes slots ---

#[test]
fn impact_tells_duplicate_outputs_apart() {
    let manifest = project(&[
        ("base", &[], "SELECT a, b FROM raw.t"),
        ("mid", &["base"], "SELECT a AS x, b AS x FROM base"),
    ]);

    let from_a = impact(&manifest, "base", "a");
    let from_b = impact(&manifest, "base", "b");
    let targets = |report: &ColumnImpactReport| -> Vec<(String, usize)> {
        report
            .confirmed
            .iter()
            .map(|c| (c.target.column.clone(), c.target.slot.index()))
            .collect()
    };
    assert_eq!(targets(&from_a), [("x".to_string(), 0)]);
    assert_eq!(targets(&from_b), [("x".to_string(), 1)]);
    assert_eq!(from_a.input.as_ref().unwrap().slot.index(), 0);
    assert_eq!(from_b.input.as_ref().unwrap().slot.index(), 1);
}

#[test]
fn impact_from_an_ambiguous_name_reports_the_candidates_and_still_searches_by_name() {
    let manifest = project(&[
        ("mid", &[], "SELECT a AS x, b AS x FROM raw.t"),
        ("top", &["mid"], "SELECT * FROM mid"),
    ]);
    let report = impact(&manifest, "mid", "x");

    assert_eq!(report.input, None);
    assert!(!report.proves_no_impact());
    let gaps = value_gaps(&report);
    let Some(TraceGap::UnidentifiedInput { candidates, .. }) = gaps
        .iter()
        .find(|gap| matches!(gap, TraceGap::UnidentifiedInput { .. }))
    else {
        panic!("gaps: {:?}", report.exploration);
    };
    let slots: Vec<_> = candidates.iter().map(|c| c.slot.index()).collect();
    assert_eq!(slots, [0, 1]);
    assert_eq!(report.errors.len(), 1);
    assert_eq!(
        report.errors[0].kind,
        ColumnLineageErrorKind::ColumnAmbiguous
    );
    assert!(report.errors[0].is_fatal());

    // The dependents name the column, so their outputs that read it are found
    // by that name; the edge starts at the unlisted input.
    assert!(!report.confirmed.is_empty());
    for impact in &report.confirmed {
        assert_eq!(impact.target.model, "top");
        assert!(matches!(
            &impact.path[0].from,
            DependencyRef::UnidentifiedColumn { column, .. } if column == "x"
        ));
    }
}

#[test]
fn impact_from_an_unlisted_name_is_not_a_no_impact_conclusion() {
    let manifest = project(&[
        ("mid", &[], "SELECT a FROM raw.t"),
        ("top", &["mid"], "SELECT a FROM mid"),
    ]);
    let report = impact(&manifest, "mid", "missing");
    assert!(!report.proves_no_impact());
    assert!(report.confirmed.is_empty());
    assert!(matches!(
        value_gaps(&report).as_slice(),
        [TraceGap::UnidentifiedInput { candidates, .. }] if candidates.is_empty()
    ));
    assert_eq!(
        report.errors[0].kind,
        ColumnLineageErrorKind::ColumnNotFound
    );

    // A model that has no output cannot identify the column either, but a
    // dependent that names it is still found, and the search goes on by slot.
    let broken = project(&[
        ("mid", &[], "SELECT FROM"),
        ("top", &["mid"], "SELECT a AS renamed FROM mid"),
        ("leaf", &["top"], "SELECT renamed FROM top"),
    ]);
    let report = impact(&broken, "mid", "a");
    assert!(!report.proves_no_impact());
    assert_eq!(report.errors[0].kind, ColumnLineageErrorKind::ParseFailure);
    let targets: Vec<_> = report
        .confirmed
        .iter()
        .map(|c| (c.target.model.as_str(), c.target.column.as_str()))
        .collect();
    assert_eq!(targets, [("leaf", "renamed"), ("top", "renamed")]);
    assert!(matches!(
        &report.confirmed[1].path[0].from,
        DependencyRef::UnidentifiedColumn { model, .. } if model == "mid"
    ));
    assert!(matches!(
        &report.confirmed[0].path[1].from,
        DependencyRef::Column(output) if output.model == "top"
    ));
}

#[test]
fn impact_from_a_name_hidden_behind_an_open_shape_searches_by_name() {
    let manifest = project(&[
        ("mid", &[], "SELECT a, * FROM unknown_rel"),
        ("top", &["mid"], "SELECT mid.hidden AS seen FROM mid"),
    ]);
    let report = impact(&manifest, "mid", "hidden");

    assert_eq!(report.input, None);
    assert!(!report.proves_no_impact());
    assert!(matches!(
        value_gaps(&report).as_slice(),
        [TraceGap::UnidentifiedInput { candidates, .. }] if candidates.is_empty()
    ));
    // Not found, but possibly behind the star: not fatal.
    assert_eq!(
        report.errors[0].kind,
        ColumnLineageErrorKind::ColumnIndeterminate
    );
    let targets: Vec<_> = report
        .confirmed
        .iter()
        .map(|c| (c.target.model.as_str(), c.target.column.as_str()))
        .collect();
    assert_eq!(targets, [("top", "seen")]);
}

#[test]
fn impact_reports_a_read_of_a_duplicated_name_as_a_gap() {
    // base.a feeds mid.x (slot 0), but mid also has another x (slot 1). The
    // reader `top` takes x from mid, and it is not known which one.
    for reader_sql in ["SELECT * FROM mid", "SELECT x FROM mid"] {
        let manifest = project(&[
            ("base", &[], "SELECT a, b FROM raw.t"),
            ("mid", &["base"], "SELECT a AS x, b AS x FROM base"),
            ("top", &["mid"], reader_sql),
        ]);
        let report = impact(&manifest, "base", "a");

        let targets: Vec<_> = report
            .confirmed
            .iter()
            .map(|c| (c.target.model.as_str(), c.target.slot.index()))
            .collect();
        assert_eq!(targets, [("mid", 0)], "{reader_sql}");
        assert!(!report.proves_no_impact(), "{reader_sql}");
        let gaps = value_gaps(&report);
        assert!(
            gaps.iter().all(|gap| matches!(
                gap,
                TraceGap::AmbiguousInput { .. } | TraceGap::UnresolvedLineage { .. }
            )),
            "{reader_sql}: {gaps:?}"
        );
        assert!(
            gaps.iter().all(|gap| match gap {
                TraceGap::AmbiguousInput { at, candidates } =>
                    at.model == "top" && candidates.len() == 2,
                TraceGap::UnresolvedLineage { at, .. } => at.model == "top",
                _ => false,
            }),
            "{reader_sql}: {gaps:?}"
        );
    }

    // Reading both outputs through a star is the case that the slot lookup,
    // not the engine, has to catch.
    let manifest = project(&[
        ("base", &[], "SELECT a, b FROM raw.t"),
        ("mid", &["base"], "SELECT a AS x, b AS x FROM base"),
        ("top", &["mid"], "SELECT * FROM mid"),
    ]);
    let report = impact(&manifest, "base", "a");
    assert!(
        value_gaps(&report)
            .iter()
            .any(|gap| matches!(gap, TraceGap::AmbiguousInput { .. })),
        "{:?}",
        report.exploration
    );
}

#[test]
fn impact_over_a_dependent_with_unlisted_columns_is_a_gap() {
    // `reader` lists `a` and leaves the columns of an unknown relation open.
    let manifest = project(&[
        ("base", &[], "SELECT a FROM raw.t"),
        (
            "reader",
            &["base"],
            "SELECT b.a, u.* FROM base b CROSS JOIN unknown_rel u",
        ),
    ]);
    let report = impact(&manifest, "base", "a");

    assert_eq!(report.confirmed.len(), 1);
    assert_eq!(report.confirmed[0].target.model, "reader");
    assert!(!report.proves_no_impact());
    let [
        TraceGap::OpenOutput {
            model, unexpanded, ..
        },
    ] = value_gaps(&report).as_slice()
    else {
        panic!("gaps: {:?}", report.exploration);
    };
    assert_eq!(model, "reader");
    assert_eq!(unexpanded.len(), 1);
    assert_eq!(
        unexpanded[0].relation.as_ref().unwrap().to_string(),
        "unknown_rel"
    );

    // The dependent's counts add up: listed = complete + partial.
    let reader = single(&manifest, "reader", DlinDialect::Generic);
    assert_eq!(
        reader.listed_columns,
        reader.value_complete_columns + reader.value_partial_columns
    );

    // A dependent that is only a star lists nothing, and is still a gap.
    let manifest = project(&[
        ("base", &[], "SELECT a FROM raw.t"),
        ("reader", &["base"], "SELECT * FROM unknown_rel"),
    ]);
    let report = impact(&manifest, "base", "a");
    assert!(report.confirmed.is_empty());
    assert!(matches!(
        value_gaps(&report).as_slice(),
        [TraceGap::OpenOutput { model, .. }] if model == "reader"
    ));
    let reader = single(&manifest, "reader", DlinDialect::Generic);
    assert_counts(&reader, 0, 0, 0);
}

// --- Column selection ---

#[test]
fn selecting_a_duplicated_name_reports_ambiguity_and_picks_neither() {
    let manifest = project(&[("m", &[], "SELECT a AS x, b AS x, c FROM raw.t")]);
    let dialect = DlinDialect::Generic;

    let mut result = single(&manifest, "m", dialect);
    result.select_columns(&["x"], dialect);
    assert!(result.slots().is_empty());
    assert_counts(&result, 0, 0, 0);
    let [error] = result.errors.as_slice() else {
        panic!("{:?}", result.errors);
    };
    assert_eq!(error.kind, ColumnLineageErrorKind::ColumnAmbiguous);
    assert_eq!(error.column_name(), Some("x"));

    let mut result = single(&manifest, "m", dialect);
    result.select_columns(&["c", "missing"], dialect);
    assert_eq!(result.names(), ["c"]);
    assert_counts(&result, 1, 1, 0);
    // The slot id of the kept column is the one it had in the full output.
    assert_eq!(result.slots()[0].id.index(), 2);
    let [error] = result.errors.as_slice() else {
        panic!("{:?}", result.errors);
    };
    assert_eq!(error.kind, ColumnLineageErrorKind::ColumnNotFound);
    assert_eq!(error.column_name(), Some("missing"));
}

#[test]
fn selecting_a_name_behind_an_open_shape_is_indeterminate_not_missing() {
    let manifest = project(&[("m", &[], "SELECT a, * FROM unknown_rel")]);
    let dialect = DlinDialect::Generic;
    let mut result = single(&manifest, "m", dialect);
    result.select_columns(&["hidden"], dialect);
    let [error] = result.errors.as_slice() else {
        panic!("{:?}", result.errors);
    };
    assert_eq!(error.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(!error.is_fatal());
    assert!(error.hint.is_some());
}

#[test]
fn selecting_a_quoted_name_uses_the_dialect_rules() {
    let manifest = project(&[("m", &[], r#"SELECT a AS "Id", b AS other FROM raw.t"#)]);
    let mut result = single(&manifest, "m", DlinDialect::PostgreSQL);
    // PostgreSQL folds an unquoted `Id` to `id`, which is not the quoted name.
    result.select_columns(&["Id"], DlinDialect::PostgreSQL);
    assert!(result.slots().is_empty());
    assert_eq!(
        result.errors[0].kind,
        ColumnLineageErrorKind::ColumnNotFound
    );

    let mut result = single(&manifest, "m", DlinDialect::PostgreSQL);
    result.select_columns(&[r#""Id""#], DlinDialect::PostgreSQL);
    assert_eq!(result.names(), [r#""Id""#]);
    assert!(result.errors.is_empty());
}

// --- Cache ---

fn slot_of(
    id: usize,
    ordinal: Option<usize>,
    name: OutputName,
    lineage: ValueLineage<InternalColumnSource>,
) -> OutputSlot<InternalColumnSource> {
    OutputSlot {
        id: OutputSlotId::new(id),
        ordinal,
        name,
        transformation: TransformationType::Expression,
        lineage,
    }
}

fn source_of(relation: &str, column: Ident) -> InternalColumnSource {
    InternalColumnSource {
        relation: RelationRef::parse(relation).unwrap(),
        column,
        model_path: vec![hop(
            "model.proj.up",
            "up",
            3,
            "inner",
            TransformationType::Cast,
        )],
    }
}

/// A result with a partial slot, duplicate names, quoting of every style, an
/// anonymous slot and an open shape with unexpanded stars.
fn rich_result() -> InternalModelColumnLineage {
    let origin = ModelOutputRef {
        unique_id: "model.proj.m".to_string(),
        model: "m".to_string(),
        slot: OutputSlotId::new(0),
        column: "x".to_string(),
    };
    let partial = ValueLineage::new(
        vec![source_of(
            "raw.t",
            Ident::quoted("Mixed Case", Quote::Double),
        )],
        vec![
            UnresolvedContribution {
                reason: UnresolvedReason::AmbiguousOutput {
                    relation: RelationRef::bare("up"),
                    column: Ident::unquoted("x"),
                    candidates: vec![origin.clone()],
                },
                origin: origin.clone(),
                model_path: vec![hop(
                    "model.proj.up",
                    "up",
                    1,
                    "x",
                    TransformationType::Direct,
                )],
            },
            UnresolvedContribution {
                reason: UnresolvedReason::BehindStar {
                    relation: RelationRef::parse("raw.fees").unwrap(),
                },
                origin,
                model_path: vec![],
            },
        ],
    );
    InternalModelColumnLineage {
        unique_id: "model.proj.m".to_string(),
        model: "m".to_string(),
        output: Some(OutputSlots {
            columns: vec![
                slot_of(
                    0,
                    Some(0),
                    OutputName::Explicit(Ident::unquoted("x")),
                    partial,
                ),
                slot_of(
                    1,
                    Some(1),
                    OutputName::Explicit(Ident::unquoted("x")),
                    ValueLineage::complete(vec![source_of(
                        "raw.t",
                        Ident::quoted("tick", Quote::Backtick),
                    )]),
                ),
                slot_of(
                    2,
                    Some(2),
                    OutputName::Inferred(Ident::quoted("Bracket Name", Quote::Bracket)),
                    ValueLineage::complete(vec![]),
                ),
                slot_of(
                    3,
                    Some(3),
                    OutputName::Anonymous,
                    ValueLineage::complete(vec![]),
                ),
                slot_of(
                    4,
                    None,
                    OutputName::Explicit(Ident::quoted("After Star", Quote::Double)),
                    ValueLineage::complete(vec![]),
                ),
            ],
            shape: OutputShape::Open {
                unexpanded: vec![
                    UnexpandedStar {
                        relation: Some(RelationRef::parse("raw.unknown").unwrap()),
                        after_ordinal: Some(3),
                    },
                    UnexpandedStar {
                        relation: None,
                        after_ordinal: None,
                    },
                ],
            },
        }),
        errors: vec![ColumnLineageError {
            kind: ColumnLineageErrorKind::ColumnIndeterminate,
            column: Some("x".to_string()),
            slot: Some(OutputSlotId::new(0)),
            what: "column 'x': origin remains behind a star".to_string(),
            why: None,
            hint: Some("a hint".to_string()),
        }],
    }
}

#[test]
fn cache_round_trip_preserves_partial_shape_duplicates_quoting_and_anonymous_slots() {
    let tmp = tempfile::tempdir().unwrap();
    let stored = rich_result();

    let mut cache = ColumnLineageCache::load(tmp.path(), None);
    cache.insert("model.proj.m", CACHE_DIALECT, 7, stored.clone());
    cache.save();

    let reloaded = ColumnLineageCache::load(tmp.path(), None);
    let hit = reloaded
        .get("model.proj.m", CACHE_DIALECT, 7)
        .expect("the stored entry is read back");
    // Typed equality: nothing is lost or reordered by the round trip.
    assert_eq!(hit, &stored);

    // The pieces that the round trip must keep, named explicitly.
    let output = hit.output.as_ref().unwrap();
    assert!(output.columns[0].lineage.is_partial());
    assert_eq!(output.columns[0].lineage.unresolved().len(), 2);
    assert_eq!(output.columns[0].name, output.columns[1].name);
    assert_ne!(output.columns[0].id, output.columns[1].id);
    assert_eq!(output.columns[3].name, OutputName::Anonymous);
    assert_eq!(output.columns[4].ordinal, None);
    assert!(matches!(
        &output.shape,
        OutputShape::Open { unexpanded } if unexpanded.len() == 2
    ));
    assert_eq!(
        output.columns[0].lineage.known()[0].column,
        Ident::quoted("Mixed Case", Quote::Double)
    );
    assert_eq!(
        output.columns[1].lineage.known()[0].column,
        Ident::quoted("tick", Quote::Backtick)
    );
    assert_eq!(
        output.columns[2].name,
        OutputName::Inferred(Ident::quoted("Bracket Name", Quote::Bracket))
    );
}

#[test]
fn cache_file_in_the_previous_layout_is_discarded() {
    let tmp = tempfile::tempdir().unwrap();
    let path = cache_file_path(tmp.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // The same package version and analysis semantics id as the current
    // build, with an entry in the layout that had counts and a name list.
    let old = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "semantics_id": ANALYSIS_SEMANTICS_ID,
        "entries": {
            "model.proj.m": {
                "semantic_digest": 7,
                "dialect": CACHE_DIALECT.as_str(),
                "lineage": {
                    "model": "m",
                    "traced_columns": 1,
                    "total_columns": 1,
                    "columns": [{
                        "column": "id",
                        "transformation": "direct",
                        "lineage": {"status": "complete", "known": []}
                    }],
                    "errors": []
                }
            }
        }
    });
    std::fs::write(&path, old.to_string()).unwrap();

    // The file does not parse as the current layout, so nothing is read back.
    let cache = ColumnLineageCache::load(tmp.path(), None);
    assert!(cache.get("model.proj.m", CACHE_DIALECT, 7).is_none());
}

fn cache_file_path(project_dir: &std::path::Path) -> std::path::PathBuf {
    project_dir
        .join(CACHE_DIR)
        .join(COLUMN_LINEAGE_CACHE_FILENAME)
}
