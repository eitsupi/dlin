#![cfg(feature = "column-lineage")]

use dlin_core::graph::column_lineage::{DlinDialect, debug_trace_column_json};

#[test]
fn debug_trace_keeps_case_insensitive_column_lookup() {
    for dialect in [
        DlinDialect::Generic,
        DlinDialect::Snowflake,
        DlinDialect::PostgreSQL,
    ] {
        for name in ["id", "ID"] {
            let json = debug_trace_column_json("SELECT id FROM events", dialect, None, name, false)
                .expect("debug trace should use the same lookup policy as model analysis");
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["mappings"].as_array().unwrap().len(), 1);
        }
    }
}
