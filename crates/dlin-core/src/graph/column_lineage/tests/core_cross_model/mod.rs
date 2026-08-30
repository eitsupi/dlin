use std::collections::HashMap;

use super::*;
use crate::parser::manifest::{
    DependsOn, Manifest, ManifestColumn, ManifestConfig, ManifestNode, ManifestSource,
};

fn source_free_union_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }

    fn source(unique_id: &str, name: &str, output_columns: &[&str]) -> ManifestSource {
        ManifestSource {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            database: None,
            schema: None,
            identifier: None,
        }
    }

    let source_a_id = "source.proj.external_table_a";
    let source_b_id = "source.proj.external_table_b";
    let upstream_id = "model.proj.upstream_model";
    let union_id = "model.proj.union_model";
    let mut sources = HashMap::new();
    sources.insert(
        source_a_id.to_string(),
        source(source_a_id, "external_table_a", &["real_col", "id"]),
    );
    sources.insert(
        source_b_id.to_string(),
        source(source_b_id, "external_table_b", &["id"]),
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        upstream_id.to_string(),
        model(
            upstream_id,
            "upstream_model",
            &[source_a_id, source_b_id],
            &["real_col", "id"],
            "SELECT real_col, id FROM external_table_a UNION ALL SELECT CAST(NULL AS STRING) AS real_col, id FROM external_table_b",
        ),
    );
    nodes.insert(
        union_id.to_string(),
        model(
            union_id,
            "union_model",
            &[upstream_id],
            &["real_col", "id"],
            "WITH branches AS (SELECT real_col, id FROM upstream_model UNION ALL SELECT CAST(NULL AS STRING) AS real_col, CAST(NULL AS INT64) AS id) SELECT real_col, id FROM branches",
        ),
    );

    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

fn bigquery_compound_field_access_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }

    let source_id = "source.proj.external_table_a";
    let upstream_id = "model.proj.upstream_model";
    let array_id = "model.proj.array_model";
    let mut sources = HashMap::new();
    sources.insert(
        source_id.to_string(),
        ManifestSource {
            unique_id: source_id.to_string(),
            name: "external_table_a".to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(&["id", "items_array"]),
            database: None,
            schema: None,
            identifier: None,
        },
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        upstream_id.to_string(),
        model(
            upstream_id,
            "upstream_model",
            &[source_id],
            &["id", "items_array"],
            "SELECT id, items_array FROM external_table_a",
        ),
    );
    nodes.insert(
        array_id.to_string(),
        model(
            array_id,
            "array_model",
            &[upstream_id],
            &["first_item"],
            "SELECT base.items_array[OFFSET(0)] AS first_item FROM upstream_model AS base",
        ),
    );

    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

fn bigquery_struct_field_cross_model_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: Some("p".to_string()),
            schema: Some("d".to_string()),
        }
    }

    let upstream_id = "model.proj.upstream_model";
    let downstream_id = "model.proj.quoting_model";

    let mut nodes = HashMap::new();
    nodes.insert(
        upstream_id.to_string(),
        model(
            upstream_id,
            "upstream_model",
            &[],
            &["user_id", "event"],
            "SELECT user_id, ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t GROUP BY user_id",
        ),
    );
    nodes.insert(
        downstream_id.to_string(),
        model(
            downstream_id,
            "quoting_model",
            &[upstream_id],
            &[
                "user_id",
                "qualified_field",
                "bare_field",
                "plain_column",
            ],
            "SELECT agg.user_id, agg.event.qualified_field AS qualified_field, event.bare_field AS bare_field, agg.user_id AS plain_column FROM `p`.`d`.`upstream_model` AS agg",
        ),
    );

    Manifest {
        nodes,
        ..Default::default()
    }
}

fn same_named_deep_error_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }
    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: Some("p".to_string()),
            schema: Some("d".to_string()),
        }
    }

    let deep_id = "model.proj.deep_model";
    let middle_id = "model.proj.middle_model";
    let target_id = "model.proj.target_model";
    let mut nodes = HashMap::new();
    nodes.insert(
        deep_id.to_string(),
        model(
            deep_id,
            "deep_model",
            &[],
            &["x"],
            "WITH latest AS (SELECT ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM `p`.`d`.`external_table_a` AS t) SELECT event.x AS x FROM latest",
        ),
    );
    nodes.insert(
        middle_id.to_string(),
        model(
            middle_id,
            "middle_model",
            &[deep_id],
            &["x", "y"],
            "SELECT 1 AS x, deep.x AS y FROM `p`.`d`.`deep_model` AS deep",
        ),
    );
    nodes.insert(
        target_id.to_string(),
        model(
            target_id,
            "target_model",
            &[middle_id],
            &["x"],
            "SELECT x FROM `p`.`d`.`middle_model`",
        ),
    );
    Manifest {
        nodes,
        ..Default::default()
    }
}

fn bigquery_unnest_cross_model_manifest() -> Manifest {
    let mut manifest = bigquery_compound_field_access_manifest();
    let mut downstream = manifest
        .nodes
        .remove("model.proj.array_model")
        .expect("array model fixture should exist");
    downstream.unique_id = "model.proj.downstream_model".to_string();
    downstream.name = "downstream_model".to_string();
    downstream.columns = ["item"]
        .into_iter()
        .map(|name| {
            (
                name.to_string(),
                ManifestColumn {
                    name: name.to_string(),
                },
            )
        })
        .collect();
    downstream.compiled_code = Some(
        "SELECT item FROM upstream_model AS base, UNNEST(base.items_array) AS item".to_string(),
    );
    manifest
        .nodes
        .insert("model.proj.downstream_model".to_string(), downstream);
    manifest
}

