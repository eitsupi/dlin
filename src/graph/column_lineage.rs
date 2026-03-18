use polyglot_sql::Schema;
use serde::Serialize;

use crate::parser::manifest::Manifest;

/// Column lineage result for a single model
#[derive(Debug, Serialize)]
pub struct ModelColumnLineage {
    pub model: String,
    pub columns: Vec<ColumnLineageEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<String>,
}

/// Lineage for a single output column
#[derive(Debug, Serialize)]
pub struct ColumnLineageEntry {
    pub column: String,
    pub sources: Vec<ColumnSource>,
}

/// A source column reference
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash)]
pub struct ColumnSource {
    /// Source table/model name as it appears in SQL (e.g. "stg_orders", "`raw`.`orders`")
    pub table: String,
    /// Source column name
    pub column: String,
}

/// Compute column-level lineage for a model using polyglot-sql.
///
/// Takes the manifest and a model name (short label like "orders"),
/// and returns the column lineage for that model.
pub fn compute_column_lineage(manifest: &Manifest, model_name: &str) -> ModelColumnLineage {
    // Find the node in the manifest
    let node = manifest
        .nodes
        .values()
        .find(|n| n.name == model_name && n.resource_type == "model");

    let node = match node {
        Some(n) => n,
        None => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                columns: vec![],
                errors: vec![format!("model '{}' not found in manifest", model_name)],
            };
        }
    };

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                columns: vec![],
                errors: vec![format!(
                    "model '{}' has no compiled_code (run `dbt compile` first)",
                    model_name
                )],
            };
        }
    };

    // Get column names: prefer YAML-defined columns, fall back to SQL inference
    let column_names: Vec<String> = {
        let mut names: Vec<String> = node.columns.keys().cloned().collect();
        if names.is_empty() {
            // Infer from compiled SQL
            names = infer_output_columns(compiled_code);
        }
        names.sort();
        names
    };

    if column_names.is_empty() {
        return ModelColumnLineage {
            model: model_name.to_string(),
            columns: vec![],
            errors: vec![format!(
                "model '{}': could not determine output columns (no YAML columns and SQL inference failed)",
                model_name
            )],
        };
    }

    // Parse the compiled SQL
    let expr = match polyglot_sql::parse_one(compiled_code, polyglot_sql::DialectType::Generic) {
        Ok(e) => e,
        Err(e) => {
            return ModelColumnLineage {
                model: model_name.to_string(),
                columns: vec![],
                errors: vec![format!("failed to parse SQL for '{}': {}", model_name, e)],
            };
        }
    };

    // Build schema from manifest for better column resolution
    let schema = build_schema_from_manifest(manifest, node);

    let mut columns = Vec::new();
    let mut errors = Vec::new();

    // Pre-expand CTE stars using schema for external table column lookup.
    // This is done before lineage() because qualify_columns may fail on complex
    // CTEs with ambiguous column references.
    let mut expanded_expr = expr.clone();
    polyglot_sql::lineage::expand_cte_stars(
        &mut expanded_expr,
        schema.as_ref().map(|s| s as &dyn polyglot_sql::Schema),
    );

    for col_name in &column_names {
        // Try lineage without schema first (cheaper, no qualify_columns overhead),
        // then fall back to lineage_with_schema for better resolution.
        let lineage_result = polyglot_sql::lineage::lineage(col_name, &expanded_expr, None, false)
            .or_else(|_| {
                if let Some(ref s) = schema {
                    polyglot_sql::lineage::lineage_with_schema(
                        col_name,
                        &expanded_expr,
                        Some(s as &dyn polyglot_sql::Schema),
                        None,
                        false,
                    )
                } else {
                    Err(polyglot_sql::Error::internal(format!(
                        "column '{}' not found",
                        col_name
                    )))
                }
            });

        match lineage_result {
            Ok(lineage_node) => {
                let sources = extract_leaf_sources(&lineage_node);
                columns.push(ColumnLineageEntry {
                    column: col_name.clone(),
                    sources,
                });
            }
            Err(e) => {
                errors.push(format!("column '{}': {}", col_name, e));
            }
        }
    }

    ModelColumnLineage {
        model: model_name.to_string(),
        columns,
        errors,
    }
}

