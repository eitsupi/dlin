use super::*;

#[test]
fn selecting_a_column_keeps_only_its_trace_boundaries() {
    let mut manifest = make_cross_model_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_payments")
        .unwrap()
        .compiled_code = None;
    let mut result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(
        result
            .boundaries
            .iter()
            .any(|boundary| { boundary.reason == TraceBoundaryReason::MissingDefinition })
    );
    result.select_columns(&["order_id"], DlinDialect::Generic);
    assert!(!result.boundaries.is_empty());
    assert!(result.boundaries.iter().all(|boundary| {
        boundary.root.slot == result.slots()[0].id
            && boundary.reason == TraceBoundaryReason::ExternalRelation
    }));
}

#[test]
fn snowflake_resource_resolution_distinguishes_quoted_and_folded_names() {
    use crate::graph::column_lineage::cross_model::resolve_resource;

    let mut manifest = make_test_manifest();
    let source = manifest.sources.get_mut("source.proj.raw.orders").unwrap();
    source.identifier = Some("ORDERS".to_string());
    source.schema = Some("RAW".to_string());
    let resolve = |query| {
        resolve_resource(
            &manifest,
            "model.proj.stg_orders",
            &RelationRef::parse(query).unwrap(),
            &Ident::unquoted("id"),
            DlinDialect::Snowflake,
        )
    };
    for query in ["raw.orders", "\"RAW\".\"ORDERS\""] {
        assert_eq!(resolve(query).resource.kind, TraceResourceKind::Source);
    }
    // A lowercase quoted spelling denotes a different physical relation.
    assert_eq!(
        resolve("\"raw\".\"orders\"").resource.kind,
        TraceResourceKind::PhysicalRelation
    );
}

#[test]
fn test_cross_model_bigquery_source_free_union_reaches_external_sources() {
    let manifest = source_free_union_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "union_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    assert_eq!(result.listed_columns, 2);
    assert_eq!(result.value_complete_columns, 2);
    assert_eq!(result.slots().len(), 2);

    let real_col = result
        .slots()
        .iter()
        .find(|column| column.column() == "real_col")
        .unwrap();
    assert_eq!(
        real_col
            .lineage
            .known()
            .iter()
            .map(|source| (source.table.as_str(), source.column.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "real_col")]
    );

    let id = result
        .slots()
        .iter()
        .find(|column| column.column() == "id")
        .unwrap();
    let mut id_sources = id
        .lineage
        .known()
        .iter()
        .map(|source| (source.table.as_str(), source.column.value.as_str()))
        .collect::<Vec<_>>();
    id_sources.sort_unstable();
    assert_eq!(
        id_sources,
        vec![("external_table_a", "id"), ("external_table_b", "id")]
    );
}

#[test]
fn test_cross_model_bigquery_compound_field_access_reaches_external_array_column() {
    let manifest = bigquery_compound_field_access_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "array_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let first_item = result
        .slots()
        .iter()
        .find(|column| column.column() == "first_item")
        .expect("first_item should be present");
    assert_eq!(
        first_item.lineage.known().len(),
        1,
        "sources: {:?}",
        first_item.lineage.known()
    );
    let source = &first_item.lineage.known()[0];
    assert_eq!(source.column, "items_array");
    assert_eq!(source.table, "external_table_a");
    assert!(
        source
            .model_path
            .iter()
            .any(|hop| hop.output.model == "upstream_model" && hop.output.column == "items_array"),
        "expected upstream model path, got: {:?}",
        source.model_path
    );
}

