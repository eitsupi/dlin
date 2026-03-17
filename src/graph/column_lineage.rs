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

    // Get column names from manifest (YAML-defined columns)
    let column_names: Vec<String> = {
        let mut names: Vec<String> = node.columns.keys().cloned().collect();
        names.sort();
        names
    };

    if column_names.is_empty() {
        return ModelColumnLineage {
            model: model_name.to_string(),
            columns: vec![],
            errors: vec![format!(
                "model '{}' has no columns defined in manifest (add column definitions to YAML)",
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

    for col_name in &column_names {
        // Try with schema first for better column resolution, fall back to without
        let lineage_result = if let Some(ref s) = schema {
            polyglot_sql::lineage::lineage_with_schema(
                col_name,
                &expr,
                Some(s as &dyn polyglot_sql::Schema),
                None,
                false,
            )
            .or_else(|_| polyglot_sql::lineage::lineage(col_name, &expr, None, false))
        } else {
            polyglot_sql::lineage::lineage(col_name, &expr, None, false)
        };

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
            if !dep_node.columns.is_empty() {
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = dep_node
                    .columns
                    .keys()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();
                // Use the table name as it appears in compiled SQL
                // dbt replaces ref('stg_orders') with `project`.`stg_orders` or just the table name
                let table_name = &dep_node.name;
                if schema.add_table(table_name, &cols, None).is_ok() {
                    has_entries = true;
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
                // Sources appear as `schema`.`table` in compiled SQL
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
    fn test_no_columns_defined() {
        let mut manifest = make_test_manifest();
        manifest.nodes.get_mut("model.proj.stg_orders").unwrap().columns.clear();
        let result = compute_column_lineage(&manifest, "stg_orders");

        assert!(result.columns.is_empty());
        assert!(result.errors[0].contains("no columns defined"));
    }

    #[test]
    fn test_cte_select_star_limitation() {
        // polyglot-sql v0.1.15 cannot resolve columns through CTE + SELECT *.
        // This is a known limitation: the lineage function cannot expand * from a CTE
        // without external schema information.
        let sql = r#"with renamed as (select id as customer_id from source) select * from renamed"#;
        let expr = polyglot_sql::parse_one(sql, polyglot_sql::DialectType::Generic).unwrap();
        let result = polyglot_sql::lineage::lineage("customer_id", &expr, None, false);
        assert!(result.is_err(), "CTE + SELECT * is a known limitation");

        // Direct SELECT (no CTE wrapping) works fine
        let sql2 = "select id as customer_id from source";
        let expr2 = polyglot_sql::parse_one(sql2, polyglot_sql::DialectType::Generic).unwrap();
        assert!(polyglot_sql::lineage::lineage("customer_id", &expr2, None, false).is_ok());
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
