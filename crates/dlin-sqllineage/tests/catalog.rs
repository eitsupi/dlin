mod common;

use common::{
    analyze_one_with, catalog_entry, concrete_sources, find_mapping, output_name, unresolved,
};
use dlin_sqllineage::{
    Catalog, CatalogEntry, Diagnostic, DiagnosticKind, Dialect, Lineage, Relation, Severity, Shape,
    Unresolved,
};

struct MockCatalog;

impl Catalog for MockCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        match relation.name.value.as_str() {
            "users" => Some(catalog_entry(relation, &["id", "name", "email"])),
            "orders" => Some(catalog_entry(relation, &["id", "user_id", "amount"])),
            _ => None,
        }
    }
}

struct EagerCatalog;

/// Claims every relation it is asked about has a `missing` column.
impl Catalog for EagerCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        Some(catalog_entry(relation, &["missing"]))
    }
}

struct AliasCatalog;

impl Catalog for AliasCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        (relation.name.value == "actual_table").then(|| catalog_entry(relation, &["id", "event"]))
    }
}

struct WildcardCatalog;

impl Catalog for WildcardCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        match relation.name.value.as_str() {
            "users" => Some(catalog_entry(relation, &["id", "name", "secret"])),
            "other" => Some(catalog_entry(relation, &["a", "b"])),
            _ => None,
        }
    }
}

fn configured(
    sql: &str,
    dialect: Dialect,
    catalog: Option<&dyn Catalog>,
) -> dlin_sqllineage::StatementAnalysis {
    analyze_one_with(sql, dialect, catalog)
}

#[test]
fn select_star_with_catalog_expands() {
    let result = configured("SELECT * FROM users", Dialect::Generic, Some(&MockCatalog));
    assert_eq!(result.output.columns.len(), 3);

    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "id")),
        vec![("users".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "name")),
        vec![("users".into(), "name".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "email")),
        vec![("users".into(), "email".into())]
    );
}

#[test]
fn wildcard_except_removes_catalog_column() {
    let result = configured(
        "SELECT * EXCEPT (secret) FROM users",
        Dialect::BigQuery,
        Some(&WildcardCatalog),
    );

    let names = result
        .output
        .columns
        .iter()
        .map(|mapping| output_name(mapping).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["id", "name"]);
}

#[test]
fn qualified_wildcard_except_uses_relation_binding() {
    let result = configured(
        "SELECT u.* EXCEPT (secret) FROM users AS u",
        Dialect::BigQuery,
        Some(&WildcardCatalog),
    );

    assert_eq!(
        result
            .output
            .columns
            .iter()
            .map(|mapping| output_name(mapping).unwrap())
            .collect::<Vec<_>>(),
        ["id", "name"]
    );
}

#[test]
fn wildcard_ilike_filters_known_columns() {
    let result = configured(
        "SELECT * ILIKE '%na%' FROM users",
        Dialect::Snowflake,
        Some(&WildcardCatalog),
    );

    assert_eq!(
        result
            .output
            .columns
            .iter()
            .map(|mapping| output_name(mapping).unwrap())
            .collect::<Vec<_>>(),
        ["name"]
    );
}

#[test]
fn unknown_cte_star_does_not_recompose_excluded_names() {
    let result = configured(
        "WITH x AS (SELECT * FROM unknown_source) SELECT * EXCEPT (missing) FROM x",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    assert!(
        !result
            .output
            .columns
            .iter()
            .any(|mapping| output_name(mapping) == Some("missing"))
    );
}

#[test]
fn unknown_cte_excluded_name_is_not_named_wildcard_fallback() {
    let result = configured(
        "WITH x AS (SELECT * EXCEPT (secret) FROM unknown_source) SELECT secret FROM x",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. } if matches!(&unresolved[..], [Unresolved::Unbound { column }] if column.value == "secret")
    ));
}

#[test]
fn unknown_cte_allowed_name_keeps_named_wildcard_fallback() {
    let result = configured(
        "WITH x AS (SELECT * EXCEPT (secret) FROM unknown_source) SELECT id FROM x",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. } if matches!(&unresolved[..], [Unresolved::NamedBehindStar { column, .. }] if column.value == "id")
    ));
}

#[test]
fn unknown_cte_rename_maps_new_name_without_restoring_old_name() {
    let result = configured(
        "WITH x AS (SELECT * RENAME (id AS user_id) FROM unknown_source) \
         SELECT user_id, id FROM x",
        Dialect::Snowflake,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. } if matches!(&unresolved[..], [Unresolved::NamedBehindStar { column, .. }] if column.value == "id")
    ));
    assert!(matches!(
        &result.output.columns[1].lineage,
        Lineage::Partial { unresolved, .. } if matches!(&unresolved[..], [Unresolved::Unbound { column }] if column.value == "id")
    ));
}

