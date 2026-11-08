use super::*;
#[test]
fn test_column_cache_hit() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "test_model".to_string(),
        traced_columns: 1,
        total_columns: 1,
        columns: vec![ColumnLineageEntry {
            column: "id".to_string(),
            transformation: TransformationType::Direct,
            sources: vec![ColumnSource {
                table: "raw".to_string(),
                column: "id".to_string(),
                model_path: vec![],
            }],
        }],
        errors: vec![],
    };
    cache.insert("test_model", DlinDialect::Generic, 0, lineage.into());
    cache.save();

    // Reload from disk
    let cache2 = ColumnLineageCache::load(project_dir, None);
    let hit = cache2.get("test_model", DlinDialect::Generic, 0).unwrap();
    assert_eq!(hit.columns.len(), 1);
    assert_eq!(hit.columns[0].column, "id");
}

#[test]
fn test_column_cache_persists_structural_relation_and_public_output() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let manifest = make_test_manifest();
    let mut cache = ColumnLineageCache::load(project_dir, None);
    let public = compute_column_lineage(&manifest, "stg_orders", DlinDialect::Generic, &mut cache);
    cache.save();

    let cache_path = project_dir
        .join(CACHE_DIR)
        .join(COLUMN_LINEAGE_CACHE_FILENAME);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cache_path).unwrap()).unwrap();
    let sources = json["entries"]["model.proj.stg_orders"]["lineage"]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|entry| entry["sources"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert!(!sources.is_empty());
    assert!(sources[0]["relation"].is_object());
    assert!(sources[0].get("table").is_none());

    let reloaded = ColumnLineageCache::load(project_dir, None);
    let hit = reloaded
        .get(
            "model.proj.stg_orders",
            DlinDialect::Generic,
            super::super::schema::compute_semantic_digest(
                &manifest,
                super::super::find_model_by_name(&manifest, "stg_orders").unwrap(),
            ),
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(hit.clone().into_public()).unwrap(),
        serde_json::to_value(public).unwrap()
    );
}

#[test]
fn test_compute_column_lineage_reuses_canonical_cache_entry_for_short_name() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let manifest = make_test_manifest();
    let node = super::super::find_model_by_name(&manifest, "stg_orders").unwrap();
    let unique_id = node.unique_id.clone();
    let semantic_digest = super::super::schema::compute_semantic_digest(&manifest, node);
    let sentinel = ModelColumnLineage {
        model: "canonical-cache-sentinel".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };

    // Seed the persistent cache using the canonical key, then resolve the
    // same model through both supported caller spellings. The sentinel makes
    // this assert a cache hit rather than merely equal recomputation output.
    let mut seeded = ColumnLineageCache::load(project_dir, None);
    seeded.insert(
        &unique_id,
        DlinDialect::Generic,
        semantic_digest,
        sentinel.clone().into(),
    );
    seeded.save();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let short_result =
        compute_column_lineage(&manifest, "stg_orders", DlinDialect::Generic, &mut cache);
    let unique_result =
        compute_column_lineage(&manifest, &unique_id, DlinDialect::Generic, &mut cache);

    assert_eq!(short_result.model, sentinel.model);
    assert_eq!(unique_result.model, sentinel.model);

    let cache_path = project_dir
        .join(CACHE_DIR)
        .join(COLUMN_LINEAGE_CACHE_FILENAME);
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(cache_path).unwrap()).unwrap();
    let entries = json["entries"].as_object().unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries.contains_key(&unique_id));
    assert!(!entries.contains_key("stg_orders"));
}

#[test]
fn test_column_cache_miss_on_semantic_digest_change() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::Generic, 0, lineage.into());
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(cache2.get("m", DlinDialect::Generic, 1).is_none());
}

#[test]
fn test_column_cache_miss_on_dialect_change() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::BigQuery, 0, lineage.into());
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(cache2.get("m", DlinDialect::Snowflake, 0).is_none());
}