fn duplicate_column_error_manifest() -> Manifest {
    let mut manifest = make_cross_model_manifest();
    let columns = |names: &[&str]| {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect::<HashMap<_, _>>()
    };
    let model =
        |unique_id: &str, name: &str, dependencies: &[&str], output_columns: &[&str], sql: &str| {
            ManifestNode {
                unique_id: unique_id.to_string(),
                name: name.to_string(),
                alias: None,
                resource_type: "model".to_string(),
                depends_on: DependsOn {
                    nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
                },
                config: ManifestConfig::default(),
                description: None,
                path: None,
                original_file_path: None,
                columns: columns(output_columns),
                compiled_code: Some(sql.to_string()),
                database: None,
                schema: None,
            }
        };

    manifest.nodes.insert(
        "model.proj.left_model".to_string(),
        model(
            "model.proj.left_model",
            "left_model",
            &[],
            &["dup_col"],
            "SELECT * FROM unknown_left_table",
        ),
    );
    manifest.nodes.insert(
        "model.proj.right_model".to_string(),
        model(
            "model.proj.right_model",
            "right_model",
            &[],
            &["dup_col"],
            "SELECT missing_col FROM known_right_table",
        ),
    );
    manifest.nodes.insert(
        "model.proj.duplicate_target".to_string(),
        model(
            "model.proj.duplicate_target",
            "duplicate_target",
            &["model.proj.left_model", "model.proj.right_model"],
            &["dup_col", "other_col"],
            "SELECT l.dup_col AS dup_col, r.dup_col AS other_col FROM left_model AS l JOIN right_model AS r ON l.dup_col = r.dup_col",
        ),
    );
    manifest
}

fn bigquery_nested_star_manifest() -> Manifest {
    fn columns(names: &[&str]) -> HashMap<String, ManifestColumn> {
        names
            .iter()
            .map(|name| {
                (
                    (*name).to_string(),
                    ManifestColumn {
                        name: (*name).to_string(),
                    },
                )
            })
            .collect()
    }

    fn model(
        unique_id: &str,
        name: &str,
        dependencies: &[&str],
        output_columns: &[&str],
        sql: &str,
    ) -> ManifestNode {
        ManifestNode {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: dependencies.iter().map(|id| (*id).to_string()).collect(),
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            compiled_code: Some(sql.to_string()),
            database: None,
            schema: None,
        }
    }

    fn source(unique_id: &str, name: &str, output_columns: &[&str]) -> ManifestSource {
        ManifestSource {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            source_name: "raw".to_string(),
            resource_type: "source".to_string(),
            description: None,
            path: None,
            original_file_path: None,
            columns: columns(output_columns),
            database: None,
            schema: None,
            identifier: None,
        }
    }

    let raw_table = "source.proj.raw.raw_table";
    let raw_aux = "source.proj.raw.raw_aux";
    let stg_source = "model.proj.stg_source";
    let agg_model = "model.proj.agg_model";
    let aux_model = "model.proj.aux_model";
    let downstream_model = "model.proj.downstream_model";

    let mut sources = HashMap::new();
    sources.insert(
        raw_table.to_string(),
        source(
            raw_table,
            "raw_table",
            &["user_id", "updated_at", "col_a", "col_b"],
        ),
    );
    sources.insert(
        raw_aux.to_string(),
        source(raw_aux, "raw_aux", &["user_id", "aux_value"]),
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        stg_source.to_string(),
        model(
            stg_source,
            "stg_source",
            &[raw_table],
            &["user_id", "updated_at", "col_a", "col_b"],
            "SELECT user_id, updated_at, col_a, col_b FROM raw_table",
        ),
    );
    nodes.insert(
        agg_model.to_string(),
        model(
            agg_model,
            "agg_model",
            &[stg_source],
            &["user_id", "event"],
            "SELECT user_id, ARRAY_AGG(t ORDER BY t.updated_at DESC LIMIT 1)[OFFSET(0)] AS event FROM stg_source AS t GROUP BY user_id",
        ),
    );
    nodes.insert(
        aux_model.to_string(),
        model(
            aux_model,
            "aux_model",
            &[raw_aux],
            &["user_id", "aux_value"],
            "SELECT user_id, aux_value FROM raw_aux",
        ),
    );
    nodes.insert(
        downstream_model.to_string(),
        model(
            downstream_model,
            "downstream_model",
            &[agg_model, aux_model],
            &["user_id", "updated_at", "col_a", "col_b", "aux_value"],
            "WITH join_data AS (SELECT base.* EXCEPT (event), base.event.* EXCEPT (user_id), IFNULL(aux.aux_value, 0) AS aux_value FROM agg_model AS base LEFT JOIN aux_model AS aux ON base.user_id = aux.user_id) SELECT * FROM join_data",
        ),
    );

    Manifest {
        nodes,
        sources,
        ..Default::default()
    }
}

mod scenarios;
