mod common;

use common::find_mapping;
use sqllineage::{AnalyzeOptions, CatalogProvider, ColumnOrigin, Dialect, TableRef, analyze};

struct MockCatalog;

impl CatalogProvider for MockCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        match table.table.as_str() {
            "users" => Some(vec!["id".into(), "name".into(), "email".into()]),
            "orders" => Some(vec!["id".into(), "user_id".into(), "amount".into()]),
            _ => None,
        }
    }

    fn resolve_column(&self, column: &str, candidates: &[TableRef]) -> Option<TableRef> {
        match column {
            "name" | "email" => candidates.iter().find(|t| t.table == "users").cloned(),
            "amount" => candidates.iter().find(|t| t.table == "orders").cloned(),
            _ => None,
        }
    }
}

struct EagerCatalog;

impl CatalogProvider for EagerCatalog {
    fn list_columns(&self, _table: &TableRef) -> Option<Vec<String>> {
        None
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        Some(TableRef::new("fabricated"))
    }
}

struct AliasCatalog;

impl CatalogProvider for AliasCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        (table.table == "actual_table").then(|| vec!["id".into(), "event".into()])
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        None
    }
}

struct WildcardCatalog;

impl CatalogProvider for WildcardCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        match table.table.as_str() {
            "users" => Some(vec!["id".into(), "name".into(), "secret".into()]),
            "other" => Some(vec!["a".into(), "b".into()]),
            _ => None,
        }
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        None
    }
}

fn opts_with_catalog() -> AnalyzeOptions {
    AnalyzeOptions {
        catalog: Some(Box::new(MockCatalog)),
        ..AnalyzeOptions::default()
    }
}

fn concrete_sources(mapping: &sqllineage::ColumnMapping) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = mapping
        .sources
        .iter()
        .map(|s| match s {
            ColumnOrigin::Concrete { table, column } => (table.table.clone(), column.clone()),
            other => panic!("expected Concrete, got {other:?}"),
        })
        .collect();
    v.sort();
    v
}

#[test]
fn select_star_with_catalog_expands() {
    let result = analyze("SELECT * FROM users", opts_with_catalog())
        .expect("parse")
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.columns.mappings.len(), 3);

    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "id")),
        vec![("users".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "name")),
        vec![("users".into(), "name".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "email")),
        vec![("users".into(), "email".into())]
    );
}

#[test]
fn wildcard_except_removes_catalog_column() {
    let result = analyze(
        "SELECT * EXCEPT (secret) FROM users",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    let names = result
        .columns
        .mappings
        .iter()
        .map(|mapping| mapping.target.column.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["id", "name"]);
}

#[test]
fn qualified_wildcard_except_uses_relation_binding() {
    let result = analyze(
        "SELECT u.* EXCEPT (secret) FROM users AS u",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(
        result
            .columns
            .mappings
            .iter()
            .map(|mapping| mapping.target.column.as_str())
            .collect::<Vec<_>>(),
        ["id", "name"]
    );
}

#[test]
fn wildcard_ilike_filters_known_columns() {
    let result = analyze(
        "SELECT * ILIKE '%na%' FROM users",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(
        result
            .columns
            .mappings
            .iter()
            .map(|mapping| mapping.target.column.as_str())
            .collect::<Vec<_>>(),
        ["name"]
    );
}

#[test]
fn unknown_cte_star_does_not_recompose_excluded_names() {
    let result = analyze(
        "WITH x AS (SELECT * FROM unknown_source) SELECT * EXCEPT (missing) FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(
        !result
            .columns
            .mappings
            .iter()
            .any(|mapping| mapping.target.column == "missing")
    );
}

#[test]
fn unknown_cte_excluded_name_is_not_named_wildcard_fallback() {
    let result = analyze(
        "WITH x AS (SELECT * EXCEPT (secret) FROM unknown_source) SELECT secret FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Ambiguous { candidates, .. }] if candidates.is_empty()
    ));
}

#[test]
fn unknown_cte_allowed_name_keeps_named_wildcard_fallback() {
    let result = analyze(
        "WITH x AS (SELECT * EXCEPT (secret) FROM unknown_source) SELECT id FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::NamedWildcard { column, .. }] if column == "id"
    ));
}

#[test]
fn unknown_cte_rename_maps_new_name_without_restoring_old_name() {
    let result = analyze(
        "WITH x AS (SELECT * RENAME (id AS user_id) FROM unknown_source) \
         SELECT user_id, id FROM x",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::NamedWildcard { column, .. }] if column == "id"
    ));
    assert!(matches!(
        result.columns.mappings[1].sources.as_slice(),
        [ColumnOrigin::Ambiguous { candidates, .. }] if candidates.is_empty()
    ));
}

