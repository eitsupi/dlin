#![allow(dead_code)]
//! Backend abstraction and dispatch for column lineage.

pub mod catalog;
pub mod dialect;
pub mod polyglot;
pub mod types;

#[allow(unused_imports)]
pub use catalog::CatalogSnapshot;
#[allow(unused_imports)]
pub use dialect::DlinDialect;
#[allow(unused_imports)]
pub use polyglot::{
    check_sql_parses, debug_parse_sql_ast_debug, debug_parse_sql_json, debug_trace_column_json,
};
#[allow(unused_imports)]
pub use types::*;

/// Backend identities for the column-lineage dispatch layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendId {
    Polyglot,
}

impl BackendId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Polyglot => "polyglot",
        }
    }
}

/// Uniform interface for lineages backends.
pub trait LineageBackend: Send + Sync {
    fn id(&self) -> BackendId;
    fn discover_output_columns(
        &self,
        request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError>;
    fn analyze(&self, request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError>;
}

/// Concrete backend variant (only `Polyglot` exists today).
pub enum Backend {
    Polyglot(PolyglotBackend),
}

impl LineageBackend for Backend {
    fn id(&self) -> BackendId {
        match self {
            Self::Polyglot(_) => BackendId::Polyglot,
        }
    }

    fn discover_output_columns(
        &self,
        request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError> {
        match self {
            Self::Polyglot(backend) => backend.discover_output_columns(request),
        }
    }

    fn analyze(&self, request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError> {
        match self {
            Self::Polyglot(backend) => backend.analyze(request),
        }
    }
}

/// The `polyglot-sql`-backed lineage backend.
pub struct PolyglotBackend;

impl PolyglotBackend {
    pub const fn new() -> Self {
        Self
    }
}

impl LineageBackend for PolyglotBackend {
    fn id(&self) -> BackendId {
        BackendId::Polyglot
    }

    fn discover_output_columns(
        &self,
        _request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError> {
        crate::graph::column_lineage::backend::polyglot::discover_output_columns(_request)
    }

    fn analyze(&self, _request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError> {
        crate::graph::column_lineage::backend::polyglot::analyze(_request)
    }
}
