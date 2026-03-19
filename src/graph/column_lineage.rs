use std::collections::{HashMap, HashSet};

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
        // When schema is available, prefer lineage_with_schema for proper table name
        // resolution (aliases like "o" are resolved to actual table names like "stg_orders").
        // Fall back to lineage without schema if lineage_with_schema fails.
        let lineage_result = if let Some(ref s) = schema {
            polyglot_sql::lineage::lineage_with_schema(
                col_name,
                &expanded_expr,
                Some(s as &dyn polyglot_sql::Schema),
                None,
                false,
            )
            .or_else(|_| {
                polyglot_sql::lineage::lineage(col_name, &expanded_expr, None, false)
            })
        } else {
            polyglot_sql::lineage::lineage(col_name, &expanded_expr, None, false)
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

/// Compute column-level lineage with cross-model chain tracking.
///
/// Like `compute_column_lineage`, but recursively follows source references
/// through upstream models until reaching dbt source tables (raw tables).
///
/// For example, if `orders.total_amount` traces to `stg_payments.amount`,
/// and `stg_payments.amount` traces to `raw.payments.amount`, the final result
/// will show `raw.payments.amount` as the ultimate source.
pub fn compute_cross_model_column_lineage(
    manifest: &Manifest,
    model_name: &str,
) -> ModelColumnLineage {
    let mut cache: HashMap<String, ModelColumnLineage> = HashMap::new();
    compute_cross_model_inner(manifest, model_name, &mut cache)
}

fn compute_cross_model_inner(
    manifest: &Manifest,
    model_name: &str,
    cache: &mut HashMap<String, ModelColumnLineage>,
) -> ModelColumnLineage {
    // Compute single-model lineage first
    let mut result = compute_column_lineage(manifest, model_name);

    // Build a mapping: table name (as appears in SQL output) → model name
    // for the current model's upstream dependencies
    let upstream_models = build_upstream_model_names(manifest, model_name);

    // For each column, resolve sources through upstream models
    for entry in &mut result.columns {
        let mut resolved_sources = Vec::new();
        let mut visited = HashSet::new();
        visited.insert(model_name.to_string());

        for source in &entry.sources {
            resolve_source_recursive(
                manifest,
                source,
                &upstream_models,
                &mut visited,
                &mut resolved_sources,
                &mut result.errors,
                cache,
            );
        }

        // Deduplicate and sort
        resolved_sources.sort_by(|a, b| (&a.table, &a.column).cmp(&(&b.table, &b.column)));
        resolved_sources.dedup();
        entry.sources = resolved_sources;
    }

    result
}

/// Build a mapping from table names (as they may appear in SQL lineage output)
/// to model names for upstream model dependencies.
fn build_upstream_model_names(manifest: &Manifest, model_name: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();

    let node = manifest
        .nodes
        .values()
        .find(|n| n.name == model_name && n.resource_type == "model");

    let node = match node {
        Some(n) => n,
        None => return map,
    };

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if dep_node.resource_type != "model" {
                continue;
            }
            // Register short name
            map.insert(dep_node.name.clone(), dep_node.name.clone());
            // Register FQ name (database.schema.name)
            let fq = make_fq_table_name(
                dep_node.database.as_deref(),
                dep_node.schema.as_deref(),
                &dep_node.name,
            );
            if fq != dep_node.name {
                map.insert(fq, dep_node.name.clone());
            }
        }
    }

    map
}

/// Normalize a table name by stripping quotes and extracting the short name.
///
/// Handles patterns like:
/// - `"jaffle_shop"."main"."stg_orders"` → `stg_orders`
/// - `` `raw`.`orders` `` → `orders`
/// - `stg_orders` → `stg_orders`
fn normalize_table_name(table: &str) -> String {
    let stripped: String = table.chars().filter(|c| *c != '"' && *c != '`').collect();
    stripped
        .rsplit('.')
        .next()
        .unwrap_or(&stripped)
        .to_string()
}