#[test]
fn unknown_cte_replace_keeps_replacement_lineage() {
    let result = configured(
        "WITH x AS (SELECT * REPLACE (other AS id) FROM unknown_source) \
         SELECT id FROM x",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Complete(sources) if sources.len() == 1 && sources[0].column.value == "other"
    ));
}

#[test]
fn wildcard_replace_uses_replacement_expression_lineage() {
    let result = configured(
        "SELECT * REPLACE (name AS id) FROM users",
        Dialect::BigQuery,
        Some(&WildcardCatalog),
    );

    let id = find_mapping(&result.output.columns, "id");
    assert_eq!(concrete_sources(id), vec![("users".into(), "name".into())]);
}

#[test]
fn wildcard_rename_preserves_position() {
    let result = configured(
        "SELECT * EXCLUDE (secret) RENAME (id AS user_id) FROM users",
        Dialect::Snowflake,
        Some(&WildcardCatalog),
    );

    assert_eq!(output_name(&result.output.columns[0]).unwrap(), "user_id");
    assert_eq!(output_name(&result.output.columns[1]).unwrap(), "name");
}

#[test]
fn qualified_exclude_uses_qualified_relation_not_suffix_matching() {
    let result = configured(
        "SELECT u.* EXCLUDE (u.secret) FROM users AS u",
        Dialect::Snowflake,
        Some(&WildcardCatalog),
    );

    assert_eq!(
        result
            .output
            .columns
            .iter()
            .map(|mapping| output_name(mapping).unwrap())
            .collect::<Vec<_>>(),
        ["id", "name"]
    );
}

#[test]
fn qualified_exclude_width_is_used_for_set_operation_arity() {
    let result = configured(
        "SELECT u.* EXCLUDE (u.secret) FROM users AS u \
         UNION ALL SELECT a, b FROM other",
        Dialect::Snowflake,
        Some(&WildcardCatalog),
    );

    assert_eq!(result.output.columns.len(), 2);
}

#[test]
fn qualified_exclude_requires_exact_catalog_qualification() {
    let result = configured(
        "SELECT u.* EXCLUDE (cat2.sch.users.secret) FROM cat1.sch.users AS u",
        Dialect::Snowflake,
        Some(&WildcardCatalog),
    );

    assert!(
        result
            .output
            .columns
            .iter()
            .any(|mapping| output_name(mapping) == Some("secret"))
    );
}

#[test]
fn qualified_field_path_star_does_not_forge_table_ref() {
    let result = configured("SELECT base.event.* FROM base", Dialect::Generic, None);

    assert!(result.has_unresolved_stars);
    assert!(!result.read.iter().any(|table| table.name.value == "event"));
    // The field star is not an output column and has no relation of its own.
    assert!(result.output.columns.is_empty());
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("expected an unexpanded field star")
    };
    assert_eq!(unexpanded.len(), 1);
    assert!(unexpanded[0].relation.is_none());
    assert_eq!(unexpanded[0].after_ordinal, None);
    // Its lineage keeps the field itself and marks the expansion as unknown.
    let Lineage::Partial { known, unresolved } = &unexpanded[0].lineage else {
        panic!("expected partial lineage, got {:?}", unexpanded[0].lineage)
    };
    assert_eq!(
        known
            .iter()
            .map(|source| (
                source.relation.name.value.as_str(),
                source.column.value.as_str()
            ))
            .collect::<Vec<_>>(),
        [("base", "event")]
    );
    assert_eq!(
        unresolved.as_slice(),
        [Unresolved::Unsupported {
            what: "star expansion"
        }]
    );
}

#[test]
fn cte_field_path_star_keeps_upstream_field_ancestry() {
    let result = configured(
        "WITH base AS (SELECT event FROM source) SELECT base.event.* FROM base",
        Dialect::Generic,
        None,
    );

    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("expected an unexpanded field star")
    };
    assert!(unexpanded.iter().any(|star| {
        let sources = match &star.lineage {
            Lineage::Complete(sources) | Lineage::Partial { known: sources, .. } => sources,
        };
        sources
            .iter()
            .any(|source| source.relation.name.value == "source" && source.column.value == "event")
    }));
    assert!(!result.read.iter().any(|table| table.name.value == "event"));
}

