#![cfg(feature = "column-lineage")]

use dlin_core::graph::column_lineage::{
    ColumnLineageAnalysis, ColumnLineageCache, ColumnSource, DlinDialect, ModelColumnLineage,
    OutputSlot,
};
use std::path::PathBuf;

trait SlotsExt {
    fn slots(&self) -> &[OutputSlot<ColumnSource>];
}

impl SlotsExt for ModelColumnLineage {
    fn slots(&self) -> &[OutputSlot<ColumnSource>] {
        self.output
            .as_ref()
            .map_or(&[], |output| output.columns.as_slice())
    }
}

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
    assert_eq!(result.slots().len(), 4);

    // order_id is renamed from id
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "order_id")
        .unwrap();
    assert_eq!(
        order_id.lineage.known()[0].column.value,
        "id",
        "order_id should trace to raw.orders.id"
    );

    // customer_id is renamed from user_id
    let customer_id = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "customer_id")
        .unwrap();
    assert_eq!(customer_id.lineage.known()[0].column.value, "user_id");

    // Passthrough columns
    let order_date = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "order_date")
        .unwrap();
    assert_eq!(order_date.lineage.known()[0].column.value, "order_date");
}

#[test]
fn test_orders_cte_star_with_schema_and_join() {
    // orders model: CTEs with SELECT * FROM 3-part qualified tables, then JOIN
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "orders");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.slots().len(), 6);

    // order_id from stg_orders
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "order_id")
        .unwrap();
    assert!(
        !order_id.lineage.known().is_empty(),
        "order_id should have sources"
    );
    assert_eq!(order_id.lineage.known()[0].column.value, "order_id");

    // total_amount renamed from stg_payments.amount
    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "total_amount")
        .unwrap();
    assert!(
        !total_amount.lineage.known().is_empty(),
        "total_amount should have sources"
    );
    assert_eq!(total_amount.lineage.known()[0].column.value, "amount");
}

#[test]
fn test_customers_sql_inference_without_yaml_columns() {
    // customers model has no YAML columns — columns should be inferred from SQL
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "customers");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    // Should infer: customer_id, first_name, last_name, email, order_count, lifetime_value
    assert_eq!(result.slots().len(), 6, "should infer 6 columns from SQL");

    let col_names: Vec<String> = result.slots().iter().map(|c| c.name.to_string()).collect();
    assert!(col_names.iter().any(|name| name == "customer_id"));
    assert!(col_names.iter().any(|name| name == "first_name"));
    assert!(col_names.iter().any(|name| name == "order_count"));
    assert!(col_names.iter().any(|name| name == "lifetime_value"));
}