#[test]
fn test_cross_model_bigquery_struct_field_access_has_one_honest_contract() {
    let manifest = bigquery_struct_field_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "quoting_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let user_id = result
        .slots()
        .iter()
        .find(|column| column.column() == "user_id")
        .expect("user_id should be present");
    let plain_column = result
        .slots()
        .iter()
        .find(|column| column.column() == "plain_column")
        .expect("plain_column should be present");
    for entry in [user_id, plain_column] {
        assert_eq!(
            entry.lineage.known(),
            vec![ColumnSource {
                table: "p.d.external_table_a".to_string(),
                column: Ident::unquoted("user_id"),
                model_path: vec![hop(
                    "model.proj.upstream_model",
                    "upstream_model",
                    0,
                    "user_id",
                    TransformationType::Direct,
                )],
            }]
        );
    }

    let qualified = result
        .slots()
        .iter()
        .find(|column| column.column() == "qualified_field");
    let bare = result
        .slots()
        .iter()
        .find(|column| column.column() == "bare_field");
    let qualified = qualified.expect("qualified_field should be present");
    let bare = bare.expect("bare_field should be present");
    assert_eq!(qualified.lineage.known(), bare.lineage.known());
    for entry in [qualified, bare] {
        assert!(entry.lineage.known().is_empty());
        assert!(entry.lineage.is_partial());
    }
    for output_column in ["qualified_field", "bare_field"] {
        let diagnostic = result
            .errors
            .iter()
            .find(|error| error.column_name() == Some(output_column))
            .unwrap_or_else(|| {
                panic!(
                    "row-value diagnostic should be rebased to {output_column}: {:?}",
                    result.errors
                )
            });
        assert_eq!(diagnostic.kind, ColumnLineageErrorKind::ColumnIndeterminate);
        assert!(
            diagnostic
                .what
                .starts_with(&format!("column '{output_column}':"))
        );
        assert!(!diagnostic.is_fatal());
    }
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.column_name() == Some("event")),
        "upstream origin column must not be exposed as the output diagnostic: {:?}",
        result.errors
    );

    let public = serde_json::to_string(&result).expect("public result should serialize");
    for forbidden in ["\\\"agg.event\\\"", "\\\"event\\\"", "external_table_a.t"] {
        assert!(
            !public.contains(forbidden),
            "public result must not expose {forbidden}: {public}"
        );
    }
}