#[test]
fn derived_field_path_star_keeps_upstream_field_ancestry() {
    let result = configured(
        "SELECT base.event.* FROM (SELECT event FROM source) AS base",
        Dialect::Generic,
        None,
    );

    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("expected an unexpanded field star")
    };
    assert!(unexpanded.iter().any(|star| {
        let sources = match &star.lineage {
            Lineage::Complete(sources) | Lineage::Partial { known: sources, .. } => sources,
        };
        sources
            .iter()
            .any(|source| source.relation.name.value == "source" && source.column.value == "event")
    }));
    assert!(!result.read.iter().any(|table| table.name.value == "event"));
}

#[test]
fn unknown_root_replace_keeps_barrier_and_adds_replacement_mapping() {
    let result = configured(
        "SELECT * REPLACE (other AS id) FROM unknown_source",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    assert!(matches!(&result.output.shape, Shape::Open { .. }));
    assert!(result.output.columns.iter().any(|mapping| {
        output_name(mapping) == Some("id")
            && matches!(&mapping.lineage, Lineage::Complete(sources)
                if sources.len() == 1 && sources[0].column.value == "other")
    }));
}

#[test]
fn unknown_root_rename_keeps_barrier_and_adds_named_wildcard() {
    let result = configured(
        "SELECT * RENAME (id AS user_id) FROM unknown_source",
        Dialect::Snowflake,
        None,
    );

    assert!(matches!(
        &result.output.shape,
        dlin_sqllineage::Shape::Open { .. }
    ));
    assert!(result.output.columns.iter().any(|mapping| {
        output_name(mapping) == Some("user_id")
            && matches!(
                &mapping.lineage,
                Lineage::Partial { unresolved, .. }
                    if matches!(&unresolved[..], [Unresolved::NamedBehindStar { column, .. }] if column.value == "id")
            )
    }));
}

#[test]
fn multiple_unknown_stars_keep_name_available_from_unfiltered_star() {
    let result = configured(
        "WITH x AS (SELECT *, * EXCEPT (secret) FROM unknown_source) SELECT secret FROM x",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(&unresolved[..], [Unresolved::NamedBehindStar { column, .. }] if column.value == "secret")
    ));
}

#[test]
fn unknown_qualified_exclude_does_not_restore_excluded_name() {
    let result = configured(
        "WITH x AS (SELECT u.* EXCLUDE (u.secret) FROM unknown_source AS u) \
         SELECT secret FROM x",
        Dialect::Snowflake,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(&unresolved[..], [Unresolved::Unbound { column }] if column.value == "secret")
    ));
}

#[test]
fn unqualified_qualified_exclude_keeps_other_relation_wildcard() {
    let result = configured(
        "WITH x AS (SELECT * EXCLUDE (u.secret) FROM unknown_source AS u, other_source AS v) \
         SELECT secret FROM x",
        Dialect::Snowflake,
        None,
    );

    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(&unresolved[..], [Unresolved::NamedBehindStar { relation, column }]
                if relation.name.value == "other_source" && column.value == "secret")
    ));
}

#[test]
fn set_operation_name_decision_uses_left_branch_only() {
    let result = configured(
        "WITH x AS (SELECT * FROM unknown_left UNION ALL SELECT * EXCEPT (secret) FROM unknown_right) \
         SELECT secret FROM x",
        Dialect::BigQuery,
        None,
    );

    let sources = unresolved(&result.output.columns[0]);
    assert!(sources.iter().any(|source| matches!(source,
        Unresolved::NamedBehindStar { relation, column }
            if relation.name.value == "unknown_left" && column.value == "secret")));
    assert!(sources.iter().any(|source| matches!(source,
        Unresolved::Unbound { column } if column.value == "secret")));
}

#[test]
fn set_operation_keeps_named_wildcard_sources_from_both_branches() {
    let result = configured(
        "WITH x AS (SELECT * FROM unknown_left UNION ALL SELECT * FROM unknown_right) \
         SELECT secret FROM x",
        Dialect::BigQuery,
        None,
    );

    let sources = unresolved(&result.output.columns[0]);
    for table_name in ["unknown_left", "unknown_right"] {
        assert!(sources.iter().any(|source| {
            matches!(source, Unresolved::NamedBehindStar { relation, column }
                if relation.name.value == table_name && column.value == "secret")
        }));
    }
    assert!(
        !sources
            .iter()
            .any(|source| matches!(source, Unresolved::Unbound { .. }))
    );
}

#[test]
fn expr_qualified_star_is_an_unknown_shape_set_barrier() {
    let result = configured(
        "SELECT STRUCT(1 AS value).* UNION ALL SELECT id FROM known",
        Dialect::BigQuery,
        None,
    );

    assert!(matches!(&result.output.shape, Shape::Open { .. }));
}

