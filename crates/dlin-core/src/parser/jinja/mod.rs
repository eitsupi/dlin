use super::sql::{RefCall, SourceCall, SqlConfig};

#[doc(hidden)]
pub mod interp;

/// The dependencies and config extracted from a dbt Jinja SQL template.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct JinjaExtraction {
    pub refs: Vec<RefCall>,
    pub sources: Vec<SourceCall>,
    pub config: SqlConfig,
}
