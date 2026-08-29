#![cfg(feature = "column-lineage")]

use dlin_core::graph::column_lineage::{ColumnLineageAnalysis, ColumnLineageCache, DlinDialect};
use std::path::PathBuf;

fn column_lineage_fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
        .join("column_lineage_project")
}

fn load_fixture_manifest() -> dlin_core::parser::manifest::Manifest {
    let manifest_path = column_lineage_fixture_dir()
        .join("target")
        .join("manifest.json");
    dlin_core::parser::manifest::load_manifest(&manifest_path).unwrap()
}

fn compute_column_lineage(
    manifest: &dlin_core::parser::manifest::Manifest,
    model: &str,
) -> dlin_core::graph::column_lineage::ModelColumnLineage {
    let mut cache = ColumnLineageCache::disabled();
    ColumnLineageAnalysis::new(manifest, DlinDialect::Generic, &mut cache)
        .compute_column_lineage(model)
}

fn compute_cross_model_column_lineage(
    manifest: &dlin_core::parser::manifest::Manifest,
    model: &str,
) -> dlin_core::graph::column_lineage::ModelColumnLineage {
    let mut cache = ColumnLineageCache::disabled();
    ColumnLineageAnalysis::new(manifest, DlinDialect::Generic, &mut cache)
        .compute_cross_model_column_lineage(model)
}

#[test]
fn test_stg_orders_cte_star_with_rename() {
    // stg_orders uses: WITH renamed AS (SELECT id AS order_id, ...) SELECT * FROM renamed
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "stg_orders");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 4);

    // order_id is renamed from id
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert_eq!(
        order_id.sources[0].column, "id",
        "order_id should trace to raw.orders.id"
    );

    // customer_id is renamed from user_id
    let customer_id = result
        .columns
        .iter()
        .find(|c| c.column == "customer_id")
        .unwrap();
    assert_eq!(customer_id.sources[0].column, "user_id");

    // Passthrough columns
    let order_date = result
        .columns
        .iter()
        .find(|c| c.column == "order_date")
        .unwrap();
    assert_eq!(order_date.sources[0].column, "order_date");
}

#[test]
fn test_orders_cte_star_with_schema_and_join() {
    // orders model: CTEs with SELECT * FROM 3-part qualified tables, then JOIN
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "orders");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 6);

    // order_id from stg_orders
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert!(!order_id.sources.is_empty(), "order_id should have sources");
    assert_eq!(order_id.sources[0].column, "order_id");

    // total_amount renamed from stg_payments.amount
    let total_amount = result
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(
        !total_amount.sources.is_empty(),
        "total_amount should have sources"
    );
    assert_eq!(total_amount.sources[0].column, "amount");
}

#[test]
fn test_customers_sql_inference_without_yaml_columns() {
    // customers model has no YAML columns — columns should be inferred from SQL
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "customers");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // Should infer: customer_id, first_name, last_name, email, order_count, lifetime_value
    assert_eq!(result.columns.len(), 6, "should infer 6 columns from SQL");

    let col_names: Vec<&str> = result.columns.iter().map(|c| c.column.as_str()).collect();
    assert!(col_names.contains(&"customer_id"));
    assert!(col_names.contains(&"first_name"));
    assert!(col_names.contains(&"order_count"));
    assert!(col_names.contains(&"lifetime_value"));
}

#[test]
fn test_order_enriched_nested_cte_star() {
    // 3-level nested CTE: base_orders -> with_payments -> final, all using SELECT *
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "order_enriched");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 5);

    // order_id should trace through the CTE chain to stg_orders.
    // The fixture references the table as "shop"."main"."stg_orders" (3-part name),
    // so source_name is the fully-qualified form.
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert!(!order_id.sources.is_empty(), "order_id should have sources");
    assert_eq!(order_id.sources[0].column, "order_id");
    assert_eq!(order_id.sources[0].table, "shop.main.stg_orders");

    // amount should trace to stg_payments (via alias "p")
    let amount = result
        .columns
        .iter()
        .find(|c| c.column == "amount")
        .unwrap();
    assert!(!amount.sources.is_empty(), "amount should have sources");
    assert_eq!(amount.sources[0].column, "amount");

    // All columns should have non-empty source tables
    for entry in &result.columns {
        for src in &entry.sources {
            assert!(
                !src.table.is_empty(),
                "column '{}' has empty table for source '{}'",
                entry.column,
                src.column
            );
        }
    }
}