fn resolve_source_recursive(
    manifest: &Manifest,
    source: &ColumnSource,
    upstream_models: &HashMap<String, String>,
    visited: &mut HashSet<String>,
    resolved: &mut Vec<ColumnSource>,
    errors: &mut Vec<String>,
    cache: &mut HashMap<String, ModelColumnLineage>,
) {
    // Check if the source table matches an upstream model
    let model_name = upstream_models
        .get(&source.table)
        .or_else(|| {
            // Try normalized name (strip quotes, take last component)
            let normalized = normalize_table_name(&source.table);
            upstream_models.get(&normalized)
        })
        .cloned();

    let model_name = match model_name {
        Some(name) if !visited.contains(&name) => name,
        _ => {
            // Source is a raw table, a dbt source, or already visited (cycle) — leaf
            resolved.push(source.clone());
            return;
        }
    };

    // Get or compute the upstream model's lineage
    if !cache.contains_key(&model_name) {
        let upstream_result = compute_cross_model_inner(manifest, &model_name, cache);
        cache.insert(model_name.clone(), upstream_result);
    }
    let upstream_result = cache.get(&model_name).unwrap();

    // Propagate upstream errors
    for err in &upstream_result.errors {
        if !errors.contains(err) {
            errors.push(err.clone());
        }
    }

    // Find the matching column in the upstream model's lineage
    if let Some(col_entry) = upstream_result
        .columns
        .iter()
        .find(|c| c.column == source.column)
    {
        if col_entry.sources.is_empty() {
            // Upstream column has no sources — keep original
            resolved.push(source.clone());
        } else {
            // The upstream's sources are already fully resolved (cross-model)
            // because compute_cross_model_inner was used
            resolved.extend(col_entry.sources.iter().cloned());
        }
    } else {
        // Column not in precomputed lineage (e.g. not in YAML columns).
        // Try on-demand single-column lineage from the upstream model's SQL.
        let on_demand = compute_single_column_lineage(manifest, &model_name, &source.column);
        if on_demand.is_empty() {
            // Cannot resolve — keep as leaf
            resolved.push(source.clone());
        } else {
            // Recursively resolve the on-demand results through further upstream models
            let further_upstream = build_upstream_model_names(manifest, &model_name);
            for s in &on_demand {
                resolve_source_recursive(
                    manifest,
                    s,
                    &further_upstream,
                    visited,
                    resolved,
                    errors,
                    cache,
                );
            }
        }
    }
}

