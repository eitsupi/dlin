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
        DialectType::Generic,
        0,
        None,
        lineage,
    );
    cache.save();

    // Reload from disk
    let cache2 = ColumnLineageCache::load(project_dir, None);
    let hit = cache2
        .get(
            "test_model",
            "SELECT id FROM raw",
            DialectType::Generic,
            None,
            Some(0),
        )
        .unwrap();
    assert_eq!(hit.columns.len(), 1);
    assert_eq!(hit.columns[0].column, "id");
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
    cache.insert("m", "SELECT 1", DialectType::Generic, 0, None, lineage);
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get("m", "SELECT 2", DialectType::Generic, None, Some(0))
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
    cache.insert("m", "SELECT 1", DialectType::BigQuery, 0, None, lineage);
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get("m", "SELECT 1", DialectType::Snowflake, None, Some(0))
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
    cache.insert("m", "SELECT 1", DialectType::Generic, 42, None, lineage);
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    // Same hash → hit
    assert!(
        cache2
            .get("m", "SELECT 1", DialectType::Generic, None, Some(42))
            .is_some()
    );
    // Different hash → miss (YAML columns changed in manifest)
    assert!(
        cache2
            .get("m", "SELECT 1", DialectType::Generic, None, Some(99))
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
    cache.insert("m", "SELECT 1", DialectType::Generic, 0, None, lineage);
    cache.save();

    // Tamper with version in saved file
    let cache_path = project_dir
        .join(CACHE_DIR)
        .join(COLUMN_LINEAGE_CACHE_FILENAME);
    let content = std::fs::read_to_string(&cache_path).unwrap();
    let mut cf: ColumnLineageCacheFile = serde_json::from_str(&content).unwrap();
    cf.version = "0.0.0-fake".to_string();
    std::fs::write(&cache_path, serde_json::to_string(&cf).unwrap()).unwrap();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get("m", "SELECT 1", DialectType::Generic, None, Some(0))
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
    cache.insert("m", "SELECT 1", DialectType::Generic, 0, None, lineage);
    // Disabled cache still works in-memory (only disk persistence is disabled)
    assert!(
        cache
            .get("m", "SELECT 1", DialectType::Generic, None, Some(0))
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
    cache.insert("m", "SELECT 1", DialectType::Generic, 0, None, lineage);
    cache.save();

    // Fresh cache ignores existing entries
    let fresh = ColumnLineageCache::fresh(project_dir, None);
    assert!(
        fresh
            .get("m", "SELECT 1", DialectType::Generic, None, Some(0))
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
    fresh.insert("m2", "SELECT 2", DialectType::Generic, 0, None, lineage2);
    fresh.save();

    let reloaded = ColumnLineageCache::load(project_dir, None);
    assert!(
        reloaded
            .get("m2", "SELECT 2", DialectType::Generic, None, Some(0))
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
        DialectType::Generic,
        42,
        Some(&manifest_path),
        lineage,
    );
    cache.save();

    let cache2 = ColumnLineageCache::load(project_dir, None);
    assert!(
        cache2
            .get(
                "m",
                "SELECT 1",
                DialectType::Generic,
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
                DialectType::Generic,
                Some(&manifest_path),
                Some(42)
            )
            .is_none()
    );
}
