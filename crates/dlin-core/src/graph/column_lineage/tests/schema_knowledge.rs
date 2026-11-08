//! What a dependency model provides downstream: its SQL output in SQL order,
//! never a union with its YAML declaration.

use super::*;
use crate::graph::column_lineage::backend::sqllineage::{analyze_sql, single_lineage_statement};
use crate::graph::column_lineage::schema::SchemaContext;
use crate::graph::column_lineage::schema::context::{
    AdoptedSchema, FallbackReason, NodeSchemaKnowledge, SqlOutputState,
};
use crate::graph::column_lineage::schema::evidence::{CoverageBasis, SchemaOrigin};

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

fn trace(manifest: &Manifest, model: &str) -> ModelColumnLineage {
    compute_column_lineage(
        manifest,
        model,
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    )
}

fn sources<'a>(result: &'a ModelColumnLineage, column: &str) -> Vec<(&'a str, &'a str)> {
    let entry = result
        .columns
        .iter()
        .find(|entry| entry.column == column)
        .unwrap_or_else(|| {
            panic!(
                "column '{column}' was not traced; errors: {:?}",
                result.errors
            )
        });
    let mut pairs: Vec<_> = entry
        .lineage
        .known()
        .iter()
        .map(|source| (source.table.as_str(), source.column.as_str()))
        .collect();
    pairs.sort_unstable();
    pairs
}

fn knowledge(manifest: &Manifest, model: &str) -> std::sync::Arc<NodeSchemaKnowledge> {
    SchemaContext::new(manifest, DIALECT)
        .knowledge_of(&model_id(model))
        .expect("known model")
}

fn sql_columns(knowledge: &NodeSchemaKnowledge) -> Vec<(String, Option<usize>)> {
    let SqlOutputState::Available(sql) = &knowledge.sql_output else {
        panic!("expected a SQL output, got {:?}", knowledge.sql_output);
    };
    sql.columns
        .iter()
        .map(|column| (column.name.value.clone(), column.ordinal))
        .collect()
}

fn named(columns: &[(&str, usize)]) -> Vec<(String, Option<usize>)> {
    columns
        .iter()
        .map(|(name, ordinal)| ((*name).to_string(), Some(*ordinal)))
        .collect()
}

/// `first` outputs `z, a` and `second` outputs `p, q`. The positional set
/// operation pairs `z` with `p` and `a` with `q` because the order proven
/// inside `first` survives the model boundary.
fn order_manifest(first_yaml: &[&str]) -> Manifest {
    manifest(&[
        ("first", &[], Some("SELECT z, a FROM raw_first"), first_yaml),
        ("second", &[], Some("SELECT p, q FROM raw_second"), &[]),
        (
            "downstream",
            &["first", "second"],
            Some("SELECT * FROM first UNION ALL SELECT p, q FROM second"),
            &[],
        ),
    ])
}

#[test]
fn sql_order_of_an_upstream_model_pairs_positional_set_operations() {
    for first_yaml in [&[][..], &["a", "z"], &["z", "a"]] {
        let manifest = order_manifest(first_yaml);
        let result = trace(&manifest, "downstream");

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 2);
        assert_eq!(
            sources(&result, "z"),
            [("first", "z"), ("second", "p")],
            "yaml: {first_yaml:?}"
        );
        assert_eq!(
            sources(&result, "a"),
            [("first", "a"), ("second", "q")],
            "yaml: {first_yaml:?}"
        );
    }
}

#[test]
fn sql_order_survives_cross_model_tracing_to_the_terminal_relations() {
    let manifest = order_manifest(&["a", "z"]);
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream",
        DIALECT,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        sources(&result, "z"),
        [("raw_first", "z"), ("raw_second", "p")]
    );
    assert_eq!(
        sources(&result, "a"),
        [("raw_first", "a"), ("raw_second", "q")]
    );
}