#[test]
fn unknown_cte_replace_keeps_replacement_lineage() {
    let result = analyze(
        "WITH x AS (SELECT * REPLACE (other AS id) FROM unknown_source) \
         SELECT id FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Concrete { column, .. }] if column == "other"
    ));
}

#[test]
fn wildcard_replace_uses_replacement_expression_lineage() {
    let result = analyze(
        "SELECT * REPLACE (name AS id) FROM users",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    let id = find_mapping(&result.columns.mappings, "id");
    assert_eq!(concrete_sources(id), vec![("users".into(), "name".into())]);
}

#[test]
fn wildcard_rename_preserves_position() {
    let result = analyze(
        "SELECT * EXCLUDE (secret) RENAME (id AS user_id) FROM users",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings[0].target.column, "user_id");
    assert_eq!(result.columns.mappings[1].target.column, "name");
}

#[test]
fn qualified_exclude_uses_qualified_relation_not_suffix_matching() {
    let result = analyze(
        "SELECT u.* EXCLUDE (u.secret) FROM users AS u",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(
        result
            .columns
            .mappings
            .iter()
            .map(|mapping| mapping.target.column.as_str())
            .collect::<Vec<_>>(),
        ["id", "name"]
    );
}

#[test]
fn qualified_exclude_width_is_used_for_set_operation_arity() {
    let result = analyze(
        "SELECT u.* EXCLUDE (u.secret) FROM users AS u \
         UNION ALL SELECT a, b FROM other",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 2);
}

#[test]
fn qualified_exclude_requires_exact_catalog_qualification() {
    let result = analyze(
        "SELECT u.* EXCLUDE (cat2.sch.users.secret) FROM cat1.sch.users AS u",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            catalog: Some(Box::new(WildcardCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(
        result
            .columns
            .mappings
            .iter()
            .any(|mapping| mapping.target.column == "secret")
    );
}

#[test]
fn qualified_field_path_star_does_not_forge_table_ref() {
    let result = analyze("SELECT base.event.* FROM base", AnalyzeOptions::default())
        .expect("parse")
        .into_iter()
        .next()
        .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(
        !result
            .tables
            .inputs
            .iter()
            .any(|table| table.table == "event")
    );
    assert!(!result.columns.mappings.iter().any(|mapping| {
        mapping.sources.iter().any(
            |source| matches!(source, ColumnOrigin::Wildcard { table } if table.table == "event"),
        )
    }));
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [
            ColumnOrigin::Concrete { table, column },
            ColumnOrigin::Ambiguous { column: marker, .. }
        ] if table.table == "base" && column == "event" && marker == "*"
    ));
}

#[test]
fn cte_field_path_star_keeps_upstream_field_ancestry() {
    let result = analyze(
        "WITH base AS (SELECT event FROM source) SELECT base.event.* FROM base",
        AnalyzeOptions::default(),
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(result.columns.mappings.iter().any(|mapping| {
        mapping.sources.iter().any(|source| {
            matches!(
                source,
                ColumnOrigin::Concrete { table, column }
                    if table.table == "source" && column == "event"
            )
        })
    }));
    assert!(
        !result
            .tables
            .inputs
            .iter()
            .any(|table| table.table == "event")
    );
}

#[test]
fn derived_field_path_star_keeps_upstream_field_ancestry() {
    let result = analyze(
        "SELECT base.event.* FROM (SELECT event FROM source) AS base",
        AnalyzeOptions::default(),
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(result.columns.mappings.iter().any(|mapping| {
        mapping.sources.iter().any(|source| {
            matches!(
                source,
                ColumnOrigin::Concrete { table, column }
                    if table.table == "source" && column == "event"
            )
        })
    }));
    assert!(
        !result
            .tables
            .inputs
            .iter()
            .any(|table| table.table == "event")
    );
}

#[test]
fn unknown_root_replace_keeps_barrier_and_adds_replacement_mapping() {
    let result = analyze(
        "SELECT * REPLACE (other AS id) FROM unknown_source",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(
        result
            .columns
            .mappings
            .iter()
            .any(|mapping| mapping.target.column == "*")
    );
    assert!(result.columns.mappings.iter().any(|mapping| {
        mapping.target.column == "id"
            && matches!(
                mapping.sources.as_slice(),
                [ColumnOrigin::Concrete { column, .. }] if column == "other"
            )
    }));
}

#[test]
fn unknown_root_rename_keeps_barrier_and_adds_named_wildcard() {
    let result = analyze(
        "SELECT * RENAME (id AS user_id) FROM unknown_source",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(result.columns.mappings.iter().any(|mapping| {
        mapping.target.column == "user_id"
            && matches!(
                mapping.sources.as_slice(),
                [ColumnOrigin::NamedWildcard { column, .. }] if column == "id"
            )
    }));
}

#[test]
fn multiple_unknown_stars_keep_name_available_from_unfiltered_star() {
    let result = analyze(
        "WITH x AS (SELECT *, * EXCEPT (secret) FROM unknown_source) SELECT secret FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::NamedWildcard { column, .. }] if column == "secret"
    ));
}

#[test]
fn unknown_qualified_exclude_does_not_restore_excluded_name() {
    let result = analyze(
        "WITH x AS (SELECT u.* EXCLUDE (u.secret) FROM unknown_source AS u) \
         SELECT secret FROM x",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Ambiguous { candidates, .. }] if candidates.is_empty()
    ));
}

#[test]
fn unqualified_qualified_exclude_keeps_other_relation_wildcard() {
    let result = analyze(
        "WITH x AS (SELECT * EXCLUDE (u.secret) FROM unknown_source AS u, other_source AS v) \
         SELECT secret FROM x",
        AnalyzeOptions {
            dialect: Dialect::Snowflake,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::NamedWildcard { table, column }] if table.table == "other_source" && column == "secret"
    ));
}

#[test]
fn set_operation_name_decision_uses_left_branch_only() {
    let result = analyze(
        "WITH x AS (SELECT * FROM unknown_left UNION ALL SELECT * EXCEPT (secret) FROM unknown_right) \
         SELECT secret FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    let sources = &result.columns.mappings[0].sources;
    assert!(sources.iter().any(|source| {
        matches!(source, ColumnOrigin::NamedWildcard { table, column }
            if table.table == "unknown_left" && column == "secret")
    }));
    assert!(sources.iter().any(|source| {
        matches!(source, ColumnOrigin::Ambiguous { column, candidates }
            if column == "secret" && candidates.is_empty())
    }));
}

#[test]
fn set_operation_keeps_named_wildcard_sources_from_both_branches() {
    let result = analyze(
        "WITH x AS (SELECT * FROM unknown_left UNION ALL SELECT * FROM unknown_right) \
         SELECT secret FROM x",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    let sources = &result.columns.mappings[0].sources;
    for table_name in ["unknown_left", "unknown_right"] {
        assert!(sources.iter().any(|source| {
            matches!(source, ColumnOrigin::NamedWildcard { table, column }
                if table.table == table_name && column == "secret")
        }));
    }
    assert!(
        !sources
            .iter()
            .any(|source| matches!(source, ColumnOrigin::Ambiguous { .. }))
    );
}

#[test]
fn expr_qualified_star_is_an_unknown_shape_set_barrier() {
    let result = analyze(
        "SELECT STRUCT(1 AS value).* UNION ALL SELECT id FROM known",
        AnalyzeOptions {
            dialect: Dialect::BigQuery,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert!(result.columns.has_unresolved_stars);
    assert!(result.columns.mappings.iter().any(|mapping| {
        mapping.target.column == "*"
            && mapping.sources.iter().any(
                |source| matches!(source, ColumnOrigin::Ambiguous { column, .. } if column == "*"),
            )
    }));
}

fn assert_qualified_alias_star_expands(sql: &str) {
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(AliasCatalog)),
            dialect: Dialect::Generic,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 2);
    assert!(result.columns.mappings.iter().all(|mapping| {
        mapping
            .sources
            .iter()
            .all(|source| matches!(source, ColumnOrigin::Concrete { .. }))
    }));
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "id")),
        vec![("actual_table".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "event")),
        vec![("actual_table".into(), "event".into())]
    );
}

#[test]
fn qualified_alias_star_uses_catalog_table_binding() {
    for sql in [
        "SELECT a.* FROM actual_table AS a",
        "WITH x AS (SELECT a.* FROM actual_table AS a) SELECT * FROM x",
    ] {
        assert_qualified_alias_star_expands(sql);
    }
}

#[test]
fn qualified_alias_star_without_catalog_keeps_actual_table_wildcard() {
    let result = analyze(
        "SELECT a.* FROM actual_table AS a",
        AnalyzeOptions {
            dialect: Dialect::Generic,
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 1);
    match &result.columns.mappings[0].sources[0] {
        ColumnOrigin::Wildcard { table } => assert_eq!(table.table, "actual_table"),
        other => panic!("expected Wildcard, got {other:?}"),
    }
}

#[test]
fn select_star_without_catalog_preserved() {
    let result = analyze("SELECT * FROM users", AnalyzeOptions::default())
        .expect("parse")
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(result.columns.mappings.len(), 1);
    match &result.columns.mappings[0].sources[0] {
        ColumnOrigin::Wildcard { table } => assert_eq!(table.table, "users"),
        other => panic!("expected Wildcard, got {other:?}"),
    }
}

#[test]
fn ambiguous_column_resolved_by_catalog() {
    let sql = "SELECT name FROM users JOIN orders ON users.id = orders.user_id";
    let result = analyze(sql, opts_with_catalog())
        .expect("parse")
        .into_iter()
        .next()
        .unwrap();
    let m = find_mapping(&result.columns.mappings, "name");
    assert_eq!(concrete_sources(m), vec![("users".into(), "name".into())]);
}

#[test]
fn ambiguous_column_without_catalog() {
    let sql = "SELECT name FROM users JOIN orders ON users.id = orders.user_id";
    let result = analyze(sql, AnalyzeOptions::default())
        .expect("parse")
        .into_iter()
        .next()
        .unwrap();
    let m = find_mapping(&result.columns.mappings, "name");
    match &m.sources[0] {
        ColumnOrigin::Ambiguous { column, candidates } => {
            assert_eq!(column, "name");
            assert!(candidates.len() >= 2);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn catalog_does_not_fabricate_unresolved_column_owner() {
    let result = analyze(
        "SELECT missing",
        AnalyzeOptions {
            catalog: Some(Box::new(EagerCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();
    let m = find_mapping(&result.columns.mappings, "missing");
    match &m.sources[0] {
        ColumnOrigin::Ambiguous { column, candidates } => {
            assert_eq!(column, "missing");
            assert!(candidates.is_empty());
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

#[test]
fn catalog_preserves_qualified_columns() {
    let sql =
        "SELECT users.name, orders.amount FROM users JOIN orders ON users.id = orders.user_id";
    let result = analyze(sql, opts_with_catalog())
        .expect("parse")
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "name")),
        vec![("users".into(), "name".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.columns.mappings, "amount")),
        vec![("orders".into(), "amount".into())]
    );
}

struct SetOperationCatalog;

impl CatalogProvider for SetOperationCatalog {
    fn list_columns(&self, table: &TableRef) -> Option<Vec<String>> {
        match table.table.as_str() {
            "users" => Some(vec!["id".into(), "name".into(), "email".into()]),
            "other" => Some(vec!["a".into(), "b".into(), "c".into(), "d".into()]),
            "ext_a" => Some(vec!["col_x".into(), "col_y".into()]),
            _ => None,
        }
    }

    fn resolve_column(&self, _column: &str, _candidates: &[TableRef]) -> Option<TableRef> {
        None
    }
}

#[test]
fn set_operation_expands_leading_star_before_positional_merge() {
    let sql = "SELECT * FROM users UNION ALL SELECT a, b, c FROM other";
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 3);
    assert_eq!(result.columns.mappings[0].target.column, "id");
    assert_eq!(result.columns.mappings[1].target.column, "name");
    assert_eq!(result.columns.mappings[2].target.column, "email");
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("other".into(), "a".into()), ("users".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(&result.columns.mappings[1]),
        vec![
            ("other".into(), "b".into()),
            ("users".into(), "name".into())
        ]
    );
}

#[test]
fn set_operation_preserves_non_leading_star_contribution() {
    let sql = "SELECT id, * FROM users UNION ALL SELECT a, b, c, d FROM other";
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 4);
    assert_eq!(
        concrete_sources(&result.columns.mappings[1]),
        vec![("other".into(), "b".into()), ("users".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(&result.columns.mappings[3]),
        vec![
            ("other".into(), "d".into()),
            ("users".into(), "email".into())
        ]
    );
}

#[test]
fn set_operation_branches_survive_cte_and_derived_boundaries() {
    let sql = "WITH combined AS (SELECT * FROM users UNION ALL SELECT a, b, c FROM other) \
               SELECT * FROM (SELECT * FROM combined) derived";
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 3);
    assert_eq!(
        concrete_sources(&result.columns.mappings[1]),
        vec![
            ("other".into(), "b".into()),
            ("users".into(), "name".into())
        ]
    );
}

#[test]
fn non_leading_star_in_right_set_branch_contributes_to_named_output() {
    let sql = "WITH lit AS (SELECT 1 AS col_a), \
               u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM ext_a) \
               SELECT col_a FROM u";
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 1);
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::Concrete { table, column }, ColumnOrigin::SourceFree { column: marker }]
            if table.table == "ext_a" && column == "col_x" && marker == "col_a"
    ));
}

#[test]
fn named_lookup_through_set_operation_cte_keeps_all_branches() {
    let sql = "WITH combined AS (SELECT * FROM users UNION ALL SELECT a, b, c FROM other) \
               SELECT name FROM combined";
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![
            ("other".into(), "b".into()),
            ("users".into(), "name".into())
        ]
    );
}

#[test]
fn named_lookup_through_projection_cte_and_derived_star_chain() {
    let sql = "WITH base AS (SELECT * FROM users), wrapped AS (SELECT * FROM base) \
               SELECT name FROM (SELECT * FROM wrapped) derived";
    let result = analyze(
        sql,
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 1);
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("users".into(), "name".into())]
    );
}

#[test]
fn named_lookup_through_unknown_projection_star_is_indeterminate() {
    let result = analyze(
        "WITH base AS (SELECT * FROM unknown) SELECT name FROM base",
        AnalyzeOptions::default(),
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 1);
    assert!(matches!(
        result.columns.mappings[0].sources.as_slice(),
        [ColumnOrigin::NamedWildcard { table, column }]
            if table.table == "unknown" && column == "name"
    ));
}

#[test]
fn named_lookup_through_projection_preserves_inner_transform() {
    let result = analyze(
        "WITH aggregated AS (SELECT SUM(amount) AS total FROM orders) \
         SELECT total FROM aggregated",
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 1);
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("orders".into(), "amount".into())]
    );
    assert_eq!(
        result.columns.mappings[0].transform,
        sqllineage::TransformKind::Aggregation
    );
}

#[test]
fn named_lookup_through_projection_preserves_inner_expression_transform() {
    let result = analyze(
        "WITH transformed AS (SELECT amount + 1 AS adjusted FROM orders) \
         SELECT adjusted FROM transformed",
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    )
    .expect("parse")
    .into_iter()
    .next()
    .unwrap();

    assert_eq!(result.columns.mappings.len(), 1);
    assert_eq!(
        concrete_sources(&result.columns.mappings[0]),
        vec![("orders".into(), "amount".into())]
    );
    assert_eq!(
        result.columns.mappings[0].transform,
        sqllineage::TransformKind::Expression
    );
}

#[test]
fn catalog_known_set_arity_mismatch_is_an_analysis_error() {
    let result = analyze(
        "SELECT * FROM users UNION ALL SELECT a, b, c, d FROM other",
        AnalyzeOptions {
            catalog: Some(Box::new(SetOperationCatalog)),
            ..AnalyzeOptions::default()
        },
    );
    let error = match result {
        Ok(_) => panic!("catalog-known arity mismatch should not be truncated"),
        Err(error) => error,
    };
    assert_eq!(
        error.message,
        "set operation arity mismatch: left has 3 columns, right has 4 columns"
    );
}
