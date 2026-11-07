//! YAML-declared schemas through the production path: `compute_column_lineage`
//! over a decoded manifest.

use super::*;

fn manifest(dialect_sql: &str, source_columns: serde_json::Value) -> Manifest {
    serde_json::from_value(serde_json::json!({
        "nodes": {
            "model.proj.m": {
                "unique_id": "model.proj.m",
                "name": "m",
                "resource_type": "model",
                "depends_on": {"nodes": ["source.proj.raw.src"]},
                "compiled_code": dialect_sql,
                "columns": {}
            }
        },
        "sources": {
            "source.proj.raw.src": {
                "unique_id": "source.proj.raw.src",
                "name": "src",
                "source_name": "raw",
                "resource_type": "source",
                "columns": source_columns
            }
        }
    }))
    .unwrap()
}

fn trace(manifest: &Manifest, dialect: DlinDialect) -> ModelColumnLineage {
    compute_column_lineage(manifest, "m", dialect, &mut ColumnLineageCache::disabled())
}

fn pairs(entry: &ColumnLineageEntry) -> Vec<(&str, &str)> {
    entry
        .sources
        .iter()
        .map(|source| (source.table.as_str(), source.column.as_str()))
        .collect()
}

/// A star over a source that only has YAML columns expands to the declared
/// columns, in any order, without needing SQL for the source.
#[test]
fn star_over_yaml_only_source_expands_declared_columns() {
    let manifest = manifest(
        "SELECT * FROM src",
        serde_json::json!({"z": {"name": "z"}, "a": {"name": "a"}}),
    );
    let result = trace(&manifest, DlinDialect::PostgreSQL);

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let mut names: Vec<_> = result
        .columns
        .iter()
        .map(|entry| entry.column.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["a", "z"]);
    for entry in &result.columns {
        assert_eq!(
            pairs(entry),
            [("src", entry.column.as_str())],
            "column {}",
            entry.column
        );
    }
}

/// A source column declared with `quote: true` is traced from a quoted
/// reference.
#[test]
fn quoted_yaml_column_matches_a_quoted_reference_in_snowflake() {
    let manifest = manifest(
        r#"SELECT s."id" FROM src s"#,
        serde_json::json!({"id": {"name": "id", "quote": true}}),
    );
    let result = trace(&manifest, DlinDialect::Snowflake);

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 1);
    assert_eq!(result.columns[0].column, "id");
    assert_eq!(pairs(&result.columns[0]), [("src", "id")]);
}
