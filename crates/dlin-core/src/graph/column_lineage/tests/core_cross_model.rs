use std::collections::HashMap;

use super::*;
use crate::parser::manifest::{
    DependsOn, Manifest, ManifestColumn, ManifestConfig, ManifestNode, ManifestSource,
};

fn source_free_union_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }

    fn source(unique_id: &str, name: &str, output_columns: &[&str]) -> ManifestSource {
        ManifestSource {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            database: None,
            schema: None,
            identifier: None,
        }
    }

    let source_a_id = "source.proj.external_table_a";
    let source_b_id = "source.proj.external_table_b";
    let upstream_id = "model.proj.upstream_model";
    let union_id = "model.proj.union_model";
    let mut sources = HashMap::new();
    sources.insert(
        source_a_id.to_string(),
        source(source_a_id, "external_table_a", &["real_col", "id"]),
    );
    sources.insert(
        source_b_id.to_string(),
        source(source_b_id, "external_table_b", &["id"]),
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        upstream_id.to_string(),
        model(
            upstream_id,
            "upstream_model",
            &[source_a_id, source_b_id],
            &["real_col", "id"],
            "SELECT real_col, id FROM external_table_a UNION ALL SELECT CAST(NULL AS STRING) AS real_col, id FROM external_table_b",
        ),
    );
    nodes.insert(
        union_id.to_string(),
        model(
            union_id,
            "union_model",
            &[upstream_id],
            &["real_col", "id"],
            "WITH branches AS (SELECT real_col, id FROM upstream_model UNION ALL SELECT CAST(NULL AS STRING) AS real_col, CAST(NULL AS INT64) AS id) SELECT real_col, id FROM branches",
        ),
    );

    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

fn bigquery_compound_field_access_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }

    let source_id = "source.proj.external_table_a";
    let upstream_id = "model.proj.upstream_model";
    let array_id = "model.proj.array_model";
    let mut sources = HashMap::new();
    sources.insert(
        source_id.to_string(),
        ManifestSource {
            unique_id: source_id.to_string(),
            name: "external_table_a".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(&["id", "items_array"]),
            database: None,
            schema: None,
            identifier: None,
        },
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        upstream_id.to_string(),
        model(
            upstream_id,
            "upstream_model",
            &[source_id],
            &["id", "items_array"],
            "SELECT id, items_array FROM external_table_a",
        ),
    );
    nodes.insert(
        array_id.to_string(),
        model(
            array_id,
            "array_model",
            &[upstream_id],
            &["first_item"],
            "SELECT base.items_array[OFFSET(0)] AS first_item FROM upstream_model AS base",
        ),
    );

    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

fn duplicate_column_error_manifest() -> Manifest {
    let mut manifest = make_cross_model_manifest();
    let columns = |names: &[&str]| {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect::<HashMap<_, _>>()
    };
    let model =
        |unique_id: &str, name: &str, dependencies: &[&str], output_columns: &[&str], sql: &str| {
            ManifestNode {
                unique_id: unique_id.to_string(),
                name: name.to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                original_file_path: None,
                columns: columns(output_columns),
                compiled_code: Some(sql.to_string()),
                database: None,
                schema: None,
            }
        };

    manifest.nodes.insert(
        "model.proj.left_model".to_string(),
        model(
            "model.proj.left_model",
            "left_model",
            &[],
            &["dup_col"],
            "SELECT * FROM unknown_left_table",
        ),
    );
    manifest.nodes.insert(
        "model.proj.right_model".to_string(),
        model(
            "model.proj.right_model",
            "right_model",
            &[],
            &["dup_col"],
            "SELECT missing_col FROM known_right_table",
        ),
    );
    manifest.nodes.insert(
        "model.proj.duplicate_target".to_string(),
        model(
            "model.proj.duplicate_target",
            "duplicate_target",
            &["model.proj.left_model", "model.proj.right_model"],
            &["dup_col", "other_col"],
            "SELECT l.dup_col AS dup_col, r.dup_col AS other_col FROM left_model AS l JOIN right_model AS r ON l.dup_col = r.dup_col",
        ),
    );
    manifest
}