#[test]
fn test_cross_model_bigquery_row_value_child_keeps_partial_without_a_fake_source() {
    let mut manifest = bigquery_struct_field_cross_model_manifest();
    let upstream = manifest.nodes.get_mut("model.proj.upstream_model").unwrap();
    upstream.columns = ["col_a"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                crate::parser::manifest::ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    upstream.compiled_code = Some(
        "WITH latest AS (SELECT ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t) SELECT event.col_a AS col_a FROM latest".to_string(),
    );

    let mut downstream = manifest.nodes.remove("model.proj.quoting_model").unwrap();
    downstream.name = "repro_model".to_string();
    downstream.unique_id = "model.proj.repro_model".to_string();
    downstream.columns = ["col_a"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                crate::parser::manifest::ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    downstream.compiled_code = Some("SELECT col_a FROM `p`.`d`.`upstream_model`".to_string());
    manifest
        .nodes
        .insert(downstream.unique_id.clone(), downstream);

    let result = compute_cross_model_column_lineage(
        &manifest,
        "repro_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );
    let col_a = result
        .slots()
        .iter()
        .find(|column| column.column() == "col_a")
        .expect("col_a should remain published");
    assert!(col_a.lineage.known().is_empty());
    // The row-value part of upstream_model.col_a stays visible as unresolved.
    assert!(col_a.lineage.is_partial());
    assert!(matches!(
        col_a.lineage.unresolved(),
        [UnresolvedContribution {
            reason: UnresolvedReason::RowValue { .. },
            origin: ModelOutputRef { model, column, .. },
            ..
        }] if model == "upstream_model" && column == "col_a"
    ));
    let diagnostic = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("col_a"))
        .expect("partial lineage should remain explicit");
    assert_eq!(diagnostic.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(!diagnostic.is_fatal());
}

#[test]
fn test_cross_model_rebases_deep_diagnostic_to_root_output() {
    let manifest = same_named_deep_error_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "target_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let x = result
        .slots()
        .iter()
        .find(|column| column.column() == "x")
        .expect("x should be present");
    assert!(x.lineage.known().is_empty());
    assert!(x.lineage.is_complete());
    assert!(
        result
            .errors
            .iter()
            .all(|error| error.column_name() != Some("x")),
        "diagnostic from middle_model.y must not leak into target_model.x: {:?}",
        result.errors
    );
}

#[test]
fn test_cross_model_rebases_diagnostic_across_rename() {
    let mut manifest = same_named_deep_error_manifest();
    let middle = manifest.nodes.get_mut("model.proj.middle_model").unwrap();
    middle.compiled_code =
        Some("SELECT deep.x AS x, 1 AS y FROM `p`.`d`.`deep_model` AS deep".to_string());
    let target = manifest.nodes.get_mut("model.proj.target_model").unwrap();
    target.columns = ["z"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                crate::parser::manifest::ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    target.compiled_code = Some("SELECT x AS z FROM `p`.`d`.`middle_model`".to_string());

    let result = compute_cross_model_column_lineage(
        &manifest,
        "target_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );
    let diagnostic = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("z"))
        .expect("deep indeterminate diagnostic should follow renamed output");
    assert_eq!(diagnostic.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(diagnostic.what.starts_with("column 'z':"));
    assert!(!diagnostic.is_fatal());
    assert!(
        result
            .errors
            .iter()
            .all(|error| error.column_name() != Some("x")),
        "rebased diagnostic must not retain the upstream column: {:?}",
        result.errors
    );
}

#[test]
fn test_cross_model_bigquery_unnest_reaches_external_array_column() {
    let manifest = bigquery_unnest_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let item = result
        .slots()
        .iter()
        .find(|column| column.column() == "item")
        .expect("item should be present");
    assert_eq!(
        item.lineage
            .known()
            .iter()
            .map(|source| (source.table.as_str(), source.column.value.as_str()))
            .collect::<Vec<_>>(),
        vec![("external_table_a", "items_array")]
    );
    assert!(
        item.lineage
            .known()
            .iter()
            .all(|source| !(source.table == "upstream_model" && source.column == "item")),
        "UNNEST must not fabricate an upstream_model.item source: {:?}",
        item.lineage.known()
    );
}

#[test]
fn test_cross_model_rebases_distinct_same_column_errors_to_target_outputs() {
    let manifest = duplicate_column_error_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "duplicate_target",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    // Each upstream diagnostic is rebased to the target output that reads it,
    // identified by its slot, and the two are not merged.
    let left_error = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("dup_col"))
        .expect("left diagnostic should remain scoped to dup_col");
    assert_eq!(left_error.slot(), Some(OutputSlotId::new(0)));
    assert_eq!(left_error.kind, ColumnLineageErrorKind::ColumnIndeterminate);
    assert!(left_error.what.starts_with("column 'dup_col':"));
    assert!(left_error.what.contains("unexpanded wildcard"));

    let right_error = result
        .errors
        .iter()
        .find(|error| error.column_name() == Some("other_col"))
        .expect("right diagnostic should be rebased to other_col");
    assert_eq!(right_error.slot(), Some(OutputSlotId::new(1)));
    assert_eq!(right_error.kind, ColumnLineageErrorKind::ColumnAmbiguous);
    assert!(right_error.what.starts_with("column 'other_col':"));
    assert!(right_error.what.contains("is ambiguous between"));
    assert_eq!(result.errors.len(), 2, "errors: {:?}", result.errors);
}

#[test]
fn test_cross_model_bigquery_nested_star_keeps_known_user_id_lineage() {
    // BigQuery-specific EXCEPT and nested field-star syntax intentionally
    // leaves some STRUCT expansion unresolved; known scalar lineage must
    // remain available rather than being discarded with those errors.
    let manifest = bigquery_nested_star_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "downstream_model",
        DlinDialect::BigQuery,
        &mut ColumnLineageCache::disabled(),
    );

    let user_id = result
        .slots()
        .iter()
        .find(|column| column.column() == "user_id")
        .expect("user_id should remain in the known lineage columns");
    assert!(
        user_id
            .lineage
            .known()
            .iter()
            .any(|source| source.table == "raw_table" && source.column == "user_id"),
        "user_id should trace to raw_table.user_id, got: {:?}; errors: {:?}",
        user_id.lineage.known(),
        result.errors
    );
    assert!(
        !result
            .slots()
            .iter()
            .any(|column| column.column() == "updated_at"),
        "nested field star should remain unresolved"
    );
    // The nested field star leaves the output open, and the YAML column it
    // might provide is reported as not enumerated rather than as an output.
    assert!(matches!(
        result.output.as_ref().map(|output| &output.shape),
        Some(OutputShape::Open { .. })
    ));
    assert!(
        result.errors.iter().any(|error| {
            error.kind == ColumnLineageErrorKind::DeclaredColumnNotEnumerated
                && error.what.contains("column updated_at ")
                && !error.is_fatal()
        }),
        "expected a non-fatal declaration diagnostic for updated_at, got: {:?}",
        result.errors
    );
    assert!(
        !result
            .errors
            .iter()
            .any(|error| error.column_name() == Some("event")),
        "diagnostics for EXCEPT-ed event must not leak: {:?}",
        result.errors
    );
}

/// Build a manifest with 3 levels: customers → orders → stg_orders → raw.orders
#[test]
fn test_cross_model_single_hop() {
    // orders.order_id → stg_orders.order_id → raw.orders.id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    // Should trace through stg_orders to raw source (orders table)
    assert!(
        order_id
            .lineage
            .known()
            .iter()
            .any(|s| s.column == "id" && s.table.contains("orders")),
        "order_id should trace to raw orders.id, got: {:?}",
        order_id.lineage.known()
    );
    // model_path should show the hop through stg_orders
    let src = order_id
        .lineage
        .known()
        .iter()
        .find(|s| s.column == "id")
        .unwrap();
    assert!(
        src.model_path
            .iter()
            .any(|hop| hop.output.model == "stg_orders"),
        "model_path should include stg_orders, got: {:?}",
        src.model_path
    );
}

#[test]
fn test_cross_model_two_hops() {
    // customers.customer_id → orders.customer_id → stg_orders.customer_id → raw.orders.user_id
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "customers",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let customer_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "customer_id")
        .unwrap();
    assert!(
        customer_id
            .lineage
            .known()
            .iter()
            .any(|s| s.column == "user_id" && s.table.contains("orders")),
        "customer_id should trace to raw orders.user_id, got: {:?}",
        customer_id.lineage.known()
    );
    // model_path should show both hops: orders → stg_orders
    let src = customer_id
        .lineage
        .known()
        .iter()
        .find(|s| s.column == "user_id")
        .unwrap();
    assert!(
        src.model_path
            .iter()
            .any(|hop| hop.output.model == "orders")
            && src
                .model_path
                .iter()
                .any(|hop| hop.output.model == "stg_orders"),
        "model_path should include orders and stg_orders, got: {:?}",
        src.model_path
    );
    // orders should come before stg_orders in the path (closer to target)
    let orders_pos = src
        .model_path
        .iter()
        .position(|hop| hop.output.model == "orders")
        .unwrap();
    let stg_pos = src
        .model_path
        .iter()
        .position(|hop| hop.output.model == "stg_orders")
        .unwrap();
    assert!(
        orders_pos < stg_pos,
        "orders should precede stg_orders in path"
    );
}

#[test]
fn test_cross_model_join_sources() {
    // orders.total_amount → stg_payments.amount → raw.payments.amount
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert!(
        total_amount
            .lineage
            .known()
            .iter()
            .any(|s| s.column == "amount" && s.table.contains("payments")),
        "total_amount should trace to raw payments.amount, got: {:?}",
        total_amount.lineage.known()
    );
    // model_path should show the hop through stg_payments
    let src = total_amount
        .lineage
        .known()
        .iter()
        .find(|s| s.column == "amount")
        .unwrap();
    assert!(
        src.model_path
            .iter()
            .any(|hop| hop.output.model == "stg_payments"),
        "model_path should include stg_payments, got: {:?}",
        src.model_path
    );
}

#[test]
fn test_cross_model_source_table_is_leaf() {
    // stg_orders directly references a source — cross-model should not change the result
    let manifest = make_cross_model_manifest();
    let single = compute_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let cross = compute_cross_model_column_lineage(
        &manifest,
        "stg_orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert_eq!(single.slots().len(), cross.slots().len());
    assert!(cross.boundaries.iter().any(|boundary| {
        boundary.reason == TraceBoundaryReason::ExternalRelation
            && boundary.resource.kind == TraceResourceKind::Source
    }));
    for (s, c) in single.slots().iter().zip(cross.slots().iter()) {
        assert_eq!(s.name, c.name);
        assert_eq!(s.lineage.known(), c.lineage.known());
    }
}

#[test]
fn seed_is_an_external_relation_under_the_fixed_policy() {
    let seed_id = "seed.proj.seed_orders";
    let model_id = "model.proj.seed_reader";
    let seed = ManifestNode {
        unique_id: seed_id.to_string(),
        name: "seed_orders".to_string(),
        alias: None,
        resource_type: "seed".to_string(),
        depends_on: crate::parser::manifest::DependsOn { nodes: vec![] },
        config: crate::parser::manifest::ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: indexmap::IndexMap::new(),
        compiled_code: None,
        database: None,
        schema: None,
    };
    let reader = ManifestNode {
        unique_id: model_id.to_string(),
        name: "seed_reader".to_string(),
        alias: None,
        resource_type: "model".to_string(),
        depends_on: crate::parser::manifest::DependsOn {
            nodes: vec![seed_id.to_string()],
        },
        config: crate::parser::manifest::ManifestConfig::default(),
        description: None,
        path: None,
        original_file_path: None,
        columns: indexmap::IndexMap::new(),
        compiled_code: Some("select id from seed_orders".to_string()),
        database: None,
        schema: None,
    };
    let manifest = Manifest {
        nodes: HashMap::from([(seed_id.to_string(), seed), (model_id.to_string(), reader)]),
        ..Default::default()
    };
    let result = compute_cross_model_column_lineage(
        &manifest,
        "seed_reader",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.slot_named("id").lineage.is_complete());
    assert!(result.boundaries.iter().any(|boundary| {
        boundary.reason == TraceBoundaryReason::ExternalRelation
            && boundary.resource.kind == TraceResourceKind::Seed
            && boundary.resource.policy == TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal
    }));
}

#[test]
fn source_free_output_has_a_source_free_boundary_without_a_fake_source() {
    let mut manifest = make_cross_model_manifest();
    let unique_id = "model.proj.constant_model";
    manifest.nodes.insert(
        unique_id.to_string(),
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: "constant_model".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn { nodes: vec![] },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: indexmap::IndexMap::new(),
            compiled_code: Some("select 1 as value".to_string()),
            database: None,
            schema: None,
        },
    );
    let downstream_id = "model.proj.constant_downstream";
    manifest.nodes.insert(
        downstream_id.to_string(),
        ManifestNode {
            unique_id: downstream_id.to_string(),
            name: "constant_downstream".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: vec![unique_id.to_string()],
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: indexmap::IndexMap::new(),
            compiled_code: Some("select value from constant_model".to_string()),
            database: None,
            schema: None,
        },
    );
    let result = compute_cross_model_column_lineage(
        &manifest,
        "constant_downstream",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.slot_named("value").lineage.known().is_empty());
    assert!(result.slot_named("value").lineage.is_complete());
    assert!(result.boundaries.iter().any(|boundary| boundary.reason
        == TraceBoundaryReason::SourceFree
        && boundary.resource.unique_id.as_deref() == Some(unique_id)));
}

#[test]
fn failed_upstream_is_a_boundary_and_makes_the_output_partial() {
    let mut manifest = make_cross_model_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = None;

    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let order_id = result.slot_named("order_id");
    assert!(order_id.lineage.is_partial());
    let boundary = result
        .boundaries
        .iter()
        .find(|boundary| boundary.reason == TraceBoundaryReason::MissingDefinition)
        .expect("missing SQL is reported as a boundary");
    assert_eq!(
        boundary.root,
        ModelOutputRef {
            unique_id: "model.proj.orders".to_string(),
            model: "orders".to_string(),
            slot: order_id.id,
            column: "order_id".to_string(),
        }
    );
    assert_eq!(boundary.at.unique_id, "model.proj.orders");
    assert_eq!(
        boundary.resource.unique_id.as_deref(),
        Some("model.proj.stg_orders")
    );
}

#[test]
fn analysis_failure_two_hops_away_is_partial_without_a_fake_source() {
    let mut manifest = make_cross_model_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_orders")
        .unwrap()
        .compiled_code = Some("select from".to_string());
    let downstream_id = "model.proj.orders_downstream";
    manifest.nodes.insert(
        downstream_id.to_string(),
        ManifestNode {
            unique_id: downstream_id.to_string(),
            name: "orders_downstream".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: vec!["model.proj.orders".to_string()],
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: indexmap::IndexMap::new(),
            compiled_code: Some("select order_id from orders".to_string()),
            database: None,
            schema: None,
        },
    );
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders_downstream",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let slot = result.slot_named("order_id");
    assert!(slot.lineage.is_partial());
    assert!(slot.lineage.known().is_empty());
    assert!(result.boundaries.iter().any(|boundary| boundary.reason
        == TraceBoundaryReason::AnalysisFailed
        && boundary.resource.unique_id.as_deref() == Some("model.proj.stg_orders")));
}

#[test]
fn snapshot_is_an_unsupported_materialization_boundary() {
    let mut manifest = make_cross_model_manifest();
    manifest
        .nodes
        .get_mut("model.proj.stg_payments")
        .unwrap()
        .resource_type = "snapshot".to_string();

    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let boundary = result
        .boundaries
        .iter()
        .find(|boundary| boundary.reason == TraceBoundaryReason::UnsupportedMaterialization)
        .expect("snapshot semantics are outside the trace model");
    assert_eq!(boundary.resource.kind, TraceResourceKind::Snapshot);
    assert_eq!(
        boundary.resource.policy,
        TraceBoundaryPolicy::SnapshotUnsupportedSeedExternal
    );
    assert!(result.slot_named("total_amount").lineage.is_partial());
}

#[test]
fn snapshot_without_definition_is_a_missing_definition_boundary() {
    let mut manifest = make_cross_model_manifest();
    let snapshot = manifest.nodes.get_mut("model.proj.stg_payments").unwrap();
    snapshot.resource_type = "snapshot".to_string();
    snapshot.compiled_code = None;

    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.boundaries.iter().any(|boundary| {
        boundary.reason == TraceBoundaryReason::MissingDefinition
            && boundary.resource.kind == TraceResourceKind::Snapshot
    }));
}

#[test]
fn missing_manifest_dependency_is_not_reported_as_an_external_relation() {
    let mut manifest = make_cross_model_manifest();
    let node = manifest.nodes.get_mut("model.proj.orders").unwrap();
    node.depends_on
        .nodes
        .push("model.proj.disappeared".to_string());
    node.compiled_code = Some("select id from disappeared".to_string());
    node.columns = indexmap::IndexMap::from([(
        "id".to_string(),
        crate::parser::manifest::ManifestColumn {
            name: "id".to_string(),
            quote: None,
        },
    )]);

    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let boundary = result
        .boundaries
        .iter()
        .find(|boundary| boundary.reason == TraceBoundaryReason::MissingDefinition)
        .expect("a dangling expected dependency is incomplete");
    assert!(
        boundary
            .resource
            .candidates
            .iter()
            .any(|candidate| { candidate.unique_id == "model.proj.disappeared" })
    );
    assert!(result.slot_named("id").lineage.is_partial());
}

#[test]
fn ambiguous_short_model_name_is_not_resolved_to_the_first_match() {
    let manifest = make_duplicate_name_manifest();
    let result = compute_column_lineage(
        &manifest,
        "customers",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert_eq!(
        result.errors[0].kind,
        ColumnLineageErrorKind::ModelAmbiguous
    );
    assert!(result.errors[0].what.contains("model.pkg_a.customers"));
    assert!(result.errors[0].what.contains("model.pkg_b.customers"));
}

#[test]
fn model_cycle_is_kept_as_an_incomplete_boundary() {
    fn node(unique_id: &str, name: &str, dependency: &str, sql: &str) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: vec![dependency.to_string()],
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: indexmap::IndexMap::new(),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }
    let left = "model.proj.cycle_left";
    let right = "model.proj.cycle_right";
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                left.to_string(),
                node(left, "cycle_left", right, "select id from cycle_right"),
            ),
            (
                right.to_string(),
                node(right, "cycle_right", left, "select id from cycle_left"),
            ),
        ]),
        ..Default::default()
    };

    let result = compute_cross_model_column_lineage(
        &manifest,
        "cycle_left",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.slot_named("id").lineage.is_partial());
    assert!(
        result
            .boundaries
            .iter()
            .any(|boundary| boundary.reason == TraceBoundaryReason::Cycle)
    );
}

