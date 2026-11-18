//! Compare dependency extraction by the Jinja interpreter with the
//! placeholder render, for every SQL file of the given dbt projects.
//!
//! ```sh
//! cargo run --release -p dlin-core --example compare_extraction -- <project_dir>...
//! ```
//!
//! Prints one JSON report per project. Differences are classified by how the
//! interpreter treated the call; `unexplained` ones need a closer look.

use std::path::Path;

fn main() -> anyhow::Result<()> {
    let mut reports = serde_json::Map::new();
    for dir in std::env::args().skip(1) {
        let report = dlin_core::parser::jinja::interp::compare::compare_project(Path::new(&dir))?;
        reports.insert(dir, serde_json::to_value(report)?);
    }
    println!("{}", serde_json::to_string_pretty(&reports)?);
    Ok(())
}