/// Compute lineage for a single column from a model's compiled SQL.
///
/// Used when the column isn't in the model's YAML-defined columns but exists
/// in the SQL output (common in dbt projects with incomplete column documentation).
fn compute_single_column_lineage(
    manifest: &Manifest,
    model_name: &str,
    column_name: &str,
) -> Vec<ColumnSource> {
    let node = manifest
        .nodes
        .values()
        .find(|n| n.name == model_name && n.resource_type == "model");

    let node = match node {
        Some(n) => n,
        None => return vec![],
    };

    let compiled_code = match &node.compiled_code {
        Some(code) => code,
        None => return vec![],
    };

    let expr = match polyglot_sql::parse_one(compiled_code, polyglot_sql::DialectType::Generic) {
        Ok(e) => e,
        Err(_) => return vec![],
    };

    let schema = build_schema_from_manifest(manifest, node);

    let mut expanded_expr = expr.clone();
    polyglot_sql::lineage::expand_cte_stars(
        &mut expanded_expr,
        schema.as_ref().map(|s| s as &dyn polyglot_sql::Schema),
    );

    let lineage_result = if let Some(ref s) = schema {
        polyglot_sql::lineage::lineage_with_schema(
            column_name,
            &expanded_expr,
            Some(s as &dyn polyglot_sql::Schema),
            None,
            false,
        )
        .or_else(|_| {
            polyglot_sql::lineage::lineage(column_name, &expanded_expr, None, false)
        })
    } else {
        polyglot_sql::lineage::lineage(column_name, &expanded_expr, None, false)
    };

    match lineage_result {
        Ok(lineage_node) => extract_leaf_sources(&lineage_node),
        Err(_) => vec![],
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

    // --- Cross-model lineage tests ---

    /// Build a manifest with 3 levels: customers → orders → stg_orders → raw.orders
    fn make_cross_model_manifest() -> Manifest {
        let mut nodes = HashMap::new();

        // Source: raw.orders
        let mut raw_orders_cols = HashMap::new();
        for name in ["id", "user_id", "order_date", "status"] {
            raw_orders_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        let mut sources = HashMap::new();
        sources.insert("source.proj.raw.orders".to_string(), ManifestSource {
            unique_id: "source.proj.raw.orders".to_string(),
            name: "orders".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            columns: raw_orders_cols,
        });

        // Source: raw.payments
        let mut raw_payments_cols = HashMap::new();
        for name in ["id", "order_id", "amount", "payment_method"] {
            raw_payments_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        sources.insert("source.proj.raw.payments".to_string(), ManifestSource {
            unique_id: "source.proj.raw.payments".to_string(),
            name: "payments".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            columns: raw_payments_cols,
        });

        // stg_orders: renames id→order_id, user_id→customer_id
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
            compiled_code: Some("select id as order_id, user_id as customer_id, order_date, status from orders".to_string()),
            database: None,
            schema: None,
        });

        // stg_payments: renames id→payment_id
        let mut stg_payments_cols = HashMap::new();
        for name in ["payment_id", "order_id", "amount", "payment_method"] {
            stg_payments_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.stg_payments".to_string(), ManifestNode {
            unique_id: "model.proj.stg_payments".to_string(),
            name: "stg_payments".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["source.proj.raw.payments".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_payments_cols,
            compiled_code: Some("select id as payment_id, order_id, amount, payment_method from payments".to_string()),
            database: None,
            schema: None,
        });

        // orders: joins stg_orders + stg_payments (CTE pattern like real dbt compiled SQL)
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
            compiled_code: Some(concat!(
                "with stg_orders as (select * from stg_orders), ",
                "stg_payments as (select * from stg_payments) ",
                "select stg_orders.order_id, stg_orders.customer_id, ",
                "stg_payments.amount as total_amount ",
                "from stg_orders left join stg_payments ",
                "on stg_orders.order_id = stg_payments.order_id"
            ).to_string()),
            database: None,
            schema: None,
        });

        // customers: aggregates from orders model (CTE pattern)
        let mut customers_cols = HashMap::new();
        for name in ["customer_id", "order_count"] {
            customers_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.customers".to_string(), ManifestNode {
            unique_id: "model.proj.customers".to_string(),
            name: "customers".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["model.proj.orders".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: customers_cols,
            compiled_code: Some(concat!(
                "with orders as (select * from orders) ",
                "select customer_id, count(*) as order_count from orders group by customer_id"
            ).to_string()),
            database: None,
            schema: None,
        });

        Manifest {
            nodes,
            sources,
            exposures: HashMap::new(),
        }
    }

    #[test]
    fn test_cross_model_single_hop() {
        // orders.order_id → stg_orders.order_id → raw.orders.id
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(&manifest, "orders");

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let order_id = result.columns.iter().find(|c| c.column == "order_id").unwrap();
        // Should trace through stg_orders to raw source
        assert!(
            order_id.sources.iter().any(|s| s.column == "id"),
            "order_id should trace to raw orders.id, got: {:?}",
            order_id.sources
        );
    }

    #[test]
    fn test_cross_model_two_hops() {
        // customers.customer_id → orders.customer_id → stg_orders.customer_id → raw.orders.user_id
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(&manifest, "customers");

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let customer_id = result.columns.iter().find(|c| c.column == "customer_id").unwrap();
        assert!(
            customer_id.sources.iter().any(|s| s.column == "user_id"),
            "customer_id should trace to raw orders.user_id, got: {:?}",
            customer_id.sources
        );
    }

    #[test]
    fn test_cross_model_join_sources() {
        // orders.total_amount → stg_payments.amount → raw.payments.amount
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(&manifest, "orders");

        let total_amount = result.columns.iter().find(|c| c.column == "total_amount").unwrap();
        assert!(
            total_amount.sources.iter().any(|s| s.column == "amount"),
            "total_amount should trace to raw payments.amount, got: {:?}",
            total_amount.sources
        );
    }

    #[test]
    fn test_cross_model_source_table_is_leaf() {
        // stg_orders directly references a source — cross-model should not change the result
        let manifest = make_cross_model_manifest();
        let single = compute_column_lineage(&manifest, "stg_orders");
        let cross = compute_cross_model_column_lineage(&manifest, "stg_orders");

        assert_eq!(single.columns.len(), cross.columns.len());
        for (s, c) in single.columns.iter().zip(cross.columns.iter()) {
            assert_eq!(s.column, c.column);
            assert_eq!(s.sources, c.sources);
        }
    }

    #[test]
    fn test_cross_model_model_not_found() {
        let manifest = make_cross_model_manifest();
        let result = compute_cross_model_column_lineage(&manifest, "nonexistent");
        assert!(!result.errors.is_empty());
        assert!(result.errors[0].contains("not found"));
    }

    #[test]
    fn test_normalize_table_name() {
        assert_eq!(normalize_table_name("stg_orders"), "stg_orders");
        assert_eq!(normalize_table_name("\"jaffle_shop\".\"main\".\"stg_orders\""), "stg_orders");
        assert_eq!(normalize_table_name("`raw`.`orders`"), "orders");
        assert_eq!(normalize_table_name("schema.table"), "table");
    }

    // --- Regression tests for known issues ---

    #[test]
    #[ignore = "CTE alias resolution not yet implemented"]
    fn test_cte_alias_resolution() {
        // Issue mml.6: FROM cte_name AS alias causes lineage to stop at alias
        // Pattern: WITH import_model AS (...) SELECT base.col FROM import_model AS base
        let mut nodes = HashMap::new();
        let mut sources = HashMap::new();

        // Source table
        let mut src_cols = HashMap::new();
        for name in ["id", "name", "status"] {
            src_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        sources.insert("source.proj.raw.items".to_string(), ManifestSource {
            unique_id: "source.proj.raw.items".to_string(),
            name: "items".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            columns: src_cols,
        });

        // stg_items: simple staging model
        let mut stg_cols = HashMap::new();
        for name in ["item_id", "name", "status"] {
            stg_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.stg_items".to_string(), ManifestNode {
            unique_id: "model.proj.stg_items".to_string(),
            name: "stg_items".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["source.proj.raw.items".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_cols,
            compiled_code: Some("select id as item_id, name, status from items".to_string()),
            database: None,
            schema: None,
        });

        // mart_items: uses FROM cte AS alias pattern
        let mut mart_cols = HashMap::new();
        for name in ["item_id", "status"] {
            mart_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.mart_items".to_string(), ManifestNode {
            unique_id: "model.proj.mart_items".to_string(),
            name: "mart_items".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["model.proj.stg_items".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: mart_cols,
            compiled_code: Some(concat!(
                "with import_stg_items as (\n",
                "    select * from stg_items\n",
                ")\n",
                "select base.item_id, base.status\n",
                "from import_stg_items as base"
            ).to_string()),
            database: None,
            schema: None,
        });

        let manifest = Manifest { nodes, sources, exposures: HashMap::new() };
        let result = compute_cross_model_column_lineage(&manifest, "mart_items");

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.columns.len(), 2);

        // item_id should trace through stg_items to raw items.id
        // NOT stop at alias "base"
        let item_id = result.columns.iter().find(|c| c.column == "item_id").unwrap();
        assert!(
            item_id.sources.iter().all(|s| s.table != "base"),
            "item_id should not reference alias 'base', got: {:?}",
            item_id.sources
        );
        assert!(
            item_id.sources.iter().any(|s| s.column == "id"),
            "item_id should trace to raw items.id, got: {:?}",
            item_id.sources
        );
    }

    #[test]
    fn test_select_star_chain_with_join() {
        // Issue mml.7: SELECT * chain + JOIN causes "Cannot find column" errors
        let mut nodes = HashMap::new();
        let mut sources = HashMap::new();

        // Source: raw.users
        let mut user_cols = HashMap::new();
        for name in ["id", "name", "area"] {
            user_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        sources.insert("source.proj.raw.users".to_string(), ManifestSource {
            unique_id: "source.proj.raw.users".to_string(),
            name: "users".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            columns: user_cols,
        });

        // Source: raw.regions
        let mut region_cols = HashMap::new();
        for name in ["id", "region_name"] {
            region_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        sources.insert("source.proj.raw.regions".to_string(), ManifestSource {
            unique_id: "source.proj.raw.regions".to_string(),
            name: "regions".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            columns: region_cols,
        });

        // stg_users: SELECT * from raw
        let mut stg_user_cols = HashMap::new();
        for name in ["id", "name", "area"] {
            stg_user_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.stg_users".to_string(), ManifestNode {
            unique_id: "model.proj.stg_users".to_string(),
            name: "stg_users".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["source.proj.raw.users".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_user_cols,
            compiled_code: Some("select id, name, area from users".to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        });

        // stg_regions
        let mut stg_region_cols = HashMap::new();
        for name in ["id", "region_name"] {
            stg_region_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.stg_regions".to_string(), ManifestNode {
            unique_id: "model.proj.stg_regions".to_string(),
            name: "stg_regions".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec!["source.proj.raw.regions".to_string()] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: stg_region_cols,
            compiled_code: Some("select id, region_name from regions".to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        });

        // mart_users: multi-level SELECT * chain + JOIN
        // Uses backtick-quoted 3-part names like real dbt BigQuery compiled SQL
        let mut mart_cols = HashMap::new();
        for name in ["id", "name", "area", "region_name"] {
            mart_cols.insert(name.to_string(), ManifestColumn { name: name.to_string() });
        }
        nodes.insert("model.proj.mart_users".to_string(), ManifestNode {
            unique_id: "model.proj.mart_users".to_string(),
            name: "mart_users".to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![
                "model.proj.stg_users".to_string(),
                "model.proj.stg_regions".to_string(),
            ] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            columns: mart_cols,
            compiled_code: Some(concat!(
                "with\n",
                "import_users as (\n",
                "    select * from `mydb`.`myschema`.`stg_users`\n",
                "),\n",
                "base as (\n",
                "    select * from import_users\n",
                "),\n",
                "import_regions as (\n",
                "    select * from `mydb`.`myschema`.`stg_regions`\n",
                ")\n",
                "select base.*, import_regions.region_name\n",
                "from base\n",
                "left join import_regions on base.area = import_regions.id"
            ).to_string()),
            database: Some("mydb".to_string()),
            schema: Some("myschema".to_string()),
        });

        let manifest = Manifest { nodes, sources, exposures: HashMap::new() };
        let result = compute_cross_model_column_lineage(&manifest, "mart_users");

        // All 4 columns should resolve without errors
        assert!(
            result.errors.is_empty(),
            "should resolve all columns without errors, got: {:?}",
            result.errors
        );
        assert_eq!(result.columns.len(), 4, "should have 4 columns, got: {:?}",
            result.columns.iter().map(|c| &c.column).collect::<Vec<_>>());

        // area should trace through to raw source
        let area = result.columns.iter().find(|c| c.column == "area").unwrap();
        assert!(
            !area.sources.is_empty(),
            "area should have sources"
        );
    }

}
