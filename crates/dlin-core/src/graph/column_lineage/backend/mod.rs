#![allow(dead_code)]
//! Backend abstraction and dispatch for column lineage.

pub mod catalog;
pub mod dialect;
pub mod sqllineage;
pub mod types;

mod catalog_provider;

#[allow(unused_imports)]
pub use catalog::CatalogSnapshot;
#[allow(unused_imports)]
pub(crate) use catalog_provider::SqllineageCatalogProvider;
#[allow(unused_imports)]
pub use dialect::{DialectClassification, DlinDialect, REMOVED_DIALECTS};
#[allow(unused_imports)]
pub use sqllineage::{
    SqllineageBackend, check_sql_parses, debug_parse_sql_ast_debug, debug_parse_sql_json,
    debug_trace_column_json,
};
#[allow(unused_imports)]
pub use types::*;

/// Backend identities for the column-lineage dispatch layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendId {
    Sqllineage,
}

impl BackendId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Sqllineage => "sqllineage",
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

/// Concrete backend variants.
pub enum Backend {
    Sqllineage(SqllineageBackend),
}

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

#[cfg(test)]
mod tests {
    #[test]
    fn sqlparser_version_matches_sqllineage() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(lock_path) = manifest_dir
            .ancestors()
            .map(|directory| directory.join("Cargo.lock"))
            .find(|path| path.is_file())
        else {
            return;
        };
        let lock = std::fs::read_to_string(&lock_path)
            .unwrap_or_else(|error| panic!("could not read {}: {error}", lock_path.display()));
        let sqlparser_packages: Vec<&str> = lock
            .split("[[package]]")
            .filter(|block| {
                block
                    .lines()
                    .any(|line| line.trim() == "name = \"sqlparser\"")
            })
            .collect();

        assert_eq!(
            sqlparser_packages.len(),
            1,
            "dlin and sqllineage must resolve one shared sqlparser package"
        );
    }
}
