use std::collections::HashMap;

use super::super::schema;
use super::super::{
    ColumnLineageCache, ColumnSource, DlinDialect, Ident, TransformationType, compute_column_impact,
};
use super::{ModelLineageTestExt, SlotTestExt, hop};
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
                    quote: None,
                },
            )
        })
        .collect();
    ManifestNode {
        package_name: None,
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
        relation_name: None,
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
    let root = manifest.nodes.get(root_id).unwrap();
    let catalog = schema::SchemaContext::new(&manifest, None, DlinDialect::Generic)
        .catalog_for(root)
        .unwrap();

    assert_eq!(
        catalog.table_columns("warehouse.analytics.physical_alias"),
        Some(vec!["id"])
    );
    assert_eq!(catalog.table_columns("physical_alias"), Some(vec!["id"]));
    assert_eq!(
        catalog.table_columns("warehouse.analytics.named_model"),
        Some(vec!["id"])
    );
    assert_eq!(catalog.table_columns("named_model"), Some(vec!["id"]));
    assert_eq!(catalog.table_columns("model_name"), None);
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
    assert_eq!(node_alias.identifier(), "resolved_alias");

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
    assert_eq!(config_alias.identifier(), "config_alias");

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
    assert_eq!(no_alias.identifier(), "model_name");

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
    assert_eq!(empty_node_alias.identifier(), "config_alias");

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
    assert_eq!(empty_both.identifier(), "model_name");
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
        .slots()
        .iter()
        .find(|column| column.column() == "id")
        .unwrap();

    assert_eq!(
        id.lineage.known(),
        vec![ColumnSource {
            table: "raw_upstream".to_string(),
            column: Ident::unquoted("id"),
            model_path: vec![hop(
                "model.proj.upstream_model",
                "upstream_model",
                0,
                "id",
                TransformationType::Direct,
            )],
            // The column is read from the upstream model's SQL output.
            evidence: vec![crate::graph::column_lineage::EvidenceId(
                "sql:model.proj.upstream_model".to_string(),
            )],
        }]
    );
}