fn bigquery_nested_star_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }

    fn source(unique_id: &str, name: &str, output_columns: &[&str]) -> ManifestSource {
        ManifestSource {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            database: None,
            schema: None,
            identifier: None,
        }
    }

    let raw_table = "source.proj.raw.raw_table";
    let raw_aux = "source.proj.raw.raw_aux";
    let stg_source = "model.proj.stg_source";
    let agg_model = "model.proj.agg_model";
    let aux_model = "model.proj.aux_model";
    let downstream_model = "model.proj.downstream_model";

    let mut sources = HashMap::new();
    sources.insert(
        raw_table.to_string(),
        source(
            raw_table,
            "raw_table",
            &["user_id", "updated_at", "col_a", "col_b"],
        ),
    );
    sources.insert(
        raw_aux.to_string(),
        source(raw_aux, "raw_aux", &["user_id", "aux_value"]),
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        stg_source.to_string(),
        model(
            stg_source,
            "stg_source",
            &[raw_table],
            &["user_id", "updated_at", "col_a", "col_b"],
            "SELECT user_id, updated_at, col_a, col_b FROM raw_table",
        ),
    );
    nodes.insert(
        agg_model.to_string(),
        model(
            agg_model,
            "agg_model",
            &[stg_source],
            &["user_id", "event"],
            "SELECT user_id, ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM stg_source AS t GROUP BY user_id",
        ),
    );
    nodes.insert(
        aux_model.to_string(),
        model(
            aux_model,
            "aux_model",
            &[raw_aux],
            &["user_id", "aux_value"],
            "SELECT user_id, aux_value FROM raw_aux",
        ),
    );
    nodes.insert(
        downstream_model.to_string(),
        model(
            downstream_model,
            "downstream_model",
            &[agg_model, aux_model],
            &["user_id", "updated_at", "col_a", "col_b", "aux_value"],
            "WITH join_data AS (SELECT base.* EXCEPT (event), base.event.* EXCEPT (user_id), IFNULL(aux.aux_value, 0) AS aux_value FROM agg_model AS base LEFT JOIN aux_model AS aux ON base.user_id = aux.user_id) SELECT * FROM join_data",
        ),
    );

    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

#[test]
fn test_cross_model_bigquery_source_free_union_reaches_external_sources() {
    let manifest = source_free_union_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "union_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.total_columns, 2);
    assert_eq!(result.traced_columns, 2);
    assert_eq!(result.columns.len(), 2);

    let real_col = result
        .columns
        .iter()
        .find(|column| column.column == "real_col")
        .unwrap();
    assert_eq!(
        real_col
            .sources
            .iter()
            .map(|source| (source.table.as_str(), source.column.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "real_col")]
    );

    let id = result
        .columns
        .iter()
        .find(|column| column.column == "id")
        .unwrap();
    let mut id_sources = id
        .sources
        .iter()
        .map(|source| (source.table.as_str(), source.column.as_str()))
        .collect::<Vec<_>>();
    id_sources.sort_unstable();
    assert_eq!(
        id_sources,
        vec![("external_table_a", "id"), ("external_table_b", "id")]
    );
}

#[test]
fn test_cross_model_bigquery_compound_field_access_reaches_external_array_column() {
    let manifest = bigquery_compound_field_access_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "array_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let first_item = result
        .columns
        .iter()
        .find(|column| column.column == "first_item")
        .expect("first_item should be present");
    assert_eq!(
        first_item.sources.len(),
        1,
        "sources: {:?}",
        first_item.sources
    );
    let source = &first_item.sources[0];
    assert_eq!(source.column, "items_array");
    assert_eq!(source.table, "external_table_a");
    assert!(
        source
            .model_path
            .iter()
            .any(|(model, column, _)| model == "upstream_model" && column == "items_array"),
        "expected upstream model path, got: {:?}",
        source.model_path
    );
}