#[test]
fn test_column_cache_miss_on_manifest_columns_change() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::Generic, 42, lineage.into());
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    // Same hash → hit
    assert!(cache2.get("m", DlinDialect::Generic, 42).is_some());
    // Different hash → miss (YAML columns changed in manifest)
    assert!(cache2.get("m", DlinDialect::Generic, 99).is_none());
}

#[test]
fn test_column_cache_version_invalidation() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::Generic, 0, lineage.into());
    cache.save();

    // Tamper with version in saved file
    let cache_path = project_dir
        .join(CACHE_DIR)
        .join(COLUMN_LINEAGE_CACHE_FILENAME);
    let content = std::fs::read_to_string(&cache_path).unwrap();
    let mut cf: ColumnLineageCacheFile = serde_json::from_str(&content).unwrap();
    assert_eq!(cf.version, env!("CARGO_PKG_VERSION"));
    cf.version = "0.0.0-fake".to_string();
    std::fs::write(&cache_path, serde_json::to_string(&cf).unwrap()).unwrap();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(cache2.get("m", DlinDialect::Generic, 0).is_none());
}

#[test]
fn test_column_cache_disabled() {
    let mut cache = ColumnLineageCache::disabled();
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::Generic, 0, lineage.into());
    // Disabled cache still works in-memory (only disk persistence is disabled)
    assert!(cache.get("m", DlinDialect::Generic, 0).is_some());
    // But save is a no-op (no cache_path)
    cache.save();
}

#[test]
fn test_column_cache_fresh() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    // Populate cache
    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::Generic, 0, lineage.into());
    cache.save();

    // Fresh cache ignores existing entries
    let fresh = ColumnLineageCache::fresh(project_dir, None);
    assert!(fresh.get("m", DlinDialect::Generic, 0).is_none());

    // But can save new entries
    let mut fresh = ColumnLineageCache::fresh(project_dir, None);
    let lineage2 = ModelColumnLineage {
        model: "m2".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    fresh.insert("m2", DlinDialect::Generic, 0, lineage2.into());
    fresh.save();

    let reloaded = ColumnLineageCache::load(project_dir, None);
    assert!(reloaded.get("m2", DlinDialect::Generic, 0).is_some());
}