#[test]
fn column_impact_matches_aliased_upstream_and_keeps_logical_labels() {
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

    let report = compute_column_impact(
        &manifest,
        "upstream_model",
        "id",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(report.model, "upstream_model");
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert!(report.confirmed.iter().any(|column| {
        column.target.unique_id == downstream_id
            && column.target.model == "downstream_model"
            && column.target.column == "id"
    }));
}

#[test]
fn cross_model_lineage_resolves_same_named_dependency_by_unique_id() {
    let pkg_a_id = "model.pkg_a.shared_model";
    let pkg_b_id = "model.pkg_b.shared_model";
    let downstream_id = "model.pkg_b.downstream_model";
    let source_a_id = "source.pkg_a.raw_a";
    let source_b_id = "source.pkg_b.raw_b";

    let manifest = Manifest {
        nodes: HashMap::from([
            (
                pkg_a_id.to_string(),
                node(
                    pkg_a_id,
                    "shared_model",
                    None,
                    None,
                    vec![source_a_id],
                    &["id"],
                    Some("select id from raw_a"),
                    None,
                    None,
                ),
            ),
            (
                pkg_b_id.to_string(),
                node(
                    pkg_b_id,
                    "shared_model",
                    None,
                    None,
                    vec![source_b_id],
                    &["id"],
                    Some("select id from raw_b"),
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
                    vec![pkg_b_id],
                    &["id"],
                    Some("select shared_model.id from shared_model"),
                    None,
                    None,
                ),
            ),
        ]),
        sources: HashMap::from([
            (source_a_id.to_string(), source("raw_a", source_a_id)),
            (source_b_id.to_string(), source("raw_b", source_b_id)),
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
        .slots()
        .iter()
        .find(|column| column.column() == "id")
        .unwrap();

    assert_eq!(id.lineage.known()[0].table, "raw.raw_b");
    assert_eq!(
        id.lineage.known()[0].model_path[0].output.model,
        "shared_model"
    );
}

/// The known tables of each output column of a model reading `source` with
/// `sql`, paired with the unique id of the resource its boundary reached.
fn source_reads(
    source: crate::parser::manifest::ManifestSource,
    sql: &str,
) -> Vec<(String, Option<String>)> {
    let model_id = "model.pkg.reader";
    let source_id = source.unique_id.clone();
    let manifest = Manifest {
        nodes: HashMap::from([(
            model_id.to_string(),
            node(
                model_id,
                "reader",
                None,
                None,
                vec![&source_id],
                &[],
                Some(sql),
                None,
                None,
            ),
        )]),
        sources: HashMap::from([(source_id, source)]),
        ..Default::default()
    };
    let result = super::super::compute_cross_model_column_lineage(
        &manifest,
        "reader",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    result
        .slots()
        .iter()
        .map(|slot| {
            let known = slot.lineage.known();
            assert_eq!(known.len(), 1, "{slot:?}");
            let boundary = result
                .boundaries
                .iter()
                .find(|boundary| boundary.root.slot == slot.id)
                .expect("each column reaches a boundary");
            (known[0].table.clone(), boundary.resource.unique_id.clone())
        })
        .collect()
}

fn read(table: &str, unique_id: Option<&str>) -> (String, Option<String>) {
    (table.to_string(), unique_id.map(str::to_string))
}

/// A source without a schema whose `relation_name` puts it in another schema
/// than the source name. Compiled SQL names a source by its physical
/// relation, so neither its logical name nor the default schema refers to it.
#[test]
fn a_source_is_read_only_through_its_physical_relation() {
    let source_id = "source.pkg.raw.orders";
    let orders = || crate::parser::manifest::ManifestSource {
        identifier: Some("orders_v2".to_string()),
        relation_name: Some(r#""landing"."orders_v2""#.to_string()),
        ..source("orders", source_id)
    };
    let source = Some(source_id);

    for (sql, expected) in [
        (
            "select id from landing.orders_v2",
            read("landing.orders_v2", source),
        ),
        (
            "select id from orders_v2",
            read("landing.orders_v2", source),
        ),
        ("select id from raw.orders", read("raw.orders", None)),
        ("select id from orders", read("orders", None)),
        ("select id from raw.orders_v2", read("raw.orders_v2", None)),
    ] {
        assert_eq!(source_reads(orders(), sql), [expected], "{sql}");
    }

    assert_eq!(
        source_reads(
            orders(),
            "select v2.id, other.id as other_id from landing.orders_v2 as v2 cross join raw.orders as other",
        ),
        [read("landing.orders_v2", source), read("raw.orders", None)]
    );
}

/// A source whose schema differs from its source name is not read through
/// the source name, even when its identifier is its logical name.
#[test]
fn a_source_is_not_read_through_its_source_name_as_a_schema() {
    let source_id = "source.pkg.stripe.payments";
    let payments = || crate::parser::manifest::ManifestSource {
        source_name: "stripe".to_string(),
        schema: Some("stripe_raw".to_string()),
        ..source("payments", source_id)
    };

    assert_eq!(
        source_reads(payments(), "select id from stripe_raw.payments"),
        [read("stripe_raw.payments", Some(source_id))]
    );
    assert_eq!(
        source_reads(payments(), "select id from payments"),
        [read("stripe_raw.payments", Some(source_id))]
    );
    assert_eq!(
        source_reads(payments(), "select id from stripe.payments"),
        [read("stripe.payments", None)]
    );
}

fn source(name: &str, unique_id: &str) -> crate::parser::manifest::ManifestSource {
    crate::parser::manifest::ManifestSource {
        package_name: None,
        unique_id: unique_id.to_string(),
        name: name.to_string(),
        source_name: "raw".to_string(),
        resource_type: "source".to_string(),
        description: None,
        path: None,
        original_file_path: None,
        columns: indexmap::IndexMap::from([(
            "id".to_string(),
            ManifestColumn {
                name: "id".to_string(),
                quote: None,
            },
        )]),
        database: None,
        schema: None,
        identifier: None,
        relation_name: None,
    }
}