#[test]
fn test_cross_model_preserves_distinct_same_column_errors() {
    let manifest = duplicate_column_error_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "duplicate_target",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let duplicate_errors: Vec<_> = result
        .errors
        .iter()
        .filter(|error| error.what.starts_with("column 'dup_col':"))
        .collect();
    assert_eq!(duplicate_errors.len(), 2, "errors: {:?}", result.errors);
    assert_eq!(
        duplicate_errors
            .iter()
            .filter(|error| error.hint.is_some())
            .count(),
        1,
        "expected one unresolved-star hint: {:?}",
        result.errors
    );
    assert_eq!(
        duplicate_errors
            .iter()
            .filter(|error| error.hint.is_none())
            .count(),
        1,
        "expected one no-mapping diagnostic: {:?}",
        result.errors
    );
    assert!(
        duplicate_errors
            .iter()
            .any(|error| error.what.contains("unexpanded SELECT *")),
        "expected unresolved-star reason: {:?}",
        result.errors
    );
    assert!(
        duplicate_errors
            .iter()
            .any(|error| error.what.contains("no sqllineage mapping")),
        "expected no-mapping reason: {:?}",
        result.errors
    );
}

#[test]
fn test_cross_model_bigquery_nested_star_keeps_known_user_id_lineage() {
    // BigQuery-specific EXCEPT and nested field-star syntax intentionally
    // leaves some STRUCT expansion unresolved; known scalar lineage must
    // remain available rather than being discarded with those errors.
    let manifest = bigquery_nested_star_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let user_id = result
        .columns
        .iter()
        .find(|column| column.column == "user_id")
        .expect("user_id should remain in the known lineage columns");
    assert!(
        user_id
            .sources
            .iter()
            .any(|source| source.table == "raw_table" && source.column == "user_id"),
        "user_id should trace to raw_table.user_id, got: {:?}; errors: {:?}",
        user_id.sources,
        result.errors
    );
    assert!(
        !result
            .columns
            .iter()
            .any(|column| column.column == "updated_at"),
        "nested field star should remain unresolved"
    );
    assert!(
        result.errors.iter().any(|error| {
            error.kind == ColumnLineageErrorKind::ColumnNotFound
                && error.what.starts_with("column 'updated_at':")
        }),
        "expected an uncertainty/error for nested field star, got: {:?}",
        result.errors
    );
}

/// Build a manifest with 3 levels: customers → orders → stg_orders → raw.orders
#[test]
fn test_cross_model_single_hop() {
    // orders.order_id → stg_orders.order_id → raw.orders.id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    // Should trace through stg_orders to raw source (orders table)
    assert!(
        order_id
            .sources
            .iter()
            .any(|s| s.column == "id" && s.table.contains("orders")),
        "order_id should trace to raw orders.id, got: {:?}",
        order_id.sources
    );
    // model_path should show the hop through stg_orders
    let src = order_id.sources.iter().find(|s| s.column == "id").unwrap();
    assert!(
        src.model_path.iter().any(|(m, _, _)| m == "stg_orders"),
        "model_path should include stg_orders, got: {:?}",
        src.model_path
    );
}

#[test]
fn test_cross_model_two_hops() {
    // customers.customer_id → orders.customer_id → stg_orders.customer_id → raw.orders.user_id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "customers",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let customer_id = result
        .columns
        .iter()
        .find(|c| c.column == "customer_id")
        .unwrap();
    assert!(
        customer_id
            .sources
            .iter()
            .any(|s| s.column == "user_id" && s.table.contains("orders")),
        "customer_id should trace to raw orders.user_id, got: {:?}",
        customer_id.sources
    );
    // model_path should show both hops: orders → stg_orders
    let src = customer_id
        .sources
        .iter()
        .find(|s| s.column == "user_id")
        .unwrap();
    assert!(
        src.model_path.iter().any(|(m, _, _)| m == "orders")
            && src.model_path.iter().any(|(m, _, _)| m == "stg_orders"),
        "model_path should include orders and stg_orders, got: {:?}",
        src.model_path
    );
    // orders should come before stg_orders in the path (closer to target)
    let orders_pos = src
        .model_path
        .iter()
        .position(|(m, _, _)| m == "orders")
        .unwrap();
    let stg_pos = src
        .model_path
        .iter()
        .position(|(m, _, _)| m == "stg_orders")
        .unwrap();
    assert!(
        orders_pos < stg_pos,
        "orders should precede stg_orders in path"
    );
}