/// Build a MappingSchema from the manifest's upstream nodes for column qualification.
///
/// For each upstream dependency, columns are determined by:
/// 1. YAML-defined columns in the manifest (preferred)
/// 2. Inferring output columns from the upstream model's compiled SQL (fallback)
///
/// Tables are registered with their fully-qualified name (database.schema.name)
/// when database/schema info is available, so that references like
/// `"jaffle_shop"."main"."stg_orders"` can be resolved.
fn build_schema_from_manifest(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> Option<polyglot_sql::MappingSchema> {
    let mut schema = polyglot_sql::MappingSchema::new();
    let mut has_entries = false;

    // Add columns from upstream dependencies
    for dep_id in &node.depends_on.nodes {
        // Try as a node (model/seed/snapshot)
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let col_names = resolve_node_columns(dep_node);
            if !col_names.is_empty() {
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = col_names
                    .iter()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();

                // Register with fully-qualified name if database/schema available
                let fq_name = make_fq_table_name(
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    &dep_node.name,
                );
                if schema.add_table(&fq_name, &cols, None).is_ok() {
                    has_entries = true;
                }
                // Also register with short name for non-qualified references
                if fq_name != dep_node.name {
                    let _ = schema.add_table(&dep_node.name, &cols, None);
                }
            }
            continue;
        }

        // Try as a source
        if let Some(dep_source) = manifest.sources.get(dep_id) {
            if !dep_source.columns.is_empty() {
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = dep_source
                    .columns
                    .keys()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();
                let table_name = &dep_source.name;
                if schema.add_table(table_name, &cols, None).is_ok() {
                    has_entries = true;
                }
            }
        }
    }

    if has_entries {
        Some(schema)
    } else {
        None
    }
}

/// Resolve columns for a manifest node.
///
/// Prefers SQL inference (which gives the complete output column list) over YAML columns
/// (which may be incomplete). Falls back to YAML columns when compiled SQL is unavailable
/// or SQL inference fails.
fn resolve_node_columns(dep_node: &crate::parser::manifest::ManifestNode) -> Vec<String> {
    // Try SQL inference first — gives the complete column list
    if let Some(ref code) = dep_node.compiled_code {
        let inferred = infer_output_columns(code);
        if !inferred.is_empty() {
            return inferred;
        }
    }
    // Fall back to YAML-defined columns
    dep_node.columns.keys().cloned().collect()
}

/// Infer output column names from a model's compiled SQL by parsing it and extracting
/// the top-level SELECT column list. Handles CTE patterns by using lineage's
/// expand_cte_stars logic.
fn infer_output_columns(sql: &str) -> Vec<String> {
    let expr = match polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic) {
        Ok(e) => e,
        Err(_) => return vec![],
    };
    // Use polyglot-sql's extract_select_columns to get the output column names
    crate::parser::columns::extract_select_columns_from_expr(&expr)
}

/// Build a fully-qualified table name from optional database, schema, and table name.
fn make_fq_table_name(database: Option<&str>, schema: Option<&str>, name: &str) -> String {
    match (database, schema) {
        (Some(db), Some(s)) => format!("{}.{}.{}", db, s, name),
        (None, Some(s)) => format!("{}.{}", s, name),
        _ => name.to_string(),
    }
}

/// Walk the lineage tree and extract leaf-level source columns.
fn extract_leaf_sources(node: &polyglot_sql::lineage::LineageNode) -> Vec<ColumnSource> {
    let mut sources = Vec::new();
    collect_leaves(node, &mut sources);
    // Deduplicate
    sources.sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
    sources.dedup();
    sources
}