fn assert_qualified_alias_star_expands(sql: &str) {
    let result = configured(sql, Dialect::Generic, Some(&AliasCatalog));

    assert_eq!(result.output.columns.len(), 2);
    assert!(
        result
            .output
            .columns
            .iter()
            .all(|mapping| matches!(&mapping.lineage, Lineage::Complete(_)))
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "id")),
        vec![("actual_table".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "event")),
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
    let result = configured("SELECT a.* FROM actual_table AS a", Dialect::Generic, None);

    assert!(result.has_unresolved_stars);
    assert!(result.output.columns.is_empty());
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("expected an unexpanded star")
    };
    assert_eq!(unexpanded.len(), 1);
    assert_eq!(unexpanded[0].after_ordinal, None);
    assert_eq!(
        unexpanded[0]
            .relation
            .as_ref()
            .map(|r| r.name.value.as_str()),
        Some("actual_table")
    );
}

#[test]
fn select_star_without_catalog_preserved() {
    let result = configured("SELECT * FROM users", Dialect::Generic, None);
    assert!(result.has_unresolved_stars);
    assert!(result.output.columns.is_empty());
    let Shape::Open { unexpanded } = &result.output.shape else {
        panic!("expected an unexpanded star")
    };
    assert_eq!(unexpanded.len(), 1);
    assert_eq!(unexpanded[0].after_ordinal, None);
    assert_eq!(
        unexpanded[0]
            .relation
            .as_ref()
            .map(|r| r.name.value.as_str()),
        Some("users")
    );
}

#[test]
fn ambiguous_column_resolved_by_catalog() {
    let sql = "SELECT name FROM users JOIN orders ON users.id = orders.user_id";
    let result = configured(sql, Dialect::Generic, Some(&MockCatalog));
    let m = find_mapping(&result.output.columns, "name");
    assert_eq!(concrete_sources(m), vec![("users".into(), "name".into())]);
}

#[test]
fn ambiguous_column_without_catalog() {
    let sql = "SELECT name FROM users JOIN orders ON users.id = orders.user_id";
    let result = configured(sql, Dialect::Generic, None);
    let m = find_mapping(&result.output.columns, "name");
    assert!(matches!(&m.lineage, Lineage::Partial { unresolved, .. }
        if matches!(&unresolved[..], [Unresolved::Ambiguous { column, candidates }]
            if column.value == "name" && candidates.len() >= 2)));
}

#[test]
fn catalog_does_not_fabricate_unresolved_column_owner() {
    // The catalog claims that any relation has a `missing` column. A column
    // without a relation in scope must not be attributed to one of them.
    let result = configured("SELECT missing", Dialect::Generic, Some(&EagerCatalog));
    let m = find_mapping(&result.output.columns, "missing");
    assert!(matches!(&m.lineage, Lineage::Partial { known, unresolved }
        if known.is_empty()
            && matches!(&unresolved[..], [Unresolved::Unbound { column }] if column.value == "missing")));
    assert!(result.read.is_empty());
}

#[test]
fn catalog_preserves_qualified_columns() {
    let sql =
        "SELECT users.name, orders.amount FROM users JOIN orders ON users.id = orders.user_id";
    let result = configured(sql, Dialect::Generic, Some(&MockCatalog));
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "name")),
        vec![("users".into(), "name".into())]
    );
    assert_eq!(
        concrete_sources(find_mapping(&result.output.columns, "amount")),
        vec![("orders".into(), "amount".into())]
    );
}

struct SetOperationCatalog;

impl Catalog for SetOperationCatalog {
    fn lookup(&self, relation: &Relation) -> Option<CatalogEntry<'_>> {
        match relation.name.value.as_str() {
            "users" => Some(catalog_entry(relation, &["id", "name", "email"])),
            "other" => Some(catalog_entry(relation, &["a", "b", "c", "d"])),
            "ext_a" => Some(catalog_entry(relation, &["col_x", "col_y"])),
            _ => None,
        }
    }
}

#[test]
fn set_operation_expands_leading_star_before_positional_merge() {
    let sql = "SELECT * FROM users UNION ALL SELECT a, b, c FROM other";
    let result = configured(sql, Dialect::Generic, Some(&SetOperationCatalog));

    assert_eq!(result.output.columns.len(), 3);
    assert_eq!(output_name(&result.output.columns[0]).unwrap(), "id");
    assert_eq!(output_name(&result.output.columns[1]).unwrap(), "name");
    assert_eq!(output_name(&result.output.columns[2]).unwrap(), "email");
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("other".into(), "a".into()), ("users".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(&result.output.columns[1]),
        vec![
            ("other".into(), "b".into()),
            ("users".into(), "name".into())
        ]
    );
}