#[test]
fn yaml_extra_column_is_not_provided_downstream() {
    let manifest = manifest(&[
        (
            "up",
            &[],
            Some("SELECT z, a FROM raw.t"),
            &["a", "z", "extra"],
        ),
        ("down", &["up"], Some("SELECT * FROM up"), &[]),
    ]);

    let up = knowledge(&manifest, "up");
    assert_eq!(sql_columns(&up), named(&[("z", 0), ("a", 1)]));
    let SqlOutputState::Available(sql) = &up.sql_output else {
        unreachable!()
    };
    assert_eq!(sql.order, sqllineage::SchemaOrder::Proven);
    assert_eq!(sql.coverage, sqllineage::SchemaCoverage::Exhaustive);
    assert_eq!(sql.origin, SchemaOrigin::SqlOutput);
    assert_eq!(sql.coverage_basis, CoverageBasis::Enumerated);
    let yaml = up.yaml_declaration.as_ref().expect("YAML is kept");
    assert_eq!(yaml.origin, SchemaOrigin::YamlDeclaration);
    assert_eq!(up.adopted, AdoptedSchema::Sql(sql.id.clone()));
    assert_eq!(up.adopted_evidence(), Some(sql));
    let difference = up.name_difference(sqllineage::Dialect::Generic);
    assert_eq!(difference.yaml_only, ["extra"]);
    assert!(difference.sql_only.is_empty());

    let down = trace(&manifest, "down");
    assert!(down.errors.is_empty(), "errors: {:?}", down.errors);
    let mut columns: Vec<_> = down.columns.iter().map(|c| c.column.as_str()).collect();
    columns.sort_unstable();
    assert_eq!(columns, ["a", "z"]);

    let catalog = SchemaContext::new(&manifest, DIALECT)
        .catalog_for(manifest.nodes.get(&model_id("down")).unwrap())
        .unwrap();
    assert_eq!(catalog.table_columns("up"), Some(vec!["z", "a"]));
}

#[test]
fn unanalysable_sql_falls_back_to_yaml_and_records_why() {
    let manifest = manifest(&[
        ("broken", &[], Some("SELEKT oops FROM ("), &["a", "b"]),
        ("no_yaml", &[], Some("SELEKT oops FROM ("), &[]),
        ("down", &["broken"], Some("SELECT * FROM broken"), &[]),
    ]);

    let broken = knowledge(&manifest, "broken");
    let SqlOutputState::Failed(detail) = &broken.sql_output else {
        panic!("expected a failure, got {:?}", broken.sql_output);
    };
    let yaml = broken.yaml_declaration.as_ref().unwrap();
    assert_eq!(
        broken.adopted,
        AdoptedSchema::YamlFallback {
            evidence: yaml.id.clone(),
            reason: FallbackReason::SqlAnalysisFailed {
                detail: detail.clone()
            },
        }
    );
    assert!(!detail.is_empty());
    assert_eq!(broken.adopted_evidence(), Some(yaml));

    let no_yaml = knowledge(&manifest, "no_yaml");
    assert!(matches!(no_yaml.sql_output, SqlOutputState::Failed(_)));
    assert_eq!(no_yaml.adopted, AdoptedSchema::None);
    assert_eq!(no_yaml.adopted_evidence(), None);

    let down = trace(&manifest, "down");
    assert!(down.errors.is_empty(), "errors: {:?}", down.errors);
    assert_eq!(sources(&down, "a"), [("broken", "a")]);
    assert_eq!(sources(&down, "b"), [("broken", "b")]);
}

#[test]
fn model_without_sql_adopts_yaml_as_a_fallback() {
    let manifest = manifest(&[("declared", &[], None, &["a", "b"])]);

    let declared = knowledge(&manifest, "declared");
    assert_eq!(declared.sql_output, SqlOutputState::Unavailable);
    let yaml = declared.yaml_declaration.as_ref().unwrap();
    assert_eq!(yaml.coverage, sqllineage::SchemaCoverage::Exhaustive);
    assert_eq!(yaml.coverage_basis, CoverageBasis::AssumedByYamlPolicy);
    assert_eq!(yaml.order, sqllineage::SchemaOrder::Unknown);
    assert_eq!(
        declared.adopted,
        AdoptedSchema::YamlFallback {
            evidence: yaml.id.clone(),
            reason: FallbackReason::SqlUnavailable,
        }
    );
}

