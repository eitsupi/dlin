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
    cache.insert(
        "test_model",
        "SELECT id FROM raw",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        0,
        None,
        lineage.into(),
    );
    cache.save();

    // Reload from disk
    let cache2 = ColumnLineageCache::load(project_dir, None);
    let hit = cache2
        .get(
            "test_model",
            "SELECT id FROM raw",
            DlinDialect::Generic,
            BackendId::Sqllineage,
            None,
            Some(0),
        )
        .unwrap();
    assert_eq!(hit.columns.len(), 1);
    assert_eq!(hit.columns[0].column, "id");
}

#[test]
fn test_column_cache_persists_structural_relation_and_public_output() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let manifest = make_test_manifest();
    let compiled_code = super::super::find_model_by_name(&manifest, "stg_orders")
        .unwrap()
        .compiled_code
        .clone()
        .unwrap();

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
            &compiled_code,
            DlinDialect::Generic,
            BackendId::Sqllineage,
            None,
            Some(super::super::schema::compute_manifest_columns_hash(
                &manifest,
                super::super::find_model_by_name(&manifest, "stg_orders").unwrap(),
            )),
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
    let compiled_code = node.compiled_code.clone().unwrap();
    let manifest_columns_hash =
        super::super::schema::compute_manifest_columns_hash(&manifest, node);
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
        &compiled_code,
        DlinDialect::Generic,
        BackendId::Sqllineage,
        manifest_columns_hash,
        None,
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
fn test_column_cache_miss_on_code_change() {
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        0,
        None,
        lineage.into(),
    );
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get(
                "m",
                "SELECT 2",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(0)
            )
            .is_none()
    );
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::BigQuery,
        BackendId::Sqllineage,
        0,
        None,
        lineage.into(),
    );
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Snowflake,
                BackendId::Sqllineage,
                None,
                Some(0)
            )
            .is_none()
    );
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        42,
        None,
        lineage.into(),
    );
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    // Same hash → hit
    assert!(
        cache2
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(42)
            )
            .is_some()
    );
    // Different hash → miss (YAML columns changed in manifest)
    assert!(
        cache2
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(99)
            )
            .is_none()
    );
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        0,
        None,
        lineage.into(),
    );
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
    assert!(
        cache2
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(0)
            )
            .is_none()
    );
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        0,
        None,
        lineage.into(),
    );
    // Disabled cache still works in-memory (only disk persistence is disabled)
    assert!(
        cache
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(0)
            )
            .is_some()
    );
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        0,
        None,
        lineage.into(),
    );
    cache.save();

    // Fresh cache ignores existing entries
    let fresh = ColumnLineageCache::fresh(project_dir, None);
    assert!(
        fresh
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(0)
            )
            .is_none()
    );

    // But can save new entries
    let mut fresh = ColumnLineageCache::fresh(project_dir, None);
    let lineage2 = ModelColumnLineage {
        model: "m2".to_string(),
        traced_columns: 0,
        total_columns: 0,
        columns: vec![],
        errors: vec![],
    };
    fresh.insert(
        "m2",
        "SELECT 2",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        0,
        None,
        lineage2.into(),
    );
    fresh.save();

    let reloaded = ColumnLineageCache::load(project_dir, None);
    assert!(
        reloaded
            .get(
                "m2",
                "SELECT 2",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                None,
                Some(0)
            )
            .is_some()
    );
}

#[test]
fn test_column_cache_miss_on_manifest_stat_change() {
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
    cache.insert(
        "m",
        "SELECT 1",
        DlinDialect::Generic,
        BackendId::Sqllineage,
        42,
        Some(&manifest_path),
        lineage.into(),
    );
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                Some(&manifest_path),
                Some(42)
            )
            .is_some()
    );

    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&manifest_path, r#"{"nodes":{"x":1}}"#).unwrap();

    let cache3 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache3
            .get(
                "m",
                "SELECT 1",
                DlinDialect::Generic,
                BackendId::Sqllineage,
                Some(&manifest_path),
                Some(42)
            )
            .is_none()
    );
}

#[test]
fn test_compute_column_lineage_recomputes_when_manifest_stat_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path();
    let manifest_path = project_dir.join("manifest.json");
    std::fs::write(&manifest_path, r#"{"nodes":{}}"#).unwrap();

    let manifest = make_test_manifest();
    let model_name = "stg_orders";
    let node = super::super::find_model_by_name(&manifest, model_name).unwrap();
    let unique_id = node.unique_id.clone();
    let compiled_code = node.compiled_code.as_deref().unwrap();
    let manifest_columns_hash =
        super::super::schema::compute_manifest_columns_hash(&manifest, node);

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
        compiled_code,
        DlinDialect::Generic,
        BackendId::Sqllineage,
        manifest_columns_hash,
        Some(&manifest_path),
        sentinel.into(),
    );
    seeded.save();

    let mut before_change = ColumnLineageCache::load(project_dir, None);
    let cached = compute_column_lineage_with_manifest_path(
        &manifest,
        model_name,
        DlinDialect::Generic,
        Some(&manifest_path),
        &mut before_change,
    );
    assert_eq!(cached.model, "manifest-stat-cache-sentinel");

    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(&manifest_path, r#"{"nodes":{"changed":1}}"#).unwrap();

    let mut cache = ColumnLineageCache::load(project_dir, None);
    let result = compute_column_lineage_with_manifest_path(
        &manifest,
        model_name,
        DlinDialect::Generic,
        Some(&manifest_path),
        &mut cache,
    );

    assert!(
        result.total_columns > 0,
        "should recompute, not return sentinel"
    );
}

#[test]
fn test_compute_column_lineage_recomputes_when_upstream_alias_changes() {
    let manifest = make_test_manifest();
    let downstream = super::super::find_model_by_name(&manifest, "orders").unwrap();
    let unique_id = downstream.unique_id.clone();
    let initial_hash = super::super::schema::compute_manifest_columns_hash(&manifest, downstream);
    let compiled_code = downstream.compiled_code.as_deref().unwrap();

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
        compiled_code,
        DlinDialect::Generic,
        BackendId::Sqllineage,
        initial_hash,
        None,
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
        super::super::schema::compute_manifest_columns_hash(&changed_manifest, changed_downstream);

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
