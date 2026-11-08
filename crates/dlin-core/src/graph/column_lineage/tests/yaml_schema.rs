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

/// Two models-of-record for one physical table: two source definitions with
/// different `source_name` that share database, schema and identifier.
fn shared_table_manifest(first: serde_json::Value, second: serde_json::Value) -> Manifest {
    let source = |source_name: &str, columns: serde_json::Value| {
        let id = format!("source.proj.{source_name}.events");
        (
            id.clone(),
            serde_json::json!({
                "unique_id": id,
                "name": "events",
                "source_name": source_name,
                "resource_type": "source",
                "database": "db",
                "schema": "raw",
                "identifier": "events",
                "columns": columns
            }),
        )
    };
    let sources: serde_json::Map<_, _> = [source("first", first), source("second", second)]
        .into_iter()
        .collect();
    serde_json::from_value(serde_json::json!({
        "nodes": {
            "model.proj.m": {
                "unique_id": "model.proj.m",
                "name": "m",
                "resource_type": "model",
                "depends_on": {
                    "nodes": ["source.proj.first.events", "source.proj.second.events"]
                },
                "compiled_code": "SELECT * FROM db.raw.events",
                "columns": {}
            }
        },
        "sources": sources
    }))
    .unwrap()
}

/// Source definitions that declare the same columns for one physical table
/// (even in a different listing order) do not contradict each other.
#[test]
fn star_over_a_table_shared_by_consistent_source_definitions_expands() {
    for second in [
        serde_json::json!({"id": {"name": "id"}, "ts": {"name": "ts"}}),
        serde_json::json!({"ts": {"name": "ts"}, "id": {"name": "id"}}),
    ] {
        let manifest = shared_table_manifest(
            serde_json::json!({"id": {"name": "id"}, "ts": {"name": "ts"}}),
            second,
        );
        let result = trace(&manifest, DlinDialect::PostgreSQL);

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let mut names: Vec<_> = result
            .columns
            .iter()
            .map(|entry| entry.column.as_str())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["id", "ts"]);
        for entry in &result.columns {
            assert_eq!(
                pairs(entry),
                [("db.raw.events", entry.column.as_str())],
                "column {}",
                entry.column
            );
        }
    }
}

/// Source definitions that disagree about the columns of one physical table
/// leave the star unexpanded.
#[test]
fn star_over_a_table_shared_by_contradicting_source_definitions_is_not_expanded() {
    let manifest = shared_table_manifest(
        serde_json::json!({"id": {"name": "id"}, "ts": {"name": "ts"}}),
        serde_json::json!({"id": {"name": "id"}, "ts": {"name": "ts"}, "extra": {"name": "extra"}}),
    );
    let result = trace(&manifest, DlinDialect::PostgreSQL);

    assert!(
        result.columns.iter().all(|entry| entry.column != "id"),
        "columns: {:?}",
        result.columns
    );
}