#[test]
fn test_order_enriched_nested_cte_star() {
    // 3-level nested CTE: base_orders -> with_payments -> final, all using SELECT *
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "order_enriched");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.slots().len(), 5);

    // order_id should trace through the CTE chain to stg_orders.
    // The fixture references the table as "shop"."main"."stg_orders" (3-part name),
    // so source_name is the fully-qualified form.
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "order_id")
        .unwrap();
    assert!(
        !order_id.lineage.known().is_empty(),
        "order_id should have sources"
    );
    assert_eq!(order_id.lineage.known()[0].column.value, "order_id");
    assert_eq!(order_id.lineage.known()[0].table, "shop.main.stg_orders");

    // amount should trace to stg_payments (via alias "p")
    let amount = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "amount")
        .unwrap();
    assert!(
        !amount.lineage.known().is_empty(),
        "amount should have sources"
    );
    assert_eq!(amount.lineage.known()[0].column.value, "amount");

    // All columns should have non-empty source tables
    for entry in result.slots() {
        for src in entry.lineage.known() {
            assert!(
                !src.table.is_empty(),
                "column '{}' has empty table for source '{}'",
                entry.name,
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
        for entry in result.slots() {
            for src in entry.lineage.known() {
                assert!(
                    !src.table.is_empty(),
                    "model '{}' column '{}' has empty table for source column '{}'",
                    model,
                    entry.name,
                    src.column
                );
            }
        }
    }
}

// --- Dependency schema tests ---

#[test]
fn test_dependency_sql_output_is_not_merged_with_yaml_columns() {
    // stg_orders_passthrough: `select id, * from raw.orders`, where the
    //   source declares id, user_id, order_date and status in YAML. The model
    //   really outputs `id` twice: once explicitly and once from the star.
    //   Its own YAML lists `id` once, but the YAML declaration is a separate
    //   record and is never merged into what the model provides downstream.
    //
    // mart_yaml_star: `with source as (select * from stg_orders_passthrough)
    //   select * from source`
    //   The star expands to the SQL output of stg_orders_passthrough, so the
    //   duplicated `id` stays ambiguous while the other columns are traced.
    let manifest = load_fixture_manifest();
    let result = compute_column_lineage(&manifest, "mart_yaml_star");

    // The star expands to every SQL output of the passthrough model, in order,
    // so the duplicated `id` is two outputs here. Each reads `id` of the
    // passthrough model; at this level nothing tells the two apart.
    let names: Vec<String> = result.slots().iter().map(|c| c.name.to_string()).collect();
    assert_eq!(names, ["id", "id", "user_id", "order_date", "status"]);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    for entry in result.slots() {
        assert_eq!(entry.lineage.known().len(), 1, "column '{}'", entry.name);
        assert_eq!(
            entry.lineage.known()[0].column.value,
            entry.name.to_string()
        );
        assert_eq!(
            entry.lineage.known()[0].table,
            "shop.main.stg_orders_passthrough"
        );
    }
    assert_eq!(result.value_complete_columns, 5);

    // Across the model boundary the reference to `id` matches two outputs of
    // the passthrough model, so neither is a proven source: the output is
    // partial, with the candidates listed in its unresolved contribution.
    let cross = compute_cross_model_column_lineage(&manifest, "mart_yaml_star");
    let ids: Vec<_> = cross
        .slots()
        .iter()
        .filter(|slot| slot.name.to_string() == "id")
        .collect();
    assert_eq!(ids.len(), 2);
    for slot in ids {
        assert!(slot.lineage.is_partial());
        let [contribution] = slot.lineage.unresolved() else {
            panic!("expected one unresolved contribution: {slot:?}");
        };
        assert!(matches!(
            &contribution.reason,
            dlin_core::graph::column_lineage::UnresolvedReason::AmbiguousOutput { candidates, .. }
                if candidates.len() == 2
        ));
        assert!(slot.lineage.known().is_empty());
    }
    assert_eq!(cross.value_partial_columns, 2);
}

// --- Cross-model lineage integration tests ---

#[test]
fn test_cross_model_orders_traces_to_raw_sources() {
    // orders depends on stg_orders + stg_payments which depend on raw sources.
    // Cross-model should trace through to raw source columns.
    let manifest = load_fixture_manifest();
    let result = compute_cross_model_column_lineage(&manifest, "orders");

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.slots().len(), 6);

    // order_id: orders → stg_orders → raw.orders.id (renamed via stg_orders)
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "order_id")
        .unwrap();
    assert!(
        order_id
            .lineage
            .known()
            .iter()
            .any(|s| s.column.value == "id"),
        "order_id should trace to raw source's 'id' column, got: {:?}",
        order_id.lineage.known()
    );

    // total_amount: orders → stg_payments.amount → raw.payments.amount
    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.name.to_string() == "total_amount")
        .unwrap();
    assert!(
        total_amount
            .lineage
            .known()
            .iter()
            .any(|s| s.column.value == "amount"),
        "total_amount should trace to raw payments.amount, got: {:?}",
        total_amount.lineage.known()
    );

    // All sources should be raw tables (not intermediate models)
    for entry in result.slots() {
        for src in entry.lineage.known() {
            assert!(
                !src.table.is_empty(),
                "column '{}' has empty source table",
                entry.name
            );
            // Source tables should not be stg_ models (those are intermediate)
            assert!(
                !src.table.contains("stg_"),
                "column '{}' still references intermediate model '{}' instead of raw source",
                entry.name,
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

    assert_eq!(single.slots().len(), cross.slots().len());
    for (s, c) in single.slots().iter().zip(cross.slots().iter()) {
        assert_eq!(s.name, c.name);
        assert_eq!(
            s.lineage.known().len(),
            c.lineage.known().len(),
            "column '{}' source count differs",
            s.name
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
    for entry in result.slots() {
        for src in entry.lineage.known() {
            assert!(
                !src.table.is_empty(),
                "column '{}' has empty source table",
                entry.name
            );
        }
    }
}