#[test]
fn model_level_cycle_does_not_cut_an_acyclic_slot_path() {
    fn node(unique_id: &str, name: &str, dependencies: &[&str], sql: &str) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: dependencies.iter().map(|id| id.to_string()).collect(),
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: indexmap::IndexMap::new(),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }
    let left = "model.proj.slot_cycle_left";
    let right = "model.proj.slot_cycle_right";
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                left.to_string(),
                node(
                    left,
                    "slot_cycle_left",
                    &[right],
                    "select 1 as x, slot_cycle_right.y as z from slot_cycle_right",
                ),
            ),
            (
                right.to_string(),
                node(
                    right,
                    "slot_cycle_right",
                    &[left],
                    "select slot_cycle_left.x as y from slot_cycle_left",
                ),
            ),
        ]),
        ..Default::default()
    };

    let result = compute_cross_model_column_lineage(
        &manifest,
        "slot_cycle_left",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let z = result.slot_named("z");
    assert!(z.lineage.is_complete(), "z lineage: {:?}", z.lineage);
    assert!(z.lineage.known().is_empty());
    assert!(result.boundaries.iter().any(|boundary| {
        boundary.root.slot == z.id && boundary.reason == TraceBoundaryReason::SourceFree
    }));
    assert!(!result.boundaries.iter().any(|boundary| {
        boundary.root.slot == z.id && boundary.reason == TraceBoundaryReason::Cycle
    }));
}