#[test]
fn open_upstream_output_is_partial_and_keeps_its_known_prefix() {
    let manifest = manifest(&[
        (
            "up",
            &[],
            Some("SELECT k, t.* FROM unknown_t t"),
            &["k", "other"],
        ),
        ("down", &["up"], Some("SELECT k FROM up"), &[]),
    ]);

    let up = knowledge(&manifest, "up");
    let SqlOutputState::Available(sql) = &up.sql_output else {
        panic!("expected a SQL output, got {:?}", up.sql_output);
    };
    assert_eq!(sql.coverage, sqllineage::SchemaCoverage::Partial);
    assert_eq!(sql.order, sqllineage::SchemaOrder::Unknown);
    assert_eq!(sql_columns(&up), named(&[("k", 0)]));
    assert_eq!(
        sql.unknown_segments,
        [sqllineage::CatalogUnknownSegment {
            after_ordinal: Some(0)
        }]
    );
    // The open SQL output is adopted as it is; YAML does not complete it.
    assert_eq!(up.adopted, AdoptedSchema::Sql(sql.id.clone()));

    let down = trace(&manifest, "down");
    assert!(down.errors.is_empty(), "errors: {:?}", down.errors);
    assert_eq!(sources(&down, "k"), [("up", "k")]);
}

#[test]
fn star_chain_keeps_the_sql_order_of_the_first_model() {
    let manifest = manifest(&[
        ("a", &[], Some("SELECT z, a FROM raw_a"), &["a", "z"]),
        ("b", &["a"], Some("SELECT * FROM a"), &["a", "z"]),
        ("c", &["b"], Some("SELECT * FROM b"), &[]),
        ("second", &[], Some("SELECT p, q FROM raw_second"), &[]),
        (
            "tail",
            &["c", "second"],
            Some("SELECT * FROM c UNION ALL SELECT p, q FROM second"),
            &[],
        ),
    ]);

    for model in ["a", "b", "c"] {
        let knowledge = knowledge(&manifest, model);
        assert_eq!(
            sql_columns(&knowledge),
            named(&[("z", 0), ("a", 1)]),
            "model {model}"
        );
        let SqlOutputState::Available(sql) = &knowledge.sql_output else {
            unreachable!()
        };
        assert_eq!(sql.order, sqllineage::SchemaOrder::Proven, "model {model}");
    }

    let tail = trace(&manifest, "tail");
    assert!(tail.errors.is_empty(), "errors: {:?}", tail.errors);
    assert_eq!(sources(&tail, "z"), [("c", "z"), ("second", "p")]);
    assert_eq!(sources(&tail, "a"), [("c", "a"), ("second", "q")]);
}

/// Duplicate output names stay separate slots, so the downstream reference is
/// ambiguous instead of silently bound to the first.
#[test]
fn duplicate_output_names_are_kept_and_referenced_ambiguously() {
    let manifest = manifest(&[
        ("up", &[], Some("SELECT p AS x, q AS x FROM raw_t"), &["x"]),
        ("down", &["up"], Some("SELECT x FROM up"), &[]),
        ("qualified", &["up"], Some("SELECT up.x FROM up"), &[]),
    ]);

    let up = knowledge(&manifest, "up");
    assert_eq!(sql_columns(&up), named(&[("x", 0), ("x", 1)]));

    for model in ["down", "qualified"] {
        let result = trace(&manifest, model);
        // The ambiguous reference is listed as a partial column with no known
        // source, explained by one diagnostic.
        assert_eq!(result.columns.len(), 1, "model {model}: {result:?}");
        assert!(result.columns[0].lineage.known().is_empty());
        assert!(
            matches!(
                result.columns[0].lineage.unresolved(),
                [UnresolvedContribution {
                    reason: UnresolvedReason::Ambiguous { .. },
                    ..
                }]
            ),
            "model {model}: {result:?}"
        );
        assert_eq!(result.traced_columns, 0, "model {model}: {result:?}");
        assert_eq!(result.errors.len(), 1, "model {model}: {result:?}");
        assert_eq!(
            result.errors[0].kind,
            ColumnLineageErrorKind::ColumnAmbiguous,
            "model {model}"
        );
    }
}

