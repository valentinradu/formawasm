//! `WasmBackend` — entry point that turns an [`IrModule`] into a
//! WebAssembly component. Implementation is staged across the phases
//! laid out in `PLAN.md`; this module currently runs pre-flight checks
//! and stubs the rest of the pipeline.

use formalang::ir::IrModule;
use formalang::pipeline::Backend;
use thiserror::Error;

use crate::preflight::{self, PreflightError};

/// Backend that lowers a typed IR module to a WebAssembly component.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct WasmBackend;

impl WasmBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Errors produced by [`WasmBackend::generate`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WasmBackendError {
    /// Pre-flight rejected the module because of an upstream-pipeline
    /// invariant violation.
    #[error(transparent)]
    Preflight(#[from] PreflightError),

    /// Returned when the backend reaches a code path that hasn't been
    /// implemented yet. Replaced with concrete variants as each phase
    /// lands.
    #[error("not yet implemented")]
    NotYetImplemented,
}

impl Backend for WasmBackend {
    type Output = Vec<u8>;
    type Error = WasmBackendError;

    fn generate(&self, module: &IrModule) -> Result<Self::Output, Self::Error> {
        preflight::check(module)?;
        Err(WasmBackendError::NotYetImplemented)
    }
}
