//! `WasmBackend` — entry point that turns an [`IrModule`] into a
//! WebAssembly component.
//!
//! The Phase 1a pipeline is end-to-end: pre-flight rejection, public-
//! surface survey, core-module lowering, WIT generation, and component
//! wrap. Phases 1b+ extend the lowering and WIT layers without
//! changing the shape of this entry point.

use formalang::ir::IrModule;
use formalang::pipeline::Backend;
use thiserror::Error;

use crate::component::{self, ComponentWrapError};
use crate::module_lowering::{self, ModuleLowerError};
use crate::preflight::{self, PreflightError};
use crate::survey;
use crate::wit::{self, WitEmitError};

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

    /// Core-module lowering surfaced an error.
    #[error(transparent)]
    ModuleLower(#[from] ModuleLowerError),

    /// WIT emission rejected the public surface — typically a
    /// non-primitive type appearing in an exported function signature.
    #[error(transparent)]
    WitEmit(#[from] WitEmitError),

    /// `wit-component` failed to wrap the core module into a
    /// component artifact.
    #[error(transparent)]
    ComponentWrap(#[from] ComponentWrapError),
}

impl Backend for WasmBackend {
    type Output = Vec<u8>;
    type Error = WasmBackendError;

    fn generate(&self, module: &IrModule) -> Result<Self::Output, Self::Error> {
        preflight::check(module)?;
        let surface = survey::survey(module);
        let core_bytes = module_lowering::lower_module(module)?;
        let wit_text = wit::emit_wit(module, &surface)?;
        let bytes = component::wrap_component(core_bytes, &wit_text)?;
        Ok(bytes)
    }
}