#[test]
fn diamond_can_visit_the_same_slot_in_separate_branches() {
    fn node(unique_id: &str, name: &str, dependencies: &[&str], sql: &str) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: crate::parser::manifest::DependsOn {
                nodes: dependencies.iter().map(|id| id.to_string()).collect(),
            },
            config: crate::parser::manifest::ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: indexmap::IndexMap::new(),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }
    let shared = "model.proj.diamond_shared";
    let left = "model.proj.diamond_left";
    let right = "model.proj.diamond_right";
    let root = "model.proj.diamond_root";
    let manifest = Manifest {
        nodes: HashMap::from([
            (
                shared.to_string(),
                node(shared, "diamond_shared", &[], "select 1 as id"),
            ),
            (
                left.to_string(),
                node(
                    left,
                    "diamond_left",
                    &[shared],
                    "select id from diamond_shared",
                ),
            ),
            (
                right.to_string(),
                node(
                    right,
                    "diamond_right",
                    &[shared],
                    "select id from diamond_shared",
                ),
            ),
            (
                root.to_string(),
                node(
                    root,
                    "diamond_root",
                    &[left, right],
                    "select coalesce(l.id, r.id) as id from diamond_left l join diamond_right r on 1 = 1",
                ),
            ),
        ]),
        ..Default::default()
    };

    let result = compute_cross_model_column_lineage(
        &manifest,
        "diamond_root",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    let id = result.slot_named("id");
    assert!(id.lineage.is_complete(), "id lineage: {:?}", id.lineage);
    assert!(id.lineage.known().is_empty());
    assert!(!result.boundaries.iter().any(|boundary| {
        boundary.root.slot == id.id && boundary.reason == TraceBoundaryReason::Cycle
    }));
    let source_free_paths = result
        .boundaries
        .iter()
        .filter(|boundary| {
            boundary.root.slot == id.id && boundary.reason == TraceBoundaryReason::SourceFree
        })
        .map(|boundary| boundary.model_path.clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        source_free_paths.len(),
        2,
        "boundaries: {:?}",
        result.boundaries
    );
}