fn collect_leaves(node: &polyglot_sql::lineage::LineageNode, sources: &mut Vec<ColumnSource>) {
    if node.downstream.is_empty() {
        // Leaf node — this is a source column
        let name = &node.name;
        // Name is typically "table.column" or just "column"
        if let Some((table, column)) = name.rsplit_once('.') {
            sources.push(ColumnSource {
                table: table.to_string(),
                column: column.to_string(),
            });
        } else {
            sources.push(ColumnSource {
                table: String::new(),
                column: name.to_string(),
            });
        }
    } else {
        for child in &node.downstream {
            collect_leaves(child, sources);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use crate::parser::manifest::{ManifestNode, ManifestSource, ManifestColumn, DependsOn, ManifestConfig};

    /// Build a minimal manifest for testing column lineage.
    fn make_test_manifest() -> Manifest {
        let mut nodes = HashMap::new();

        // stg_orders: SELECT id as order_id, user_id as customer_id, order_date, status FROM raw.orders
        let mut stg_orders_cols = HashMap::new();
        for name in ["order_id", "customer_id", "order_date", "status"] {
            stg_orders_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.stg_orders".to_string(), ManifestNode {
            unique_id: "model.proj.stg_orders".to_string(),
            name: "stg_orders".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["source.proj.raw.orders".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_orders_cols,
            compiled_code: Some("select id as order_id, user_id as customer_id, order_date, status from raw.orders".to_string()),
            database: None,
            schema: None,
        });

        // orders: SELECT o.order_id, o.customer_id, p.amount as total_amount FROM stg_orders o LEFT JOIN stg_payments p ON o.order_id = p.order_id
        let mut orders_cols = HashMap::new();
        for name in ["order_id", "customer_id", "total_amount"] {
            orders_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
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
            columns: orders_cols,
            compiled_code: Some("select o.order_id, o.customer_id, p.amount as total_amount from stg_orders o left join stg_payments p on o.order_id = p.order_id".to_string()),
            database: None,
            schema: None,
        });

        // stg_payments (upstream, for schema)
        let mut stg_payments_cols = HashMap::new();
        for name in ["payment_id", "order_id", "amount", "payment_method"] {
            stg_payments_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.stg_payments".to_string(), ManifestNode {
            unique_id: "model.proj.stg_payments".to_string(),
            name: "stg_payments".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_payments_cols,
            compiled_code: Some("select id as payment_id, order_id, amount, payment_method from raw.payments".to_string()),
            database: None,
            schema: None,
        });

        // Source: raw.orders
        let mut source_cols = HashMap::new();
        for name in ["id", "user_id", "order_date", "status"] {
            source_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        let mut sources = HashMap::new();
        sources.insert("source.proj.raw.orders".to_string(), ManifestSource {
            unique_id: "source.proj.raw.orders".to_string(),
            name: "orders".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            columns: source_cols,
        });

        Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        }
    }

    #[test]
    fn test_rename_detection() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(&manifest, "stg_orders");

        assert_eq!(result.model, "stg_orders");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 4);

        // order_id comes from orders.id (renamed)
        let order_id = result.columns.iter().find(|c| c.column == "order_id").unwrap();
        assert!(!order_id.sources.is_empty(), "order_id should have sources");
        assert_eq!(order_id.sources[0].column, "id");

        // customer_id comes from orders.user_id (renamed)
        let customer_id = result.columns.iter().find(|c| c.column == "customer_id").unwrap();
        assert_eq!(customer_id.sources[0].column, "user_id");
    }

    #[test]
    fn test_join_lineage() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(&manifest, "orders");

        assert_eq!(result.model, "orders");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 3);

        // total_amount is aliased from p.amount
        let total_amount = result.columns.iter().find(|c| c.column == "total_amount").unwrap();
        assert!(!total_amount.sources.is_empty());
        assert_eq!(total_amount.sources[0].column, "amount");

        // order_id comes from o.order_id
        let order_id = result.columns.iter().find(|c| c.column == "order_id").unwrap();
        assert_eq!(order_id.sources[0].column, "order_id");
    }

    #[test]
    fn test_model_not_found() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(&manifest, "nonexistent");

        assert_eq!(result.columns.len(), 0);
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("not found"));
    }

    #[test]
    fn test_no_compiled_code() {
        let mut manifest = make_test_manifest();
        // Remove compiled_code from stg_orders
        manifest.nodes.get_mut("model.proj.stg_orders").unwrap().compiled_code = None;
        let result = compute_column_lineage(&manifest, "stg_orders");

        assert!(result.columns.is_empty());
        assert!(result.errors[0].contains("compiled_code"));
    }

    #[test]
    fn test_no_yaml_columns_uses_sql_inference() {
        // When YAML columns are empty, column names should be inferred from compiled SQL
        let mut manifest = make_test_manifest();
        manifest.nodes.get_mut("model.proj.stg_orders").unwrap().columns.clear();
        let result = compute_column_lineage(&manifest, "stg_orders");

        // SQL inference should find: customer_id, order_date, order_id, status
        assert_eq!(result.columns.len(), 4, "should infer 4 columns from SQL: {:?}", result.errors);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    }

    #[test]
    fn test_no_columns_and_no_sql() {
        // When YAML columns are empty AND compiled SQL cannot be parsed, error
        let mut manifest = make_test_manifest();
        let node = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
        node.columns.clear();
        node.compiled_code = Some("INVALID SQL %%%".to_string());
        let result = compute_column_lineage(&manifest, "stg_orders");

        assert!(result.columns.is_empty());
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("could not determine output columns"));
    }

    #[test]
    fn test_cte_select_star() {
        // CTE + SELECT * now works with the expand_cte_stars preprocessing
        let sql = r#"with renamed as (select id as customer_id from source) select * from renamed"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("customer_id", &expr, None, false);
        assert!(result.is_ok(), "CTE + SELECT * should work: {:?}", result.err());
        let node = result.unwrap();
        assert_eq!(node.name, "customer_id");
    }

    #[test]
    fn test_nested_cte_select_star() {
        // Nested CTE: cte2 references cte1 via SELECT *
        let sql = r#"
            with
                cte1 as (select id as order_id, amount from raw_orders),
                cte2 as (select * from cte1)
            select * from cte2
        "#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("order_id", &expr, None, false);
        assert!(result.is_ok(), "nested CTE + SELECT * should work: {:?}", result.err());
    }

    #[test]
    fn test_cte_select_star_in_manifest_model() {
        // Integration test: typical dbt pattern with CTE + SELECT *
        let mut manifest = make_test_manifest();
        let sql = r#"with renamed as (
            select
                id as order_id,
                user_id as customer_id,
                order_date,
                status
            from raw.orders
        )
        select * from renamed"#;
        manifest.nodes.get_mut("model.proj.stg_orders").unwrap().compiled_code = Some(sql.to_string());
        let result = compute_column_lineage(&manifest, "stg_orders");

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 4);

        let order_id = result.columns.iter().find(|c| c.column == "order_id").unwrap();
        assert_eq!(order_id.sources[0].column, "id");
    }

    #[test]
    fn test_schema_resolves_cte_star_from_external_table() {
        // Test that lineage_with_schema can resolve columns through CTEs that
        // reference external tables registered in the schema.
        let sql = r#"with
orders as (
    select * from stg_orders
),
enriched as (
    select orders.*, 'extra' as extra_col
    from orders
)
select * from enriched"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            ("order_id".to_string(), polyglot_sql::expressions::DataType::Unknown),
            ("customer_id".to_string(), polyglot_sql::expressions::DataType::Unknown),
            ("order_total".to_string(), polyglot_sql::expressions::DataType::Unknown),
        ];
        schema.add_table("stg_orders", &cols, None).unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(result.is_ok(), "should resolve order_id: {:?}", result.err());
    }

    #[test]
    fn test_schema_resolves_three_part_name() {
        // Test with fully-qualified 3-part table name as dbt generates
        let sql = r#"with
orders as (
    select * from "jaffle_shop"."main"."stg_orders"
)
select * from orders"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();

        let mut schema = polyglot_sql::MappingSchema::new();
        let cols = vec![
            ("order_id".to_string(), polyglot_sql::expressions::DataType::Unknown),
            ("customer_id".to_string(), polyglot_sql::expressions::DataType::Unknown),
        ];
        // Register with 3-part name
        schema.add_table("jaffle_shop.main.stg_orders", &cols, None).unwrap();

        let result = polyglot_sql::lineage::lineage_with_schema(
            "order_id",
            &expr,
            Some(&schema as &dyn polyglot_sql::Schema),
            None,
            false,
        );
        assert!(result.is_ok(), "should resolve order_id via 3-part name: {:?}", result.err());
    }

    #[test]
    fn test_json_serialization() {
        let manifest = make_test_manifest();
        let result = compute_column_lineage(&manifest, "stg_orders");
        let json = serde_json::to_string_pretty(&result).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["model"], "stg_orders");
        assert!(parsed["columns"].is_array());
    }

}
