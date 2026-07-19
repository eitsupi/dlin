use std::collections::HashMap;

use super::cache::{CACHE_DIR, COLUMN_LINEAGE_CACHE_FILENAME, ColumnLineageCacheFile};
use super::cross_model::normalize_table_name;
use super::impact::build_downstream_model_map;
use super::single_model::format_lineage_error;
use super::*;
use crate::parser::manifest::{
    DependsOn, ManifestColumn, ManifestConfig, ManifestNode, ManifestSource,
};
use polyglot_sql::Schema;

/// Build a minimal manifest for testing column lineage.
fn make_test_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    // stg_orders: SELECT id as order_id, user_id as customer_id, order_date, status FROM raw.orders
    let mut stg_orders_cols = HashMap::new();
    for name in ["order_id", "customer_id", "order_date", "status"] {
        stg_orders_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_orders".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_orders".to_string(),
            name: "stg_orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_orders_cols,
            compiled_code: Some(
                "select id as order_id, user_id as customer_id, order_date, status from raw.orders"
                    .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // orders: SELECT o.order_id, o.customer_id, p.amount as total_amount FROM stg_orders o LEFT JOIN stg_payments p ON o.order_id = p.order_id
    let mut orders_cols = HashMap::new();
    for name in ["order_id", "customer_id", "total_amount"] {
        orders_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert("model.proj.orders".to_string(), ManifestNode {
            unique_id: "model.proj.orders".to_string(),
            name: "orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![
                "model.proj.stg_orders".to_string(),
                "model.proj.stg_payments".to_string(),
            ] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: orders_cols,
            compiled_code: Some("select o.order_id, o.customer_id, p.amount as total_amount from stg_orders o left join stg_payments p on o.order_id = p.order_id".to_string()),
            database: None,
            schema: None,
        });

    // stg_payments (upstream, for schema)
    let mut stg_payments_cols = HashMap::new();
    for name in ["payment_id", "order_id", "amount", "payment_method"] {
        stg_payments_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_payments".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_payments".to_string(),
            name: "stg_payments".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_payments_cols,
            compiled_code: Some(
                "select id as payment_id, order_id, amount, payment_method from raw.payments"
                    .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // Source: raw.orders
    let mut source_cols = HashMap::new();
    for name in ["id", "user_id", "order_date", "status"] {
        source_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    let mut sources = HashMap::new();
    sources.insert(
        "source.proj.raw.orders".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.orders".to_string(),
            name: "orders".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: source_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    Manifest {
        nodes,
        sources,
        exposures: HashMap::new(),
        ..Default::default()
    }
}

fn make_cross_model_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    // Source: raw.orders
    let mut raw_orders_cols = HashMap::new();
    for name in ["id", "user_id", "order_date", "status"] {
        raw_orders_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    let mut sources = HashMap::new();
    sources.insert(
        "source.proj.raw.orders".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.orders".to_string(),
            name: "orders".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: raw_orders_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // Source: raw.payments
    let mut raw_payments_cols = HashMap::new();
    for name in ["id", "order_id", "amount", "payment_method"] {
        raw_payments_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.payments".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.payments".to_string(),
            name: "payments".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: raw_payments_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // stg_orders: renames id→order_id, user_id→customer_id
    let mut stg_orders_cols = HashMap::new();
    for name in ["order_id", "customer_id", "order_date", "status"] {
        stg_orders_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_orders".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_orders".to_string(),
            name: "stg_orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_orders_cols,
            compiled_code: Some(
                "select id as order_id, user_id as customer_id, order_date, status from orders"
                    .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // stg_payments: renames id→payment_id
    let mut stg_payments_cols = HashMap::new();
    for name in ["payment_id", "order_id", "amount", "payment_method"] {
        stg_payments_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.stg_payments".to_string(),
        ManifestNode {
            unique_id: "model.proj.stg_payments".to_string(),
            name: "stg_payments".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.payments".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: stg_payments_cols,
            compiled_code: Some(
                "select id as payment_id, order_id, amount, payment_method from payments"
                    .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // orders: joins stg_orders + stg_payments (CTE pattern like real dbt compiled SQL)
    let mut orders_cols = HashMap::new();
    for name in ["order_id", "customer_id", "total_amount"] {
        orders_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.orders".to_string(),
        ManifestNode {
            unique_id: "model.proj.orders".to_string(),
            name: "orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec![
                    "model.proj.stg_orders".to_string(),
                    "model.proj.stg_payments".to_string(),
                ],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: orders_cols,
            compiled_code: Some(
                concat!(
                    "with stg_orders as (select * from stg_orders), ",
                    "stg_payments as (select * from stg_payments) ",
                    "select stg_orders.order_id, stg_orders.customer_id, ",
                    "stg_payments.amount as total_amount ",
                    "from stg_orders left join stg_payments ",
                    "on stg_orders.order_id = stg_payments.order_id"
                )
                .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // customers: aggregates from orders model (CTE pattern)
    let mut customers_cols = HashMap::new();
    for name in ["customer_id", "order_count"] {
        customers_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.customers".to_string(),
        ManifestNode {
            unique_id: "model.proj.customers".to_string(),
            name: "customers".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: customers_cols,
            compiled_code: Some(
                concat!(
                    "with orders as (select * from orders) ",
                    "select customer_id, count(*) as order_count from orders group by customer_id"
                )
                .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    Manifest {
        nodes,
        sources,
        exposures: HashMap::new(),
        ..Default::default()
    }
}

fn make_duplicate_name_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    // stg_orders in pkg_a (the upstream model being traced)
    nodes.insert(
        "model.pkg_a.stg_orders".to_string(),
        ManifestNode {
            unique_id: "model.pkg_a.stg_orders".to_string(),
            name: "stg_orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: {
                let mut cols = HashMap::new();
                cols.insert(
                    "customer_id".to_string(),
                    ManifestColumn {
                        name: "customer_id".to_string(),
                    },
                );
                cols
            },
            compiled_code: Some("select customer_id from raw_customers".to_string()),
            database: None,
            schema: None,
        },
    );

    // customers in pkg_a — SELECT customer_id FROM stg_orders
    nodes.insert(
        "model.pkg_a.customers".to_string(),
        ManifestNode {
            unique_id: "model.pkg_a.customers".to_string(),
            name: "customers".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.pkg_a.stg_orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: {
                let mut cols = HashMap::new();
                cols.insert(
                    "customer_id".to_string(),
                    ManifestColumn {
                        name: "customer_id".to_string(),
                    },
                );
                cols
            },
            compiled_code: Some("select customer_id from stg_orders".to_string()),
            database: None,
            schema: None,
        },
    );

    // customers in pkg_b — same name, different package, same SQL pattern
    nodes.insert(
        "model.pkg_b.customers".to_string(),
        ManifestNode {
            unique_id: "model.pkg_b.customers".to_string(),
            name: "customers".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.pkg_a.stg_orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: {
                let mut cols = HashMap::new();
                cols.insert(
                    "customer_id".to_string(),
                    ManifestColumn {
                        name: "customer_id".to_string(),
                    },
                );
                cols
            },
            compiled_code: Some("select customer_id from stg_orders".to_string()),
            database: None,
            schema: None,
        },
    );

    Manifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        ..Default::default()
    }
}

#[test]
fn test_column_impact_duplicate_model_names_across_packages() {
    // Two packages each have a "customers" model depending on the same stg_orders.
    // Both should appear in impacted_columns with distinct unique_ids.
    let manifest = make_duplicate_name_manifest();
    let result = compute_column_impact(
        &manifest,
        "stg_orders",
        "customer_id",
        DialectType::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let unique_ids: Vec<&str> = result
        .impacted_columns
        .iter()
        .filter(|ic| ic.column == "customer_id")
        .map(|ic| ic.unique_id.as_str())
        .collect();

    assert!(
        unique_ids.contains(&"model.pkg_a.customers"),
        "pkg_a customers should be impacted, got unique_ids: {:?}",
        unique_ids
    );
    assert!(
        unique_ids.contains(&"model.pkg_b.customers"),
        "pkg_b customers should be impacted, got unique_ids: {:?}",
        unique_ids
    );
    assert_eq!(
        unique_ids.len(),
        2,
        "both same-named models should appear separately, got: {:?}",
        result.impacted_columns
    );
}

/// Build a diamond DAG manifest where different columns flow through a shared model:
///
///   raw_data (x, y)
///      |
///   shared (x, y)   -- passes both columns through
///    /        \
///  left(x)  right(y)  -- each uses a different column from shared
///    \        /
///   diamond_out(lx, ry) -- combines left.x and right.y
fn make_diamond_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    // raw_data: source with columns x, y
    let mut raw_cols = HashMap::new();
    for name in ["x", "y"] {
        raw_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.raw_data".to_string(),
        ManifestNode {
            unique_id: "model.proj.raw_data".to_string(),
            name: "raw_data".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: raw_cols,
            compiled_code: Some("select x, y from source_table".to_string()),
            database: None,
            schema: None,
        },
    );

    // shared: passes x and y through from raw_data
    let mut shared_cols = HashMap::new();
    for name in ["x", "y"] {
        shared_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.shared".to_string(),
        ManifestNode {
            unique_id: "model.proj.shared".to_string(),
            name: "shared".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.raw_data".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: shared_cols,
            compiled_code: Some("select x, y from raw_data".to_string()),
            database: None,
            schema: None,
        },
    );

    // left: uses column x from shared
    let mut left_cols = HashMap::new();
    left_cols.insert(
        "x".to_string(),
        ManifestColumn {
            name: "x".to_string(),
        },
    );
    nodes.insert(
        "model.proj.left_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.left_model".to_string(),
            name: "left_model".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.shared".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: left_cols,
            compiled_code: Some("select x from shared".to_string()),
            database: None,
            schema: None,
        },
    );

    // right: uses column y from shared
    let mut right_cols = HashMap::new();
    right_cols.insert(
        "y".to_string(),
        ManifestColumn {
            name: "y".to_string(),
        },
    );
    nodes.insert(
        "model.proj.right_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.right_model".to_string(),
            name: "right_model".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.shared".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: right_cols,
            compiled_code: Some("select y from shared".to_string()),
            database: None,
            schema: None,
        },
    );

    // diamond_out: combines left.x and right.y
    let mut out_cols = HashMap::new();
    for name in ["lx", "ry"] {
        out_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.diamond_out".to_string(),
        ManifestNode {
            unique_id: "model.proj.diamond_out".to_string(),
            name: "diamond_out".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec![
                    "model.proj.left_model".to_string(),
                    "model.proj.right_model".to_string(),
                ],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: out_cols,
            compiled_code: Some(
                "select l.x as lx, r.y as ry from left_model l join right_model r on 1=1"
                    .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    Manifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        ..Default::default()
    }
}

/// Build a manifest for testing transformation type classification.
pub(super) fn make_transformation_manifest() -> Manifest {
    let mut nodes = HashMap::new();
    let mut sources = HashMap::new();

    let mut raw_cols = HashMap::new();
    for name in ["id", "status", "name"] {
        raw_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    sources.insert(
        "source.proj.raw.orders".to_string(),
        ManifestSource {
            unique_id: "source.proj.raw.orders".to_string(),
            name: "orders".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: raw_cols,
            database: None,
            schema: None,
            identifier: None,
        },
    );

    // scalar_funcs: tests Bug 1 — general function calls (UPPER, CONCAT, generic Function)
    let mut scalar_cols = HashMap::new();
    for name in ["col_upper", "col_concat", "col_coalesce"] {
        scalar_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.scalar_funcs".to_string(),
        ManifestNode {
            unique_id: "model.proj.scalar_funcs".to_string(),
            name: "scalar_funcs".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: scalar_cols,
            compiled_code: Some(
                concat!(
                    "select",
                    " UPPER(status) as col_upper,",
                    " CONCAT(status, '_x') as col_concat,",
                    " COALESCE(status, 'default') as col_coalesce",
                    " from orders"
                )
                .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // passthrough_upper: tests Bug 2 — CTE computes UPPER, next SELECT passes through
    let mut pt_upper_cols = HashMap::new();
    for name in ["id", "status_upper"] {
        pt_upper_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.passthrough_upper".to_string(),
        ManifestNode {
            unique_id: "model.proj.passthrough_upper".to_string(),
            name: "passthrough_upper".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: pt_upper_cols,
            compiled_code: Some(
                concat!(
                    "with step1 as (",
                    " select id, UPPER(status) as status_upper from orders",
                    ")",
                    " select id, status_upper from step1"
                )
                .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    // passthrough_coalesce: tests Bug 2 — CTE computes COALESCE, next SELECT passes through
    let mut pt_coalesce_cols = HashMap::new();
    for name in ["id", "status_coalesced"] {
        pt_coalesce_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.passthrough_coalesce".to_string(),
        ManifestNode {
            unique_id: "model.proj.passthrough_coalesce".to_string(),
            name: "passthrough_coalesce".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["source.proj.raw.orders".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: pt_coalesce_cols,
            compiled_code: Some(
                concat!(
                    "with step1 as (",
                    " select id, COALESCE(status, 'default') as status_coalesced from orders",
                    ")",
                    " select id, status_coalesced from step1"
                )
                .to_string(),
            ),
            database: None,
            schema: None,
        },
    );

    Manifest {
        nodes,
        sources,
        exposures: HashMap::new(),
        ..Default::default()
    }
}

/// Build a manifest for testing reconverging DAG where the same column name
/// flows through two separate paths before merging back:
///
///   source_model (x)
///        |
///   +----+----+
///   |         |
/// left(x)  right(x)   -- each passes x through from source_model
///   |         |
///   +----+----+
///        |
///    final(x)          -- x from both left and right via COALESCE
///        |
///     mart(x)
///        |
///  dashboard(x)
///
/// The path-local cycle guard allows final/mart/dashboard to appear once per
/// upstream path rather than being deduplicated to a single entry.
pub(super) fn make_reconverging_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    let make_node = |uid: &str, name: &str, deps: Vec<String>, sql: &str| ManifestNode {
        unique_id: uid.to_string(),
        name: name.to_string(),
        resource_type: "model".to_string(),
        depends_on: DependsOn { nodes: deps },
        config: ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: {
            let mut cols = HashMap::new();
            cols.insert(
                "x".to_string(),
                ManifestColumn {
                    name: "x".to_string(),
                },
            );
            cols
        },
        compiled_code: Some(sql.to_string()),
        database: None,
        schema: None,
    };

    nodes.insert(
        "model.proj.source_model".to_string(),
        make_node(
            "model.proj.source_model",
            "source_model",
            vec![],
            "select x from raw_table",
        ),
    );
    nodes.insert(
        "model.proj.left_model".to_string(),
        make_node(
            "model.proj.left_model",
            "left_model",
            vec!["model.proj.source_model".to_string()],
            "select x from source_model",
        ),
    );
    nodes.insert(
        "model.proj.right_model".to_string(),
        make_node(
            "model.proj.right_model",
            "right_model",
            vec!["model.proj.source_model".to_string()],
            "select x from source_model",
        ),
    );
    nodes.insert(
        "model.proj.final_model".to_string(),
        make_node(
            "model.proj.final_model",
            "final_model",
            vec![
                "model.proj.left_model".to_string(),
                "model.proj.right_model".to_string(),
            ],
            "select COALESCE(l.x, r.x) as x from left_model l join right_model r on 1=1",
        ),
    );
    nodes.insert(
        "model.proj.mart_model".to_string(),
        make_node(
            "model.proj.mart_model",
            "mart_model",
            vec!["model.proj.final_model".to_string()],
            "select x from final_model",
        ),
    );
    nodes.insert(
        "model.proj.dashboard_model".to_string(),
        make_node(
            "model.proj.dashboard_model",
            "dashboard_model",
            vec!["model.proj.mart_model".to_string()],
            "select x from mart_model",
        ),
    );

    Manifest {
        nodes,
        sources: HashMap::new(),
        exposures: HashMap::new(),
        ..Default::default()
    }
}

mod cache;
mod core_basic;
mod core_cross_model;
mod core_star;
mod core_util;
mod impact;
