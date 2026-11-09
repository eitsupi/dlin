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

fn pairs(entry: &OutputSlot<ColumnSource>) -> Pairs {
    known_pairs_of(entry)
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
    let mut names = result.names();
    names.sort_unstable();
    assert_eq!(names, ["a", "z"]);
    for entry in result.slots() {
        assert_eq!(
            pairs(entry),
            [("src", entry.column().as_str())],
            "column {}",
            entry.column()
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
    assert_eq!(result.slots().len(), 1);
    // The output name keeps the quote of the reference it was inferred from.
    assert_eq!(
        result.slots()[0].name,
        OutputName::Explicit(Ident::quoted("id", Quote::Double))
    );
    assert_eq!(pairs(&result.slots()[0]), [("src", "\"id\"")]);
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
        let mut names = result.names();
        names.sort_unstable();
        assert_eq!(names, ["id", "ts"]);
        for entry in result.slots() {
            assert_eq!(
                pairs(entry),
                [("db.raw.events", entry.column().as_str())],
                "column {}",
                entry.column()
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
        result.slots().iter().all(|entry| entry.column() != "id"),
        "columns: {:?}",
        result.slots()
    );
}

/// A model and a source that resolve to one physical relation, both direct
/// dependencies of `down`, which pairs the relation's star positionally with
/// `other`. The model's SQL lists `b, a`; the source YAML lists `a, b`.
fn model_and_source_on_one_relation(dependency_order: [&str; 2]) -> Manifest {
    let mut dependencies = Vec::from(dependency_order);
    dependencies.push("model.proj.other");
    serde_json::from_value(serde_json::json!({
        "nodes": {
            "model.proj.m_up": {
                "unique_id": "model.proj.m_up",
                "name": "events",
                "resource_type": "model",
                "database": "db",
                "schema": "raw",
                "depends_on": {"nodes": []},
                "compiled_code": "SELECT b, a FROM raw_x",
                "columns": {}
            },
            "model.proj.other": {
                "unique_id": "model.proj.other",
                "name": "other",
                "resource_type": "model",
                "depends_on": {"nodes": []},
                "compiled_code": "SELECT p, q FROM raw_other",
                "columns": {}
            },
            "model.proj.down": {
                "unique_id": "model.proj.down",
                "name": "down",
                "resource_type": "model",
                "depends_on": {"nodes": dependencies},
                "compiled_code":
                    "SELECT * FROM db.raw.events UNION ALL SELECT p, q FROM other",
                "columns": {}
            }
        },
        "sources": {
            "source.proj.raw.events": {
                "unique_id": "source.proj.raw.events",
                "name": "events",
                "source_name": "raw",
                "resource_type": "source",
                "database": "db",
                "schema": "raw",
                "identifier": "events",
                "columns": {"a": {"name": "a"}, "b": {"name": "b"}}
            }
        }
    }))
    .unwrap()
}

/// A model's SQL output and a source's YAML that name the same columns for one
/// physical relation do not contradict each other: the star expands in the
/// proven SQL order, so the positional set operation pairs `b` with `p` and
/// `a` with `q`, whichever dependency is listed first.
#[test]
fn star_over_a_relation_shared_by_a_model_and_a_source_follows_the_sql_order() {
    for dependency_order in [
        ["model.proj.m_up", "source.proj.raw.events"],
        ["source.proj.raw.events", "model.proj.m_up"],
    ] {
        let manifest = model_and_source_on_one_relation(dependency_order);
        let result = compute_column_lineage(
            &manifest,
            "down",
            DlinDialect::PostgreSQL,
            &mut ColumnLineageCache::disabled(),
        );

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let mut columns = result.names();
        columns.sort_unstable();
        assert_eq!(columns, ["a", "b"], "dependencies: {dependency_order:?}");
        for (column, upstream) in [("b", "p"), ("a", "q")] {
            let entry = result
                .slots()
                .iter()
                .find(|e| e.column() == column)
                .unwrap();
            let mut sources: Vec<_> = pairs(entry).0;
            sources.sort_unstable();
            let expected = [
                ("db.raw.events".to_string(), column.to_string()),
                ("other".to_string(), upstream.to_string()),
            ];
            assert_eq!(
                sources, expected,
                "column {column}, dependencies: {dependency_order:?}"
            );
        }
    }
}