#[test]
fn test_cross_model_model_not_found() {
    let manifest = make_cross_model_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "nonexistent",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(!result.errors.is_empty());
    assert!(result.errors[0].what.contains("not found"));
}

#[test]
fn test_cross_model_diamond_different_columns_through_shared_model() {
    // In a diamond DAG, different columns (x and y) flow through a shared
    // upstream model. Both should be resolved independently — the visited set
    // must not truncate the second column's path through the shared model.
    let manifest = make_diamond_manifest();

    // Verify left_model traces x through shared to raw_data
    let left = compute_cross_model_column_lineage(
        &manifest,
        "left_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(left.errors.is_empty(), "left errors: {:?}", left.errors);
    let left_x = left.slots().iter().find(|c| c.column() == "x").unwrap();
    assert!(
        left_x.lineage.known().iter().any(|s| s.column == "x"),
        "left_model.x should trace through shared, got: {:?}",
        left_x.lineage.known()
    );

    // Verify right_model traces y through shared to raw_data
    let right = compute_cross_model_column_lineage(
        &manifest,
        "right_model",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(right.errors.is_empty(), "right errors: {:?}", right.errors);
    let right_y = right.slots().iter().find(|c| c.column() == "y").unwrap();
    assert!(
        right_y.lineage.known().iter().any(|s| s.column == "y"),
        "right_model.y should trace through shared, got: {:?}",
        right_y.lineage.known()
    );

    // Both left and right depend on 'shared' — the key assertion is that
    // resolving one does not prevent the other from being resolved.
    // With the old model-only visited set, whichever resolved first would
    // block the other from tracing through 'shared'.
    assert!(
        !left_x.lineage.known().is_empty() && !right_y.lineage.known().is_empty(),
        "both paths through shared should resolve independently"
    );
}

// The orders model uses aliases (stg_orders AS o, stg_payments AS p).
// These two tests guard against regressions where collect_leaves returns
// the SQL alias instead of the actual model name.

#[test]
fn test_join_alias_resolves_to_model_name() {
    let manifest = make_test_manifest();
    let result = compute_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    // p.amount → table must be "stg_payments", not the alias "p"
    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert_eq!(
        total_amount.lineage.known()[0].table,
        "stg_payments",
        "expected stg_payments, got SQL alias 'p': {:?}",
        total_amount.lineage.known()
    );

    // o.order_id → table must be "stg_orders", not the alias "o"
    let order_id = result
        .slots()
        .iter()
        .find(|c| c.column() == "order_id")
        .unwrap();
    assert_eq!(
        order_id.lineage.known()[0].table,
        "stg_orders",
        "expected stg_orders, got SQL alias 'o': {:?}",
        order_id.lineage.known()
    );
}

#[test]
fn test_cross_model_join_alias_traces_to_raw_source() {
    // Cross-model lineage must follow through aliases to reach raw sources.
    // Before the fix, "p" didn't match upstream_models so the trace stopped early.
    let manifest = make_test_manifest();
    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let total_amount = result
        .slots()
        .iter()
        .find(|c| c.column() == "total_amount")
        .unwrap();
    assert!(!total_amount.lineage.known().is_empty());
    let src = &total_amount.lineage.known()[0];
    assert_ne!(src.table, "p", "source table must not be SQL alias 'p'");
    assert_ne!(
        src.table, "stg_payments",
        "cross-model must trace beyond stg_payments"
    );
    assert_eq!(src.table, "raw.payments");
    assert_eq!(src.column, "amount");
}

#[test]
fn test_cross_model_case_insensitive_catalog_match_uses_upstream_output_name() {
    let mut manifest = make_cross_model_manifest();

    // The upstream SQL and manifest expose ID, while the downstream SQL uses
    // lowercase id. stg_payments has no matching column, so its presence in
    // the join lets the catalog resolve id to stg_orders.ID unambiguously.
    let upstream = manifest.nodes.get_mut("model.proj.stg_orders").unwrap();
    upstream.columns.shift_remove("order_id");
    upstream.columns.insert(
        "ID".to_string(),
        ManifestColumn {
            name: "ID".to_string(),
            quote: None,
        },
    );
    upstream.compiled_code =
        Some("select id as ID, user_id as customer_id, order_date, status from orders".to_string());

    let downstream = manifest.nodes.get_mut("model.proj.orders").unwrap();
    downstream.columns = ["chosen"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                    quote: None,
                },
            )
        })
        .collect();
    downstream.compiled_code = Some(
        "SELECT id AS chosen FROM stg_orders JOIN stg_payments ON stg_orders.ID = stg_payments.order_id"
            .to_string(),
    );

    let result = compute_cross_model_column_lineage(
        &manifest,
        "orders",
        DlinDialect::Generic,
        &mut ColumnLineageCache::disabled(),
    );

    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
    let chosen = result
        .slots()
        .iter()
        .find(|column| column.column() == "chosen")
        .expect("chosen should be present");
    assert_eq!(
        chosen.lineage.known().len(),
        1,
        "sources: {:?}",
        chosen.lineage.known()
    );
    let source = &chosen.lineage.known()[0];
    // The raw source is referenced as orders in the upstream SQL.
    assert_eq!(source.table, "orders");
    assert_eq!(source.column, "id");
    assert!(
        source
            .model_path
            .iter()
            .any(|hop| hop.output.model == "stg_orders" && hop.output.column == "ID"),
        "model_path should retain the upstream output name ID: {:?}",
        source.model_path
    );
}
