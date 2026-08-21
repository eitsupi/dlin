use std::collections::HashSet;

use polyglot_sql::{DialectType, Expression, Schema};

use crate::parser::cache::hash_str;
use crate::parser::manifest::Manifest;

fn make_fq_table_name(database: Option<&str>, schema: Option<&str>, name: &str) -> String {
    match (database, schema) {
        (Some(db), Some(s)) => format!("{}.{}.{}", db, s, name),
        (None, Some(s)) => format!("{}.{}", s, name),
        _ => name.to_string(),
    }
}

pub(super) fn build_schema_from_manifest(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    dialect: DialectType,
) -> Option<polyglot_sql::MappingSchema> {
    let mut schema = polyglot_sql::MappingSchema::new();
    let mut has_entries = false;

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let col_names = resolve_node_columns(dep_node, manifest, dialect);
            if !col_names.is_empty() {
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = col_names
                    .iter()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();

                let fq_name = make_fq_table_name(
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    &dep_node.name,
                );
                if schema.add_table(&fq_name, &cols, None).is_ok() {
                    has_entries = true;
                }
                if fq_name != dep_node.name {
                    let _ = schema.add_table(&dep_node.name, &cols, None);
                }
            }
            continue;
        }

        if let Some(dep_source) = manifest.sources.get(dep_id)
            && !dep_source.columns.is_empty()
        {
            let mut source_col_names: Vec<&String> = dep_source.columns.keys().collect();
            source_col_names.sort_unstable();
            let cols: Vec<(String, polyglot_sql::expressions::DataType)> = source_col_names
                .into_iter()
                .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                .collect();
            let physical_identifier = dep_source.identifier.as_deref().unwrap_or(&dep_source.name);
            let physical_fq = make_fq_table_name(
                dep_source.database.as_deref(),
                dep_source.schema.as_deref(),
                physical_identifier,
            );
            if schema.add_table(&physical_fq, &cols, None).is_ok() {
                has_entries = true;
            }
            let schema_fq =
                make_fq_table_name(None, dep_source.schema.as_deref(), physical_identifier);
            let source_qualified = format!("{}.{}", dep_source.source_name, dep_source.name);
            for alias in [
                schema_fq.as_str(),
                physical_identifier,
                dep_source.name.as_str(),
                source_qualified.as_str(),
            ] {
                if alias != physical_fq {
                    let _ = schema.add_table(alias, &cols, None);
                }
            }
        }
    }

    if has_entries { Some(schema) } else { None }
}

fn resolve_node_columns(
    dep_node: &crate::parser::manifest::ManifestNode,
    manifest: &Manifest,
    dialect: DialectType,
) -> Vec<String> {
    let yaml_cols: HashSet<String> = dep_node.columns.keys().cloned().collect();
    let inferred_cols: HashSet<String> = dep_node
        .compiled_code
        .as_ref()
        .map(|code| {
            let schema = build_yaml_schema_for_node(manifest, dep_node);
            infer_output_columns(code, dialect, schema.as_ref(), None)
        })
        .unwrap_or_default()
        .into_iter()
        .collect();

    let mut merged: Vec<String> = yaml_cols.union(&inferred_cols).cloned().collect();
    merged.sort_unstable();
    merged
}

