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

    /// The optional `wasm-opt` post-pass (gated behind the
    /// `wasm-opt` cargo feature) failed to optimize the emitted
    /// core module. The wrapped string carries binaryen's diagnostic.
    #[cfg(feature = "wasm-opt")]
    #[error("wasm-opt post-pass failed: {reason}")]
    WasmOpt {
        /// Binaryen's diagnostic rendered as a string.
        reason: String,
    },
}

impl Backend for WasmBackend {
    type Output = Vec<u8>;
    type Error = WasmBackendError;

    fn generate(&self, module: &IrModule) -> Result<Self::Output, Self::Error> {
        preflight::check(module)?;
        let surface = survey::survey(module);
        let core_bytes = module_lowering::lower_module(module)?;
        // The `wasm-opt` feature gates a binaryen post-pass that
        // shrinks the emitted core module before it gets wrapped.
        // The pass is only wired in when the feature is on so the
        // default-feature build pulls in zero extra dependencies
        // and pays no runtime cost.
        #[cfg(feature = "wasm-opt")]
        let core_bytes = optimize_core_module(&core_bytes)?;
        let wit_text = wit::emit_wit(module, &surface)?;
        let bytes = component::wrap_component(core_bytes, &wit_text)?;
        Ok(bytes)
    }
}

/// Run binaryen's wasm-opt over the emitted core-module bytes.
///
/// Optimization happens before component wrapping so the post-pass
/// only sees core wasm: binaryen's component-model support is still
/// young, and the canonical-ABI wrappers / `cabi_realloc` export the
/// component layer relies on are easier to keep intact when wrapping
/// happens after.
///
/// Our emitted modules use multi-table (closure + method funcref
/// tables), reference types (`funcref` element type), and bulk-
/// memory (`memory.copy` in `__str_concat`). Binaryen's default
/// baseline rejects some of those, so we enable the full feature
/// set up-front.
#[cfg(feature = "wasm-opt")]
fn optimize_core_module(core_bytes: &[u8]) -> Result<Vec<u8>, WasmBackendError> {
    use std::io::{Read as _, Write as _};

    use wasm_opt::OptimizationOptions;

    let dir = tempfile::tempdir().map_err(|e| WasmBackendError::WasmOpt {
        reason: format!("tempdir: {e}"),
    })?;
    let in_path = dir.path().join("in.wasm");
    let out_path = dir.path().join("out.wasm");
    {
        let mut f = std::fs::File::create(&in_path).map_err(|e| WasmBackendError::WasmOpt {
            reason: format!("create input: {e}"),
        })?;
        f.write_all(core_bytes)
            .map_err(|e| WasmBackendError::WasmOpt {
                reason: format!("write input: {e}"),
            })?;
    }
    OptimizationOptions::new_optimize_for_size()
        .enable_feature(wasm_opt::Feature::All)
        .run(&in_path, &out_path)
        .map_err(|e| WasmBackendError::WasmOpt {
            reason: format!("{e:#}"),
        })?;
    let mut optimized = Vec::new();
    std::fs::File::open(&out_path)
        .map_err(|e| WasmBackendError::WasmOpt {
            reason: format!("open output: {e}"),
        })?
        .read_to_end(&mut optimized)
        .map_err(|e| WasmBackendError::WasmOpt {
            reason: format!("read output: {e}"),
        })?;
    Ok(optimized)
}