/// The engine receives a catalog entry that lists one name twice through the
/// production adapter and reports a reference to it as ambiguous.
#[test]
fn catalog_entry_listing_a_name_twice_is_ambiguous_through_the_adapter() {
    let mut catalog = CatalogSnapshot::new();
    catalog.add_table("dup", ["x".to_string(), "x".to_string()]);

    for sql in ["SELECT x FROM dup", "SELECT dup.x FROM dup"] {
        let analysis = analyze_sql(sql, DIALECT, Some(&catalog)).unwrap();
        let statement = single_lineage_statement(&analysis).unwrap();
        let lineage = &statement.output.columns[0].lineage;
        let failure = lineage_failure(lineage).unwrap_or_else(|| panic!("{sql}: {lineage:?}"));
        assert_eq!(failure.kind, OutputFailureKind::Ambiguous, "{sql}");
    }
}

fn cycle_manifest() -> Manifest {
    manifest(&[
        ("a", &["c"], Some("SELECT x, y FROM c"), &["x", "y"]),
        ("b", &["a"], Some("SELECT * FROM a"), &[]),
        ("c", &["b"], Some("SELECT * FROM b"), &[]),
        ("d", &["a"], Some("SELECT * FROM a"), &[]),
        ("selfish", &["selfish"], Some("SELECT * FROM selfish"), &[]),
    ])
}

fn permutations(items: &[&'static str]) -> Vec<Vec<&'static str>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut all = Vec::new();
    for index in 0..items.len() {
        let mut rest = items.to_vec();
        let head = rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, head);
            all.push(tail);
        }
    }
    all
}

/// A back-edge gets no entry and a result that cut an edge is not memoized,
/// so every model gets the same result whichever model is analysed first.
#[test]
fn dependency_cycle_terminates_and_does_not_depend_on_the_query_order() {
    let manifest = cycle_manifest();
    let models = ["a", "b", "c", "d", "selfish"];
    let json = |result: &ModelColumnLineage| serde_json::to_value(result).unwrap();
    let reference: Vec<_> = models
        .iter()
        .map(|model| json(&trace(&manifest, model)))
        .collect();

    for order in permutations(&models) {
        let mut cache = ColumnLineageCache::disabled();
        let mut session = ColumnLineageAnalysis::new(&manifest, DIALECT, &mut cache);
        for model in &order {
            let result = session.compute_column_lineage(model);
            let index = models
                .iter()
                .position(|candidate| candidate == model)
                .unwrap();
            assert_eq!(
                json(&result),
                reference[index],
                "model {model} in order {order:?}"
            );
        }
    }
}

#[test]
fn dependency_cycle_back_edge_gets_no_entry_even_when_yaml_declares_columns() {
    let manifest = cycle_manifest();
    let mut context = SchemaContext::new(&manifest, DIALECT);

    // `a` reads `c`, which reads `b`, which reads `a` again. The edge back to
    // `a` is cut instead of being answered from the YAML of `a`, so `b` and
    // `c` have no named columns.
    let catalog = context
        .catalog_for(manifest.nodes.get(&model_id("a")).unwrap())
        .expect("c is known through its own SQL");
    assert_eq!(catalog.table_columns("c"), Some(vec![]));
}

#[test]
fn memoized_knowledge_is_shared_between_sibling_queries() {
    let manifest = order_manifest(&[]);
    let mut context = SchemaContext::new(&manifest, DIALECT);

    let first = context.knowledge_of(&model_id("first")).unwrap();
    let again = context.knowledge_of(&model_id("first")).unwrap();
    assert!(std::sync::Arc::ptr_eq(&first, &again));
}
