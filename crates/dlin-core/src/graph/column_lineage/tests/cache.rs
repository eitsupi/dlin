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

    digests.digest_for_node(&manifest, orders);
    let computed_after_orders = digests.computed_nodes();
    digests.digest_for_node(&manifest, stg_orders);
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

    assert_ne!(orders_first_orders, 0);
    assert_eq!(orders_first_orders, stg_first_orders);
    assert_eq!(orders_first_stg, stg_first_stg);
}
