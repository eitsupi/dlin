//! Engine adapters for column lineage: dialects, catalog snapshots and the
//! sqllineage analysis entry points.

pub mod catalog;
pub mod dialect;
pub mod sqllineage;

mod catalog_provider;

pub use catalog::CatalogSnapshot;
pub(crate) use catalog_provider::source_relation;
pub use dialect::{DialectClassification, DlinDialect, REMOVED_DIALECTS};
pub use sqllineage::{
    check_sql_parses, debug_parse_sql_ast_debug, debug_parse_sql_json, debug_trace_column_json,
};