#[test]
fn test_column_cache_ignores_manifest_stat_change_when_semantics_match() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let manifest_path = project_dir.join("manifest.json");
    std::fs::write(&manifest_path, r#"{"nodes":{}}"#).unwrap();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let lineage = ModelColumnLineage {
        model: "m".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    cache.insert("m", DlinDialect::Generic, 42, lineage.into());
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(cache2.get("m", DlinDialect::Generic, 42).is_some());

    std::fs::write(&manifest_path, r#"{"nodes":{"x":1}}"#).unwrap();

    let cache3 = ColumnLineageCache::load(project_dir, None);
    assert!(cache3.get("m", DlinDialect::Generic, 42).is_some());
}

#[test]
fn test_compute_column_lineage_reuses_result_when_manifest_stat_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let manifest_path = project_dir.join("manifest.json");
    std::fs::write(&manifest_path, r#"{"nodes":{}}"#).unwrap();

    let manifest = make_test_manifest();
    let model_name = "stg_orders";
    let node = super::super::find_model_by_name(&manifest, model_name).unwrap();
    let unique_id = node.unique_id.clone();
    let semantic_digest = super::super::schema::compute_semantic_digest(&manifest, node);

    let sentinel = ModelColumnLineage {
        model: "manifest-stat-cache-sentinel".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };

    let mut seeded = ColumnLineageCache::load(project_dir, None);
    seeded.insert(
        &unique_id,
        DlinDialect::Generic,
        semantic_digest,
        sentinel.into(),
    );
    seeded.save();

    let mut before_change = ColumnLineageCache::load(project_dir, None);
    let cached = compute_column_lineage(
        &manifest,
        model_name,
        DlinDialect::Generic,
        &mut before_change,
    );
    assert_eq!(cached.model, "manifest-stat-cache-sentinel");

    std::fs::write(&manifest_path, r#"{"nodes":{"changed":1}}"#).unwrap();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let result = compute_column_lineage(&manifest, model_name, DlinDialect::Generic, &mut cache);

    assert_eq!(result.model, "manifest-stat-cache-sentinel");
}

#[test]
fn test_compute_column_lineage_recomputes_when_upstream_alias_changes() {
    let manifest = make_test_manifest();
    let downstream = super::super::find_model_by_name(&manifest, "orders").unwrap();
    let unique_id = downstream.unique_id.clone();
    let initial_hash = super::super::schema::compute_semantic_digest(&manifest, downstream);

    let sentinel = ModelColumnLineage {
        model: "upstream-alias-cache-sentinel".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    let mut cache = ColumnLineageCache::disabled();
    cache.insert(
        &unique_id,
        DlinDialect::Generic,
        initial_hash,
        sentinel.into(),
    );

    let cached = compute_column_lineage(&manifest, "orders", DlinDialect::Generic, &mut cache);
    assert_eq!(cached.model, "upstream-alias-cache-sentinel");

    let mut changed_manifest = manifest;
    changed_manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .alias = Some("stg_orders_alias".to_string());
    let changed_downstream = super::super::find_model_by_name(&changed_manifest, "orders").unwrap();
    let changed_hash =
        super::super::schema::compute_semantic_digest(&changed_manifest, changed_downstream);

    assert_ne!(initial_hash, changed_hash);
    let result = compute_column_lineage(
        &changed_manifest,
        "orders",
        DlinDialect::Generic,
        &mut cache,
    );
    assert!(
        result.total_columns > 0,
        "alias change should invalidate the cache"
    );
}

#[test]
fn test_semantic_digest_changes_when_upstream_sql_changes() {
    let manifest = make_test_manifest();
    let downstream = super::super::find_model_by_name(&manifest, "orders").unwrap();
    let initial_digest = super::super::schema::compute_semantic_digest(&manifest, downstream);

    let mut changed = manifest;
    changed
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("select changed_order_id from raw.orders".to_string());
    let changed_downstream = super::super::find_model_by_name(&changed, "orders").unwrap();
    let changed_digest =
        super::super::schema::compute_semantic_digest(&changed, changed_downstream);

    assert_ne!(initial_digest, changed_digest);
}

#[test]
fn test_semantic_digest_ignores_unrelated_manifest_changes() {
    let manifest = make_test_manifest();
    let downstream = super::super::find_model_by_name(&manifest, "orders").unwrap();
    let initial_digest = super::super::schema::compute_semantic_digest(&manifest, downstream);

    let mut changed = manifest;
    changed.exposures.insert(
        "exposure.proj.unrelated".to_string(),
        crate::parser::manifest::ManifestExposure {
            unique_id: "exposure.proj.unrelated".to_string(),
            name: "unrelated".to_string(),
            depends_on: Default::default(),
            description: None,
            label: None,
            exposure_type: None,
            url: None,
            maturity: None,
            owner: None,
        },
    );
    let changed_downstream = super::super::find_model_by_name(&changed, "orders").unwrap();
    let changed_digest =
        super::super::schema::compute_semantic_digest(&changed, changed_downstream);

    assert_eq!(initial_digest, changed_digest);
}

#[test]
fn test_unrelated_manifest_change_reuses_cached_lineage() {
    let tmp = tempfile::tempdir().unwrap();
    let manifest = make_test_manifest();
    let node = super::super::find_model_by_name(&manifest, "orders").unwrap();
    let unique_id = node.unique_id.clone();
    let digest = super::super::schema::compute_semantic_digest(&manifest, node);
    let sentinel = ModelColumnLineage {
        model: "unrelated-change-sentinel".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };

    let mut seeded = ColumnLineageCache::load(tmp.path(), None);
    seeded.insert(
        &unique_id,
        DlinDialect::Generic,
        digest,
        sentinel.clone().into(),
    );
    seeded.save();

    let mut changed = manifest;
    changed.exposures.insert(
        "exposure.proj.unrelated".to_string(),
        crate::parser::manifest::ManifestExposure {
            unique_id: "exposure.proj.unrelated".to_string(),
            name: "unrelated".to_string(),
            depends_on: Default::default(),
            description: None,
            label: None,
            exposure_type: None,
            url: None,
            maturity: None,
            owner: None,
        },
    );
    let mut cache = ColumnLineageCache::load(tmp.path(), None);
    let actual = ColumnLineageAnalysis::new(&changed, DlinDialect::Generic, &mut cache)
        .compute_column_lineage("orders");

    assert_eq!(actual.model, sentinel.model);
}

#[test]
fn test_semantic_digest_memoizes_shared_upstream_and_terminates_cycles() {
    let manifest = make_test_manifest();
    let orders = manifest.nodes.get("model.proj.orders").unwrap();
    let stg_orders = manifest.nodes.get("model.proj.stg_orders").unwrap();
    let mut digests = super::super::schema::SemanticDigestCache::default();

    let orders_digest = digests.digest_for_node(&manifest, orders);
    assert!(orders_digest.persistent_cache_safe);
    let computed_after_orders = digests.computed_nodes();
    let stg_orders_digest = digests.digest_for_node(&manifest, stg_orders);
    assert!(stg_orders_digest.persistent_cache_safe);
    assert_eq!(computed_after_orders, digests.computed_nodes());

    let mut cyclic_manifest = make_test_manifest();
    cyclic_manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .depends_on
        .nodes
        .push("model.proj.orders".to_string());
    let cyclic_orders = cyclic_manifest.nodes.get("model.proj.orders").unwrap();
    let cyclic_stg = cyclic_manifest.nodes.get("model.proj.stg_orders").unwrap();
    let mut orders_first = super::super::schema::SemanticDigestCache::default();
    let orders_first_orders = orders_first.digest_for_node(&cyclic_manifest, cyclic_orders);
    let orders_first_stg = orders_first.digest_for_node(&cyclic_manifest, cyclic_stg);
    let mut stg_first = super::super::schema::SemanticDigestCache::default();
    let stg_first_stg = stg_first.digest_for_node(&cyclic_manifest, cyclic_stg);
    let stg_first_orders = stg_first.digest_for_node(&cyclic_manifest, cyclic_orders);

    assert_ne!(orders_first_orders.value, 0);
    assert!(!orders_first_orders.persistent_cache_safe);
    assert!(!orders_first_stg.persistent_cache_safe);
    assert_eq!(orders_first_orders, stg_first_orders);
    assert_eq!(orders_first_stg, stg_first_stg);
}

#[test]
fn test_cycle_digest_is_not_used_for_persistent_cache_reuse() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cyclic_manifest = make_test_manifest();
    cyclic_manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .depends_on
        .nodes
        .push("model.proj.orders".to_string());

    let orders = super::super::find_model_by_name(&cyclic_manifest, "orders").unwrap();
    let orders_id = orders.unique_id.clone();
    let digest = super::super::schema::SemanticDigestCache::default()
        .digest_for_node(&cyclic_manifest, orders);
    assert!(!digest.persistent_cache_safe);

    let sentinel = ModelColumnLineage {
        model: "cycle-cache-sentinel".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    let mut seeded = ColumnLineageCache::load(tmp.path(), None);
    seeded.insert(
        &orders_id,
        DlinDialect::Generic,
        digest.value,
        sentinel.clone().into(),
    );
    seeded.save();

    // The old digest remains the same even though one cycle participant
    // changes, so this specifically exercises the persistent-cache safety
    // flag rather than merely relying on digest invalidation.
    let mut changed_manifest = cyclic_manifest;
    changed_manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("select changed_order_id from raw.orders".to_string());
    let changed_orders = super::super::find_model_by_name(&changed_manifest, "orders").unwrap();
    let changed_digest = super::super::schema::SemanticDigestCache::default()
        .digest_for_node(&changed_manifest, changed_orders);
    assert_eq!(digest.value, changed_digest.value);
    assert!(!changed_digest.persistent_cache_safe);

    let mut cache = ColumnLineageCache::load(tmp.path(), None);
    let result = compute_column_lineage(
        &changed_manifest,
        "orders",
        DlinDialect::Generic,
        &mut cache,
    );
    assert_ne!(result.model, sentinel.model);

    // Unsafe cycle results are also not written back to persistent storage.
    cache.save();
    let persisted = ColumnLineageCache::load(tmp.path(), None);
    assert_eq!(
        persisted
            .get(&orders_id, DlinDialect::Generic, digest.value)
            .unwrap()
            .model,
        sentinel.model
    );

    // A node outside the cycle is unsafe as well because its transitive
    // dependency digest is unsafe.
    let mut external_manifest = changed_manifest;
    let external = crate::parser::manifest::ManifestNode {
        unique_id: "model.proj.report".to_string(),
        name: "report".to_string(),
        alias: None,
        resource_type: "model".to_string(),
        depends_on: crate::parser::manifest::DependsOn {
            nodes: vec![orders_id],
        },
        config: crate::parser::manifest::ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: Default::default(),
        compiled_code: Some("select * from orders".to_string()),
        database: None,
        schema: None,
    };
    external_manifest
        .nodes
        .insert(external.unique_id.clone(), external);
    let external = external_manifest.nodes.get("model.proj.report").unwrap();
    let external_digest = super::super::schema::SemanticDigestCache::default()
        .digest_for_node(&external_manifest, external);
    assert!(!external_digest.persistent_cache_safe);
}

/// Declared columns in declaration order, each with its raw `quote` setting.
type ColumnSpec<'a> = &'a [(&'a str, Option<bool>)];

fn declared(columns: ColumnSpec<'_>) -> indexmap::IndexMap<String, ManifestColumn> {
    columns
        .iter()
        .map(|(name, quote)| {
            (
                (*name).to_string(),
                ManifestColumn {
                    name: (*name).to_string(),
                    quote: *quote,
                },
            )
        })
        .collect()
}

/// A model `m` reading `SELECT * FROM src` over a source `src`, and a model
/// `d` reading `m`. Columns are set after decoding because a JSON object does
/// not carry a reliable declaration order in these tests. `contract` is the
/// `enforced` setting: `None` leaves the contract entry out.
fn schema_inputs_manifest(
    source_columns: ColumnSpec<'_>,
    model_columns: ColumnSpec<'_>,
    contract: Option<bool>,
) -> Manifest {
    let mut manifest: Manifest = serde_json::from_value(serde_json::json!({
        "nodes": {
            "model.proj.m": {
                "unique_id": "model.proj.m",
                "name": "m",
                "resource_type": "model",
                "depends_on": {"nodes": ["source.proj.raw.src"]},
                "compiled_code": "SELECT * FROM src",
                "config": {
                    "contract": contract.map(|enforced| serde_json::json!({"enforced": enforced}))
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
            "source.proj.raw.src": {
                "unique_id": "source.proj.raw.src",
                "name": "src",
                "source_name": "raw",
                "resource_type": "source"
            }
        }
    }))
    .unwrap();
    manifest.nodes.get_mut("model.proj.m").unwrap().columns = declared(model_columns);
    manifest
        .sources
        .get_mut("source.proj.raw.src")
        .unwrap()
        .columns = declared(source_columns);
    manifest
}

fn digests_of(manifest: &Manifest) -> (u64, u64) {
    let digest = |name: &str| {
        let node = super::super::find_model_by_name(manifest, name).unwrap();
        super::super::schema::compute_semantic_digest(manifest, node)
    };
    (digest("m"), digest("d"))
}

fn assert_digests_differ(before: &Manifest, after: &Manifest) {
    let (model_before, downstream_before) = digests_of(before);
    let (model_after, downstream_after) = digests_of(after);
    assert_ne!(model_before, model_after, "reading model digest");
    assert_ne!(
        downstream_before, downstream_after,
        "downstream model digest"
    );
}

const PLAIN: ColumnSpec<'static> = &[("z", None), ("a", None)];

#[test]
fn test_semantic_digest_changes_when_source_column_quote_changes() {
    let with_quote = |quote| schema_inputs_manifest(&[("z", quote), ("a", None)], &[], None);
    let unset = with_quote(None);
    let explicit_false = with_quote(Some(false));
    let explicit_true = with_quote(Some(true));

    assert_digests_differ(&unset, &explicit_true);
    assert_digests_differ(&explicit_false, &explicit_true);
    // Unset and an explicit `false` are different raw declarations.
    assert_digests_differ(&unset, &explicit_false);
}

#[test]
fn test_semantic_digest_changes_when_model_column_quote_changes() {
    let with_quote = |quote| schema_inputs_manifest(PLAIN, &[("a", quote)], None);
    let unset = with_quote(None);
    let explicit_false = with_quote(Some(false));
    let explicit_true = with_quote(Some(true));

    assert_digests_differ(&unset, &explicit_true);
    assert_digests_differ(&explicit_false, &explicit_true);
    assert_digests_differ(&unset, &explicit_false);
}

#[test]
fn test_semantic_digest_changes_when_yaml_column_order_changes() {
    assert_digests_differ(
        &schema_inputs_manifest(&[("z", None), ("a", None)], &[], None),
        &schema_inputs_manifest(&[("a", None), ("z", None)], &[], None),
    );
    assert_digests_differ(
        &schema_inputs_manifest(PLAIN, &[("z", None), ("a", None)], None),
        &schema_inputs_manifest(PLAIN, &[("a", None), ("z", None)], None),
    );
}

#[test]
fn test_semantic_digest_changes_when_contract_enforcement_changes() {
    let with_contract = |enforced| schema_inputs_manifest(PLAIN, &[("a", None)], enforced);
    let absent = with_contract(None);
    let not_enforced = with_contract(Some(false));
    let enforced = with_contract(Some(true));

    assert_digests_differ(&absent, &not_enforced);
    assert_digests_differ(&not_enforced, &enforced);
    assert_digests_differ(&absent, &enforced);
}

#[test]
fn test_semantic_digest_is_stable_for_identical_schema_inputs() {
    let build = || schema_inputs_manifest(PLAIN, &[("a", Some(true))], Some(true));
    assert_eq!(digests_of(&build()), digests_of(&build()));
}

fn cache_file_path(project_dir: &std::path::Path) -> std::path::PathBuf {
    project_dir
        .join(CACHE_DIR)
        .join(COLUMN_LINEAGE_CACHE_FILENAME)
}

fn seeded_cache_file(project_dir: &std::path::Path) -> ColumnLineageCacheFile {
    let mut cache = ColumnLineageCache::load(project_dir, None);
    cache.insert(
        "m",
        DlinDialect::Generic,
        0,
        ModelColumnLineage {
            model: "m".to_string(),
            traced_columns: 0,
            total_columns: 0,
            columns: vec![],
            errors: vec![],
        }
        .into(),
    );
    cache.save();
    serde_json::from_str(&std::fs::read_to_string(cache_file_path(project_dir)).unwrap()).unwrap()
}

#[test]
fn test_column_cache_stores_and_uses_matching_semantics_id() {
    let tmp = tempfile::tempdir().unwrap();
    let file = seeded_cache_file(tmp.path());
    assert_eq!(file.semantics_id, ANALYSIS_SEMANTICS_ID);

    let reloaded = ColumnLineageCache::load(tmp.path(), None);
    assert!(reloaded.get("m", DlinDialect::Generic, 0).is_some());
}

#[test]
fn test_column_cache_ignores_file_with_different_semantics_id() {
    let tmp = tempfile::tempdir().unwrap();
    let mut file = seeded_cache_file(tmp.path());
    file.semantics_id = format!("{ANALYSIS_SEMANTICS_ID}-other");
    std::fs::write(
        cache_file_path(tmp.path()),
        serde_json::to_string(&file).unwrap(),
    )
    .unwrap();

    let reloaded = ColumnLineageCache::load(tmp.path(), None);
    assert!(reloaded.get("m", DlinDialect::Generic, 0).is_none());
}

#[test]
fn test_column_cache_ignores_file_without_semantics_id() {
    let tmp = tempfile::tempdir().unwrap();
    seeded_cache_file(tmp.path());
    let path = cache_file_path(tmp.path());
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json.as_object_mut().unwrap().remove("semantics_id");
    std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();

    let reloaded = ColumnLineageCache::load(tmp.path(), None);
    assert!(reloaded.get("m", DlinDialect::Generic, 0).is_none());
}

/// Edits to the source YAML that dlin makes through the manifest. dlin cannot
/// read catalog.json yet, so catalog evidence replacing YAML evidence (the
/// engine contract cases E4 and E5) is modelled through the only input dlin
/// does read: the declared columns of the source.
fn e1_manifest(source_columns: &[&str]) -> Manifest {
    let columns: Vec<_> = source_columns.iter().map(|name| (*name, None)).collect();
    schema_inputs_manifest(&columns, &[], None)
}

fn pairs_by_column(result: &ModelColumnLineage) -> Vec<(String, Vec<(String, String)>)> {
    let mut pairs: Vec<_> = result
        .columns
        .iter()
        .map(|entry| {
            let mut sources: Vec<_> = entry
                .sources
                .iter()
                .map(|source| (source.table.clone(), source.column.clone()))
                .collect();
            sources.sort();
            (entry.column.clone(), sources)
        })
        .collect();
    pairs.sort();
    pairs
}

fn compute_persisted(
    project_dir: &std::path::Path,
    manifest: &Manifest,
) -> (ModelColumnLineage, u64) {
    let mut cache = ColumnLineageCache::load(project_dir, None);
    let result = compute_column_lineage(manifest, "m", DlinDialect::PostgreSQL, &mut cache);
    cache.save();
    (result, digests_of(manifest).0)
}

#[test]
fn test_persistent_cache_does_not_reuse_results_when_yaml_evidence_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();

    // E1: the YAML list `[z, a]` is the assumed exhaustive schema.
    let e1 = e1_manifest(&["z", "a"]);
    let (e1_result, e1_digest) = compute_persisted(project_dir, &e1);
    assert!(
        e1_result.errors.is_empty(),
        "errors: {:?}",
        e1_result.errors
    );
    let e1_pairs = pairs_by_column(&e1_result);
    assert_eq!(
        e1_pairs.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(),
        ["a", "z"]
    );

    // E4: the same columns with the order refined to `[a, z]`.
    let e4 = e1_manifest(&["a", "z"]);
    let (e4_result, e4_digest) = compute_persisted(project_dir, &e4);
    assert_ne!(e1_digest, e4_digest);
    assert!(
        e4_result.errors.is_empty(),
        "errors: {:?}",
        e4_result.errors
    );
    let e4_pairs = pairs_by_column(&e4_result);
    assert_eq!(e4_pairs.len(), 2, "both columns are still traced");
    // The old entry was replaced, not hit: only the new digest is stored.
    let after_e4 = ColumnLineageCache::load(project_dir, None);
    assert!(
        after_e4
            .get("model.proj.m", DlinDialect::PostgreSQL, e1_digest)
            .is_none()
    );
    assert!(
        after_e4
            .get("model.proj.m", DlinDialect::PostgreSQL, e4_digest)
            .is_some()
    );

    // Everything known before stays known after the refinement.
    for (column, sources) in &e1_pairs {
        let refined = e4_pairs
            .iter()
            .find(|(name, _)| name == column)
            .unwrap_or_else(|| panic!("column {column} was lost by the refinement"));
        for source in sources {
            assert!(
                refined.1.contains(source),
                "source {source:?} of {column} was lost by the refinement"
            );
        }
    }

    // E5: the assumption that `[z, a]` is exhaustive is replaced.
    let e5 = e1_manifest(&["a", "z", "extra"]);
    let (e5_result, e5_digest) = compute_persisted(project_dir, &e5);
    assert_ne!(e4_digest, e5_digest);
    assert!(
        e5_result.errors.is_empty(),
        "errors: {:?}",
        e5_result.errors
    );
    let e5_pairs = pairs_by_column(&e5_result);
    assert!(
        e5_pairs.iter().any(|(column, sources)| column == "extra"
            && sources == &[("src".to_string(), "extra".to_string())]),
        "SELECT * must include the new column: {e5_pairs:?}"
    );
}
