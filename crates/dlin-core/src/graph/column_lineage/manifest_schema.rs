//! Manifest helpers that are safe to use for ordinal-aware analysis.
//!
//! The legacy YAML schema builders remain in `schema.rs`.  The builder in this
//! module has a stricter contract: only a compiled query whose output order is
//! proven by polyglot-sql may become a `MappingSchema` relation.

use std::collections::HashSet;

use polyglot_sql::{DialectType, MappingSchema, Schema};

use crate::parser::cache::hash_str;
use crate::parser::manifest::{Manifest, ManifestNode};

use super::semantic_model::build_query_model_from_sql;

pub(super) fn make_fq_table_name(
    database: Option<&str>,
    schema: Option<&str>,
    name: &str,
) -> String {
    match (database, schema) {
        (Some(db), Some(s)) => format!("{}.{}.{}", db, s, name),
        (None, Some(s)) => format!("{}.{}", s, name),
        _ => name.to_string(),
    }
}

/// Build a schema only from dependency models with a proven SQL output order.
///
/// Manifest YAML columns are intentionally not consulted for the relation's
/// column order (or for adding a relation).  A source, YAML-only model,
/// unnamed output, duplicate output name, or unresolved wildcard is omitted.
pub fn build_proven_ordered_schema(
    manifest: &Manifest,
    node: &ManifestNode,
    dialect: DialectType,
) -> Option<MappingSchema> {
    let mut schema = MappingSchema::with_dialect(dialect);
    let mut has_entries = false;

    for dependency_id in &node.depends_on.nodes {
        let Some(dependency) = manifest.nodes.get(dependency_id) else {
            // Sources and other YAML-only relations do not prove an ordinal
            // schema for this stage.
            continue;
        };
        let Some(sql) = dependency.compiled_code.as_deref() else {
            continue;
        };
        let Ok(query_model) = build_query_model_from_sql(sql, dialect, None) else {
            continue;
        };
        let Some(column_names) = query_model.outputs.proven_ordered_named_outputs() else {
            continue;
        };

        let columns = column_names
            .into_iter()
            .map(|name| (name, polyglot_sql::expressions::DataType::Unknown))
            .collect::<Vec<_>>();
        let qualified_name = make_fq_table_name(
            dependency.database.as_deref(),
            dependency.schema.as_deref(),
            &dependency.name,
        );
        if schema.add_table(&qualified_name, &columns, None).is_ok() {
            has_entries = true;
        }
        if qualified_name != dependency.name {
            let _ = schema.add_table(&dependency.name, &columns, None);
        }
    }

    has_entries.then_some(schema)
}

pub(super) fn compute_manifest_columns_hash(manifest: &Manifest, node: &ManifestNode) -> u64 {
    let mut visited = HashSet::new();
    hash_node_columns_transitive(manifest, node, &mut visited)
}

