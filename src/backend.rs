//! `WasmBackend` — entry point that turns an [`IrModule`] into a
//! WebAssembly component. Implementation is staged across the phases
//! laid out in `PLAN.md`; this module currently holds the stub that
//! wires the trait impl up.

use formalang::ir::IrModule;
use formalang::pipeline::Backend;
use thiserror::Error;

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
    /// Returned by the current stub. Replaced with real variants as
    /// the backend gains capability.
    #[error("not yet implemented")]
    NotYetImplemented,
}

impl Backend for WasmBackend {
    type Output = Vec<u8>;
    type Error = WasmBackendError;

    fn generate(&self, _module: &IrModule) -> Result<Self::Output, Self::Error> {
        Err(WasmBackendError::NotYetImplemented)
    }
}