pub(super) fn build_yaml_schema_for_node(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> Option<polyglot_sql::MappingSchema> {
    let mut schema = polyglot_sql::MappingSchema::new();
    let mut has_entries = false;

    for dep_id in &node.depends_on.nodes {
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            if !dep_node.columns.is_empty() {
                let mut node_col_names: Vec<&String> = dep_node.columns.keys().collect();
                node_col_names.sort_unstable();
                let cols: Vec<(String, polyglot_sql::expressions::DataType)> = node_col_names
                    .into_iter()
                    .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                    .collect();
                let fq_name = make_fq_table_name(
                    dep_node.database.as_deref(),
                    dep_node.schema.as_deref(),
                    &dep_node.name,
                );
                if schema.add_table(&fq_name, &cols, None).is_ok() {
                    has_entries = true;
                }
                if fq_name != dep_node.name {
                    let _ = schema.add_table(&dep_node.name, &cols, None);
                }
            }
            continue;
        }

        if let Some(dep_source) = manifest.sources.get(dep_id)
            && !dep_source.columns.is_empty()
        {
            let mut source_col_names: Vec<&String> = dep_source.columns.keys().collect();
            source_col_names.sort_unstable();
            let cols: Vec<(String, polyglot_sql::expressions::DataType)> = source_col_names
                .into_iter()
                .map(|name| (name.clone(), polyglot_sql::expressions::DataType::Unknown))
                .collect();
            let physical_identifier = dep_source.identifier.as_deref().unwrap_or(&dep_source.name);
            let physical_fq = make_fq_table_name(
                dep_source.database.as_deref(),
                dep_source.schema.as_deref(),
                physical_identifier,
            );
            if schema.add_table(&physical_fq, &cols, None).is_ok() {
                has_entries = true;
            }
            let schema_fq =
                make_fq_table_name(None, dep_source.schema.as_deref(), physical_identifier);
            let source_qualified = format!("{}.{}", dep_source.source_name, dep_source.name);
            for alias in [
                schema_fq.as_str(),
                physical_identifier,
                dep_source.name.as_str(),
                source_qualified.as_str(),
            ] {
                if alias != physical_fq {
                    let _ = schema.add_table(alias, &cols, None);
                }
            }
        }
    }

    if has_entries { Some(schema) } else { None }
}

pub(super) fn infer_output_columns(
    sql: &str,
    dialect: DialectType,
    schema: Option<&polyglot_sql::MappingSchema>,
    parsed_expr: Option<&Expression>,
) -> Vec<String> {
    let expr = match parsed_expr {
        Some(e) => e.clone(),
        None => match polyglot_sql::parse_one(sql, dialect) {
            Ok(e) => e,
            Err(_) => return vec![],
        },
    };
    crate::parser::columns::extract_select_columns_from_expr(
        &expr,
        schema.map(|s| s as &dyn polyglot_sql::Schema),
    )
}

pub(super) fn compute_manifest_columns_hash(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
) -> u64 {
    let mut visited: HashSet<String> = HashSet::new();
    hash_node_columns_transitive(manifest, node, &mut visited)
}

fn hash_node_columns_transitive(
    manifest: &Manifest,
    node: &crate::parser::manifest::ManifestNode,
    visited: &mut HashSet<String>,
) -> u64 {
    let mut parts: Vec<String> = Vec::new();

    let mut own_cols: Vec<&String> = node.columns.keys().collect();
    own_cols.sort();
    for col in own_cols {
        parts.push(col.clone());
    }
    if let Some(code) = &node.compiled_code {
        parts.push(format!("sql:{}", hash_str(code)));
    }
    parts.push("|".to_string());

    let mut dep_ids: Vec<&String> = node.depends_on.nodes.iter().collect();
    dep_ids.sort();
    for dep_id in dep_ids {
        parts.push(dep_id.clone());
        if visited.contains(dep_id) {
            continue;
        }
        visited.insert(dep_id.clone());
        if let Some(dep_node) = manifest.nodes.get(dep_id) {
            let dep_hash = hash_node_columns_transitive(manifest, dep_node, visited);
            parts.push(format!("node:{}", dep_hash));
        } else if let Some(dep_source) = manifest.sources.get(dep_id) {
            let mut cols: Vec<&String> = dep_source.columns.keys().collect();
            cols.sort();
            for col in cols {
                parts.push(col.clone());
            }
            if let Some(db) = &dep_source.database {
                parts.push(format!("db:{}", db));
            }
            if let Some(s) = &dep_source.schema {
                parts.push(format!("schema:{}", s));
            }
            if let Some(id) = &dep_source.identifier {
                parts.push(format!("id:{}", id));
            }
        }
    }

    hash_str(&parts.join("\0"))
}
