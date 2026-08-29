use super::*;
use std::collections::HashMap;

use crate::parser::manifest::{DependsOn, ManifestColumn, ManifestConfig, ManifestNode};

fn duplicate_column_impact_manifest() -> Manifest {
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

    let source_id = "model.proj.impact_source";
    let star_id = "model.proj.star_downstream";
    let plain_id = "model.proj.plain_downstream";
    let mut nodes = HashMap::new();
    nodes.insert(
        source_id.to_string(),
        model(
            source_id,
            "impact_source",
            &[],
            &["other_col"],
            "SELECT other_col FROM raw_source",
        ),
    );
    nodes.insert(
        star_id.to_string(),
        model(
            star_id,
            "star_downstream",
            &[source_id],
            &["other_col", "dup_col"],
            "WITH known AS (SELECT source.other_col AS other_col FROM impact_source AS source), unknown AS (SELECT * FROM unknown_star) SELECT known.other_col AS other_col, unknown.dup_col AS dup_col FROM known JOIN unknown ON 1 = 1",
        ),
    );
    nodes.insert(
        plain_id.to_string(),
        model(
            plain_id,
            "plain_downstream",
            &[source_id],
            &["other_col", "dup_col"],
            "SELECT source.other_col AS other_col FROM impact_source AS source",
        ),
    );

    Manifest {
        nodes,
        sources: HashMap::new(),
        ..Default::default()
    }
}

mod core {
    use super::*;
    include!("impact/core.rs");
}
fn make_off_path_error_manifest() -> Manifest {
    let mut nodes = HashMap::new();

    // source_model: outputs col_x and col_y
    let mut src_cols = HashMap::new();
    for name in ["col_x", "col_y"] {
        src_cols.insert(
            name.to_string(),
            ManifestColumn {
                name: name.to_string(),
            },
        );
    }
    nodes.insert(
        "model.proj.source_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.source_model".to_string(),
            name: "source_model".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn { nodes: vec![] },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: src_cols,
            compiled_code: Some("select col_x, col_y from raw_table".to_string()),
            database: None,
            schema: None,
        },
    );

    // relevant_model: only col_x from source_model
    let mut rel_cols = HashMap::new();
    rel_cols.insert(
        "col_x".to_string(),
        ManifestColumn {
            name: "col_x".to_string(),
        },
    );
    nodes.insert(
        "model.proj.relevant_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.relevant_model".to_string(),
            name: "relevant_model".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.source_model".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: rel_cols,
            compiled_code: Some("select col_x from source_model".to_string()),
            database: None,
            schema: None,
        },
    );

    // sibling_model: col_y from source_model, plus sibling_fail in YAML (not in SQL)
    let mut sib_cols = HashMap::new();
    sib_cols.insert(
        "col_y".to_string(),
        ManifestColumn {
            name: "col_y".to_string(),
        },
    );
    sib_cols.insert(
        "sibling_fail".to_string(),
        ManifestColumn {
            name: "sibling_fail".to_string(),
        },
    );
    nodes.insert(
        "model.proj.sibling_model".to_string(),
        ManifestNode {
            unique_id: "model.proj.sibling_model".to_string(),
            name: "sibling_model".to_string(),
            alias: None,
            resource_type: "model".to_string(),
            depends_on: DependsOn {
                nodes: vec!["model.proj.source_model".to_string()],
            },
            config: ManifestConfig::default(),
            description: None,
            path: None,
            original_file_path: None,
            columns: sib_cols,
            compiled_code: Some("select col_y from source_model".to_string()),
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

mod edge_cases {
    use super::*;
    include!("impact/edge_cases.rs");
}