#[test]
fn set_operation_preserves_non_leading_star_contribution() {
    let sql = "SELECT id, * FROM users UNION ALL SELECT a, b, c, d FROM other";
    let result = configured(sql, Dialect::Generic, Some(&SetOperationCatalog));

    assert_eq!(result.output.columns.len(), 4);
    assert_eq!(
        concrete_sources(&result.output.columns[1]),
        vec![("other".into(), "b".into()), ("users".into(), "id".into())]
    );
    assert_eq!(
        concrete_sources(&result.output.columns[3]),
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
    let result = configured(sql, Dialect::Generic, Some(&SetOperationCatalog));

    assert_eq!(result.output.columns.len(), 3);
    assert_eq!(
        concrete_sources(&result.output.columns[1]),
        vec![
            ("other".into(), "b".into()),
            ("users".into(), "name".into())
        ]
    );
}

#[test]
fn source_free_left_branch_does_not_hide_star_source_of_right_branch() {
    let sql = "WITH lit AS (SELECT 1 AS col_a), \
               u AS (SELECT col_a FROM lit UNION ALL SELECT * FROM ext_a) \
               SELECT col_a FROM u";
    let result = configured(sql, Dialect::Generic, Some(&SetOperationCatalog));

    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(output_name(&result.output.columns[0]), Some("col_a"));
    // The literal branch contributes no source, so the lineage stays complete.
    assert!(matches!(
        &result.output.columns[0].lineage,
        Lineage::Complete(sources) if sources.len() == 1
    ));
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("ext_a".into(), "col_x".into())]
    );
}

#[test]
fn named_lookup_through_set_operation_cte_keeps_all_branches() {
    let sql = "WITH combined AS (SELECT * FROM users UNION ALL SELECT a, b, c FROM other) \
               SELECT name FROM combined";
    let result = configured(sql, Dialect::Generic, Some(&SetOperationCatalog));

    assert_eq!(
        concrete_sources(&result.output.columns[0]),
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
    let result = configured(sql, Dialect::Generic, Some(&SetOperationCatalog));

    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("users".into(), "name".into())]
    );
}

#[test]
fn named_lookup_through_unknown_projection_star_is_indeterminate() {
    let result = configured(
        "WITH base AS (SELECT * FROM unknown) SELECT name FROM base",
        Dialect::Generic,
        None,
    );

    assert_eq!(result.output.columns.len(), 1);
    assert!(matches!(&result.output.columns[0].lineage,
        Lineage::Partial { unresolved, .. }
            if matches!(&unresolved[..], [Unresolved::NamedBehindStar { relation, column }]
                if relation.name.value == "unknown" && column.value == "name")));
}

#[test]
fn named_lookup_through_projection_preserves_inner_transform() {
    let result = configured(
        "WITH aggregated AS (SELECT SUM(amount) AS total FROM orders) \
         SELECT total FROM aggregated",
        Dialect::Generic,
        Some(&SetOperationCatalog),
    );

    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("orders".into(), "amount".into())]
    );
    assert_eq!(
        result.output.columns[0].transform,
        dlin_sqllineage::Transform::Aggregation
    );
}

#[test]
fn named_lookup_through_projection_preserves_inner_expression_transform() {
    let result = configured(
        "WITH transformed AS (SELECT amount + 1 AS adjusted FROM orders) \
         SELECT adjusted FROM transformed",
        Dialect::Generic,
        Some(&SetOperationCatalog),
    );

    assert_eq!(result.output.columns.len(), 1);
    assert_eq!(
        concrete_sources(&result.output.columns[0]),
        vec![("orders".into(), "amount".into())]
    );
    assert_eq!(
        result.output.columns[0].transform,
        dlin_sqllineage::Transform::Expression
    );
}

#[test]
fn catalog_known_set_arity_mismatch_is_an_analysis_error() {
    let result = configured(
        "SELECT * FROM users UNION ALL SELECT a, b, c, d FROM other",
        Dialect::Generic,
        Some(&SetOperationCatalog),
    );
    assert!(result.output.columns.is_empty());
    assert_eq!(
        result.diagnostics,
        [Diagnostic {
            severity: Severity::Error,
            kind: DiagnosticKind::SetOperationArityMismatch { left: 3, right: 4 },
            location: None,
        }]
    );
}
