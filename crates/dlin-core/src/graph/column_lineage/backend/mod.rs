#![allow(dead_code)]
//! Backend abstraction and dispatch for column lineage.

pub mod catalog;
pub mod dialect;
pub mod sqllineage;
#[cfg(test)]
pub mod types;

mod catalog_provider;

#[allow(unused_imports)]
pub use catalog::CatalogSnapshot;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use catalog_provider::SqllineageCatalogProvider;
pub(crate) use catalog_provider::relation_ref_from_engine;
pub(crate) use catalog_provider::source_relation;
#[allow(unused_imports)]
pub use dialect::{DialectClassification, DlinDialect, REMOVED_DIALECTS};
#[cfg(test)]
pub(crate) use sqllineage::SqllineageBackend;
#[allow(unused_imports)]
pub use sqllineage::{
    check_sql_parses, debug_parse_sql_ast_debug, debug_parse_sql_json, debug_trace_column_json,
};
#[cfg(test)]
#[allow(unused_imports)]
pub use types::*;

/// Backend identities for the column-lineage dispatch layer.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendId {
    Sqllineage,
}

/// Uniform interface for lineages backends.
#[cfg(test)]
pub trait LineageBackend: Send + Sync {
    fn id(&self) -> BackendId;
    fn discover_output_columns(
        &self,
        request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError>;
    fn analyze(&self, request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError>;
}

/// Concrete backend variants.
#[cfg(test)]
pub enum Backend {
    Sqllineage(SqllineageBackend),
}

#[cfg(test)]
impl LineageBackend for Backend {
    fn id(&self) -> BackendId {
        match self {
            Self::Sqllineage(_) => BackendId::Sqllineage,
        }
    }

    fn discover_output_columns(
        &self,
        request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError> {
        match self {
            Self::Sqllineage(backend) => backend.discover_output_columns(request),
        }
    }

    fn analyze(&self, request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError> {
        match self {
            Self::Sqllineage(backend) => backend.analyze(request),
        }
    }
}

#[cfg(test)]
pub(crate) fn backend_for_tests(_id: BackendId) -> Backend {
    Backend::Sqllineage(SqllineageBackend::new())
}