#[test]
fn test_cross_model_join_sources() {
    // orders.total_amount → stg_payments.amount → raw.payments.amount
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let total_amount = result
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(
        total_amount
            .sources
            .iter()
            .any(|s| s.column == "amount" && s.table.contains("payments")),
        "total_amount should trace to raw payments.amount, got: {:?}",
        total_amount.sources
    );
    // model_path should show the hop through stg_payments
    let src = total_amount
        .sources
        .iter()
        .find(|s| s.column == "amount")
        .unwrap();
    assert!(
        src.model_path.iter().any(|(m, _, _)| m == "stg_payments"),
        "model_path should include stg_payments, got: {:?}",
        src.model_path
    );
}

#[test]
fn test_cross_model_source_table_is_leaf() {
    // stg_orders directly references a source — cross-model should not change the result
    let manifest = make_cross_model_manifest();
    let single = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let cross = compute_cross_model_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(single.columns.len(), cross.columns.len());
    for (s, c) in single.columns.iter().zip(cross.columns.iter()) {
        assert_eq!(s.column, c.column);
        assert_eq!(s.sources, c.sources);
    }
}

#[test]
fn test_cross_model_model_not_found() {
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "nonexistent",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_cross_model_diamond_different_columns_through_shared_model() {
    // In a diamond DAG, different columns (x and y) flow through a shared
    // upstream model. Both should be resolved independently — the visited set
    // must not truncate the second column's path through the shared model.
    let manifest = make_diamond_manifest();

    // Verify left_model traces x through shared to raw_data
    let left = compute_cross_model_column_lineage(
        &manifest,
        "left_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(left.errors.is_empty(), "left errors: {:?}", left.errors);
    let left_x = left.columns.iter().find(|c| c.column == "x").unwrap();
    assert!(
        left_x.sources.iter().any(|s| s.column == "x"),
        "left_model.x should trace through shared, got: {:?}",
        left_x.sources
    );

    // Verify right_model traces y through shared to raw_data
    let right = compute_cross_model_column_lineage(
        &manifest,
        "right_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(right.errors.is_empty(), "right errors: {:?}", right.errors);
    let right_y = right.columns.iter().find(|c| c.column == "y").unwrap();
    assert!(
        right_y.sources.iter().any(|s| s.column == "y"),
        "right_model.y should trace through shared, got: {:?}",
        right_y.sources
    );

    // Both left and right depend on 'shared' — the key assertion is that
    // resolving one does not prevent the other from being resolved.
    // With the old model-only visited set, whichever resolved first would
    // block the other from tracing through 'shared'.
    assert!(
        !left_x.sources.is_empty() && !right_y.sources.is_empty(),
        "both paths through shared should resolve independently"
    );
}

// The orders model uses aliases (stg_orders AS o, stg_payments AS p).
// These two tests guard against regressions where collect_leaves returns
// the SQL alias instead of the actual model name.

#[test]
fn test_join_alias_resolves_to_model_name() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // p.amount → table must be "stg_payments", not the alias "p"
    let total_amount = result
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert_eq!(
        total_amount.sources[0].table, "stg_payments",
        "expected stg_payments, got SQL alias 'p': {:?}",
        total_amount.sources
    );

    // o.order_id → table must be "stg_orders", not the alias "o"
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert_eq!(
        order_id.sources[0].table, "stg_orders",
        "expected stg_orders, got SQL alias 'o': {:?}",
        order_id.sources
    );
}

#[test]
fn test_cross_model_join_alias_traces_to_raw_source() {
    // Cross-model lineage must follow through aliases to reach raw sources.
    // Before the fix, "p" didn't match upstream_models so the trace stopped early.
    let manifest = make_test_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let total_amount = result
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(!total_amount.sources.is_empty());
    let src = &total_amount.sources[0];
    assert_ne!(src.table, "p", "source table must not be SQL alias 'p'");
    assert_ne!(
        src.table, "stg_payments",
        "cross-model must trace beyond stg_payments"
    );
    assert_eq!(src.table, "raw.payments");
    assert_eq!(src.column, "amount");
}