fn hash_node_columns_transitive(
    manifest: &Manifest,
    node: &ManifestNode,
    visited: &mut HashSet<String>,
) -> u64 {
    let mut parts = Vec::new();

    let mut own_cols = node.columns.keys().collect::<Vec<_>>();
    own_cols.sort_unstable();
    for column in own_cols {
        parts.push(column.clone());
    }
    if let Some(code) = &node.compiled_code {
        parts.push(format!("sql:{}", hash_str(code)));
    }
    parts.push("|".to_string());

    let mut dependency_ids = node.depends_on.nodes.iter().collect::<Vec<_>>();
    dependency_ids.sort_unstable();
    for dependency_id in dependency_ids {
        parts.push(dependency_id.clone());
        if !visited.insert(dependency_id.clone()) {
            continue;
        }
        if let Some(dependency) = manifest.nodes.get(dependency_id) {
            let dependency_hash = hash_node_columns_transitive(manifest, dependency, visited);
            parts.push(format!("node:{}", dependency_hash));
        } else if let Some(source) = manifest.sources.get(dependency_id) {
            let mut columns = source.columns.keys().collect::<Vec<_>>();
            columns.sort_unstable();
            for column in columns {
                parts.push(column.clone());
            }
            if let Some(database) = &source.database {
                parts.push(format!("db:{}", database));
            }
            if let Some(schema) = &source.schema {
                parts.push(format!("schema:{}", schema));
            }
            if let Some(identifier) = &source.identifier {
                parts.push(format!("id:{}", identifier));
            }
        }
    }

    hash_str(&parts.join("\0"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::parser::manifest::{DependsOn, ManifestColumn, ManifestConfig};

    fn node(
        unique_id: &str,
        name: &str,
        compiled_code: Option<&str>,
        columns: &[&str],
        dependencies: &[&str],
    ) -> ManifestNode {
        let columns = columns
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect();
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns,
            compiled_code: compiled_code.map(str::to_string),
            database: None,
            schema: None,
        }
    }

    fn manifest(nodes: Vec<ManifestNode>) -> Manifest {
        Manifest {
            nodes: nodes
                .into_iter()
                .map(|node| (node.unique_id.clone(), node))
                .collect::<HashMap<_, _>>(),
            ..Manifest::default()
        }
    }

    #[test]
    fn yaml_map_order_does_not_change_proven_ordinal_schema() {
        let upstream_id = "model.test.upstream";
        let downstream_id = "model.test.downstream";
        let upstream_sql = Some("SELECT first_col, second_col FROM source_table");
        let downstream = node(
            downstream_id,
            "downstream",
            Some("SELECT first_col FROM upstream"),
            &[],
            &[upstream_id],
        );

        let first = build_proven_ordered_schema(
            &manifest(vec![
                node(
                    upstream_id,
                    "upstream",
                    upstream_sql,
                    &["first_col", "second_col"],
                    &[],
                ),
                node(
                    downstream_id,
                    "downstream",
                    Some("SELECT first_col FROM upstream"),
                    &[],
                    &[upstream_id],
                ),
            ]),
            &downstream,
            DialectType::Generic,
        )
        .unwrap();
        assert_eq!(
            first.column_names("upstream").unwrap(),
            vec!["first_col", "second_col"]
        );
        // The second manifest has the same SQL but a different YAML map order.
        // Rebuild the node reference from that manifest so only the SQL proof
        // participates in the schema.
        let second_manifest = manifest(vec![
            node(
                upstream_id,
                "upstream",
                upstream_sql,
                &["second_col", "first_col"],
                &[],
            ),
            node(
                downstream_id,
                "downstream",
                Some("SELECT first_col FROM upstream"),
                &[],
                &[upstream_id],
            ),
        ]);
        let second = build_proven_ordered_schema(
            &second_manifest,
            second_manifest.nodes.get(downstream_id).unwrap(),
            DialectType::Generic,
        )
        .unwrap();
        assert_eq!(
            second.column_names("upstream").unwrap(),
            first.column_names("upstream").unwrap()
        );
    }

    #[test]
    fn only_proven_compiled_named_outputs_enter_the_schema() {
        let good_id = "model.test.good";
        let unnamed_id = "model.test.unnamed";
        let star_id = "model.test.star";
        let downstream = node(
            "model.test.downstream",
            "downstream",
            Some("SELECT value FROM good"),
            &[],
            &[good_id, unnamed_id, star_id],
        );
        let manifest = manifest(vec![
            node(
                good_id,
                "good",
                Some("SELECT first_col, second_col FROM t"),
                &[],
                &[],
            ),
            node(
                unnamed_id,
                "unnamed",
                Some("SELECT value * 2 FROM t"),
                &["yaml_name"],
                &[],
            ),
            node(
                star_id,
                "star",
                Some("SELECT * FROM unknown"),
                &["yaml_name"],
                &[],
            ),
            downstream,
        ]);

        let schema = build_proven_ordered_schema(
            &manifest,
            manifest.nodes.get("model.test.downstream").unwrap(),
            DialectType::Generic,
        )
        .unwrap();
        assert_eq!(
            schema.column_names("good").unwrap(),
            vec!["first_col", "second_col"]
        );
        assert!(schema.column_names("unnamed").is_err());
        assert!(schema.column_names("star").is_err());
    }
}