#[test]
fn test_source_table_not_empty() {
    // Verify that leaf sources have non-empty table names
    let manifest = load_fixture_manifest();

    for model in ["stg_orders", "orders"] {
        let result = compute_column_lineage(&manifest, model);
        for entry in &result.columns {
            for src in &entry.sources {
                assert!(
                    !src.table.is_empty(),
                    "model '{}' column '{}' has empty table for source column '{}'",
                    model,
                    entry.column,
                    src.column
                );
            }
        }
    }
}

// --- YAML column completion tests ---

#[test]
fn test_yaml_columns_supplement_partial_sql_inference() {
    // stg_orders_passthrough: "SELECT id, * FROM raw.orders"
    //   SQL inference only captures "id" (the star is unresolvable without schema).
    //   YAML defines: id, user_id, order_date, status.
    //
    // mart_yaml_star: "WITH source AS (SELECT * FROM stg_orders_passthrough) SELECT * FROM source"
    //   Expanding this CTE star requires knowing stg_orders_passthrough's columns.
    //   With YAML+SQL merge in resolve_node_columns, the schema has all 4 columns,
    //   enabling full star expansion and lineage tracing.
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "mart_yaml_star");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(
        result.columns.len(),
        4,
        "should resolve all 4 YAML columns, got: {:?}",
        result.columns.iter().map(|c| &c.column).collect::<Vec<_>>()
    );

    // All columns should have sources tracing to stg_orders_passthrough
    for entry in &result.columns {
        assert!(
            !entry.sources.is_empty(),
            "column '{}' should have sources",
            entry.column
        );
    }
}

// --- Cross-model lineage integration tests ---

#[test]
fn test_cross_model_orders_traces_to_raw_sources() {
    // orders depends on stg_orders + stg_payments which depend on raw sources.
    // Cross-model should trace through to raw source columns.
    let manifest = load_fixture_manifest();
    let result = compute_cross_model_column_lineage(&manifest, "orders");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.columns.len(), 6);

    // order_id: orders → stg_orders → raw.orders.id (renamed via stg_orders)
    let order_id = result
        .columns
        .iter()
        .find(|c| c.column == "order_id")
        .unwrap();
    assert!(
        order_id.sources.iter().any(|s| s.column == "id"),
        "order_id should trace to raw source's 'id' column, got: {:?}",
        order_id.sources
    );

    // total_amount: orders → stg_payments.amount → raw.payments.amount
    let total_amount = result
        .columns
        .iter()
        .find(|c| c.column == "total_amount")
        .unwrap();
    assert!(
        total_amount.sources.iter().any(|s| s.column == "amount"),
        "total_amount should trace to raw payments.amount, got: {:?}",
        total_amount.sources
    );

    // All sources should be raw tables (not intermediate models)
    for entry in &result.columns {
        for src in &entry.sources {
            assert!(
                !src.table.is_empty(),
                "column '{}' has empty source table",
                entry.column
            );
            // Source tables should not be stg_ models (those are intermediate)
            assert!(
                !src.table.contains("stg_"),
                "column '{}' still references intermediate model '{}' instead of raw source",
                entry.column,
                src.table
            );
        }
    }
}

#[test]
fn test_cross_model_stg_orders_unchanged() {
    // stg_orders only depends on raw sources, so cross-model should give same result
    let manifest = load_fixture_manifest();
    let single = compute_column_lineage(&manifest, "stg_orders");
    let cross = compute_cross_model_column_lineage(&manifest, "stg_orders");

    assert_eq!(single.columns.len(), cross.columns.len());
    for (s, c) in single.columns.iter().zip(cross.columns.iter()) {
        assert_eq!(s.column, c.column);
        assert_eq!(
            s.sources.len(),
            c.sources.len(),
            "column '{}' source count differs",
            s.column
        );
    }
}

#[test]
fn test_cross_model_customers_three_levels() {
    // customers → orders → stg_orders/stg_payments → raw sources
    // This tests 3-level deep tracing
    let manifest = load_fixture_manifest();
    let result = compute_cross_model_column_lineage(&manifest, "customers");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // All sources should reach raw tables
    for entry in &result.columns {
        for src in &entry.sources {
            assert!(
                !src.table.is_empty(),
                "column '{}' has empty source table",
                entry.column
            );
        }
    }
}
