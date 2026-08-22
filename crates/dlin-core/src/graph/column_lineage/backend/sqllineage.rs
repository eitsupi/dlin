use super::{
    BackendAnalysis, BackendError, BackendErrorKind, BackendId, LineageBackend, LineageRequest,
    OutputDiscovery, OutputDiscoveryRequest,
};

/// Skeleton for the sqllineage-backed lineage implementation.
pub struct SqllineageBackend;

impl SqllineageBackend {
    pub const fn new() -> Self {
        Self
    }
}

fn not_implemented() -> BackendError {
    BackendError {
        kind: BackendErrorKind::Internal,
        message: "sqllineage backend is not implemented yet".to_string(),
    }
}

impl LineageBackend for SqllineageBackend {
    fn id(&self) -> BackendId {
        BackendId::Sqllineage
    }

    fn discover_output_columns(
        &self,
        _request: &OutputDiscoveryRequest<'_>,
    ) -> Result<OutputDiscovery, BackendError> {
        Err(not_implemented())
    }

    fn analyze(&self, _request: &LineageRequest<'_>) -> Result<BackendAnalysis, BackendError> {
        Err(not_implemented())
    }
}
