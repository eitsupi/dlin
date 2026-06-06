use super::*;
use crate::parser::discovery::DiscoveredFiles;
use std::fs;
use std::path::PathBuf;

fn setup_temp_project() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let project_dir = tmp.path().to_path_buf();

    // Create model files
    let models_dir = project_dir.join("models");
    fs::create_dir_all(&models_dir).unwrap();

    fs::write(
        models_dir.join("stg_orders.sql"),
        "SELECT * FROM {{ source('raw', 'orders') }}",
    )
    .unwrap();

    fs::write(
        models_dir.join("orders.sql"),
        "SELECT * FROM {{ ref('stg_orders') }}",
    )
    .unwrap();

    // Create schema YAML with source definition
    fs::write(
        models_dir.join("schema.yml"),
        r#"
version: 2
sources:
  - name: raw
    tables:
      - name: orders
        description: "Raw orders table"
models:
  - name: stg_orders
    description: "Staged orders"
"#,
    )
    .unwrap();

    (tmp, project_dir)
}

mod build_graph;
mod generic_tests;
mod ref_parsing;
mod versioned;
