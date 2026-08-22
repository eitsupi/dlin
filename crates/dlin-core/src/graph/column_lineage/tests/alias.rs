use std::collections::HashMap;

use super::super::backend::{BackendId, backend_for_tests};
use super::super::schema;
use super::super::{ColumnLineageCache, ColumnSource, DlinDialect, TransformationType};
use crate::parser::manifest::{DependsOn, Manifest, ManifestColumn, ManifestConfig, ManifestNode};

#[allow(clippy::too_many_arguments)]
fn node(
    id: &str,
    name: &str,
    alias: Option<&str>,
    config_alias: Option<&str>,
    deps: Vec<&str>,
    columns: &[&str],
    sql: Option<&str>,
    database: Option<&str>,
    schema_name: Option<&str>,
) -> ManifestNode {
    let columns = columns
        .iter()
        .map(|column| {
            (
                (*column).to_string(),
                ManifestColumn {
                    name: (*column).to_string(),
                },
            )
        })
        .collect();
    ManifestNode {
        unique_id: id.to_string(),
        name: name.to_string(),
        alias: alias.map(str::to_string),
        resource_type: "model".to_string(),
        depends_on: DependsOn {
            nodes: deps.into_iter().map(str::to_string).collect(),
        },
        config: ManifestConfig {
            alias: config_alias.map(str::to_string),
            ..Default::default()
        },
        description: None,
        path: None,
        original_file_path: None,
        columns,
        compiled_code: sql.map(str::to_string),
        database: database.map(str::to_string),
        schema: schema_name.map(str::to_string),
    }
}

#[test]
fn catalog_uses_alias_for_qualified_and_bare_relation_keys() {
    let aliased_id = "model.proj.aliased_model";
    let named_id = "model.proj.named_model";
    let root_id = "model.proj.root";
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                aliased_id.to_string(),
                node(
                    aliased_id,
                    "model_name",
                    Some("physical_alias"),
                    None,
                    vec![],
                    &["id"],
                    None,
                    Some("warehouse"),
                    Some("analytics"),
                ),
            ),
            (
                named_id.to_string(),
                node(
                    named_id,
                    "named_model",
                    None,
                    None,
                    vec![],
                    &["id"],
                    None,
                    Some("warehouse"),
                    Some("analytics"),
                ),
            ),
            (
                root_id.to_string(),
                node(
                    root_id,
                    "root",
                    None,
                    None,
                    vec![aliased_id, named_id],
                    &[],
                    None,
                    None,
                    None,
                ),
            ),
        ]),
        ..Default::default()
    };
    let backend = backend_for_tests(BackendId::Polyglot);
    let root = manifest.nodes.get(root_id).unwrap();
    let catalog =
        schema::build_schema_from_manifest(&manifest, root, DlinDialect::Generic, &backend)
            .unwrap();

    assert_eq!(
        catalog.table_columns("warehouse.analytics.physical_alias"),
        Some(["id".to_string()].as_slice())
    );
    assert_eq!(
        catalog.table_columns("physical_alias"),
        Some(["id".to_string()].as_slice())
    );
    assert_eq!(
        catalog.table_columns("warehouse.analytics.named_model"),
        Some(["id".to_string()].as_slice())
    );
    assert_eq!(
        catalog.table_columns("named_model"),
        Some(["id".to_string()].as_slice())
    );
    assert_eq!(catalog.table_columns("model_name"), None);

    let yaml_catalog = schema::build_yaml_schema_for_node(&manifest, root).unwrap();
    assert_eq!(
        yaml_catalog.table_columns("physical_alias"),
        Some(["id".to_string()].as_slice())
    );
    assert_eq!(
        yaml_catalog.table_columns("named_model"),
        Some(["id".to_string()].as_slice())
    );
}

#[test]
fn relation_name_prefers_resolved_node_alias_then_config_alias_then_name() {
    let node_alias = node(
        "model.proj.node_alias",
        "model_name",
        Some("resolved_alias"),
        Some("config_alias"),
        vec![],
        &[],
        None,
        None,
        None,
    );
    assert_eq!(node_alias.relation_name(), "resolved_alias");

    let config_alias = node(
        "model.proj.config_alias",
        "model_name",
        None,
        Some("config_alias"),
        vec![],
        &[],
        None,
        None,
        None,
    );
    assert_eq!(config_alias.relation_name(), "config_alias");

    let no_alias = node(
        "model.proj.no_alias",
        "model_name",
        None,
        None,
        vec![],
        &[],
        None,
        None,
        None,
    );
    assert_eq!(no_alias.relation_name(), "model_name");

    let empty_node_alias = node(
        "model.proj.empty_node_alias",
        "model_name",
        Some(""),
        Some("config_alias"),
        vec![],
        &[],
        None,
        None,
        None,
    );
    assert_eq!(empty_node_alias.relation_name(), "config_alias");

    let empty_both = node(
        "model.proj.empty_both",
        "model_name",
        Some(""),
        Some(""),
        vec![],
        &[],
        None,
        None,
        None,
    );
    assert_eq!(empty_both.relation_name(), "model_name");
}

#[test]
fn cross_model_lineage_resolves_an_aliased_upstream_relation() {
    let upstream_id = "model.proj.upstream_model";
    let downstream_id = "model.proj.downstream_model";
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                upstream_id.to_string(),
                node(
                    upstream_id,
                    "upstream_model",
                    Some("upstream_alias"),
                    None,
                    vec![],
                    &["id"],
                    Some("select raw_upstream.id from raw_upstream"),
                    None,
                    None,
                ),
            ),
            (
                downstream_id.to_string(),
                node(
                    downstream_id,
                    "downstream_model",
                    None,
                    None,
                    vec![upstream_id],
                    &["id"],
                    Some("select upstream_alias.id from upstream_alias"),
                    None,
                    None,
                ),
            ),
        ]),
        ..Default::default()
    };
    let result = super::super::compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let id = result
        .columns
        .iter()
        .find(|column| column.column == "id")
        .unwrap();

    assert_eq!(
        id.sources,
        vec![ColumnSource {
            table: "raw_upstream".to_string(),
            column: "id".to_string(),
            model_path: vec![(
                "upstream_model".to_string(),
                "id".to_string(),
                TransformationType::Direct,
            )],
        }]
    );
}
