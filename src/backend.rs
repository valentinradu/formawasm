//! `WasmBackend` — entry point that turns an [`IrModule`] into a
//! WebAssembly component.
//!
//! The pipeline is: pre-flight rejection (`preflight::check`),
//! public-surface survey (`survey::survey`), core-module lowering
//! (`module_lowering::lower_module`), optional `wasm-opt` post-pass
//! (gated behind the `wasm-opt` cargo feature), WIT generation
//! (`wit::emit_wit`), and component wrap (`component::wrap_component`).
//! `WasmBackend::with_validation()` adds a `wasmparser::Validator`
//! pass against the wrapped bytes; off by default.

use formalang::ir::IrModule;
use formalang::pipeline::Backend;
use thiserror::Error;

use crate::component::{self, ComponentWrapError};
use crate::module_lowering::{self, ModuleLowerError};
use crate::preflight::{self, PreflightError};
use crate::survey;
use crate::wit::{self, WitEmitError};

/// Backend that lowers a typed IR module to a WebAssembly component.
///
/// Construction goes through [`Self::new`]. By default `generate`
/// returns the wrapped component bytes without re-validating them —
/// `wit-component` validates internally during wrap, and the cost
/// of an external pass is unwanted on the hot path. Production
/// callers that want defence against backend bugs can opt into a
/// `wasmparser::Validator` re-check via [`Self::with_validation`].
#[derive(Debug, Default, Clone, Copy)]
#[non_exhaustive]
pub struct WasmBackend {
    /// When `true`, [`Backend::generate`] runs the wrapped component
    /// bytes through `wasmparser::Validator` before returning. Off
    /// by default. Surfaces internal-error conditions
    /// (malformed core wasm slipping through, canonical-ABI
    /// mismatches) as [`WasmBackendError::Validation`] instead of
    /// failing later inside the host runtime.
    validate: bool,
}

impl WasmBackend {
    #[must_use]
    pub const fn new() -> Self {
        Self { validate: false }
    }

    /// Return a new backend that runs the emitted component bytes
    /// through `wasmparser::Validator` before returning them. The
    /// pass is purely defensive — `wit-component`'s internal
    /// validation already covers the canonical ABI, but a malformed
    /// core module slipping through any future `lower_module` change
    /// would surface here as [`WasmBackendError::Validation`]
    /// instead of as a runtime failure inside the embedding host.
    #[must_use]
    pub const fn with_validation(mut self) -> Self {
        self.validate = true;
        self
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

    /// `wasmparser::Validator` rejected the emitted component bytes
    /// when the backend was constructed with [`WasmBackend::with_validation`].
    /// Indicates a backend bug — by the time validation runs, the
    /// upstream pipeline has signed off and `wit-component` has
    /// already wrapped, so a failure here means we emitted
    /// structurally invalid wasm despite both layers passing.
    #[error("wasmparser rejected the emitted component: {reason}")]
    Validation {
        /// `wasmparser`'s diagnostic rendered as a string.
        reason: String,
    },
}

impl Backend for WasmBackend {
    type Output = Vec<u8>;
    type Error = WasmBackendError;

    #[tracing::instrument(skip(self, module), fields(
        functions = module.functions.len(),
        structs = module.structs.len(),
        enums = module.enums.len(),
        impls = module.impls.len(),
    ))]
    fn generate(&self, module: &IrModule) -> Result<Self::Output, Self::Error> {
        tracing::debug!("preflight");
        preflight::check(module)?;
        tracing::debug!("survey");
        let surface = survey::survey(module);
        tracing::debug!("lower_module");
        let core_bytes = module_lowering::lower_module(module)?;
        tracing::debug!(core_bytes = core_bytes.len(), "core module emitted");
        // The `wasm-opt` feature gates a binaryen post-pass that
        // shrinks the emitted core module before it gets wrapped.
        // The pass is only wired in when the feature is on so the
        // default-feature build pulls in zero extra dependencies
        // and pays no runtime cost.
        #[cfg(feature = "wasm-opt")]
        let core_bytes = {
            tracing::debug!("optimize_core_module");
            optimize_core_module(&core_bytes)?
        };
        #[cfg(feature = "wasm-opt")]
        tracing::debug!(core_bytes = core_bytes.len(), "post-wasm-opt size");
        tracing::debug!("emit_wit");
        let wit_text = wit::emit_wit(module, &surface)?;
        tracing::debug!("wrap_component");
        let bytes = component::wrap_component(core_bytes, &wit_text)?;
        tracing::debug!(component_bytes = bytes.len(), "component wrapped");
        if self.validate {
            tracing::debug!("validate");
            validate_component(&bytes)?;
        }
        Ok(bytes)
    }
}

/// Run `wasmparser::Validator` against the wrapped component bytes.
///
/// The default `WasmFeatures` set covers the proposals our emitted
/// modules use (multi-table, reference types, bulk-memory, plus the
/// component model bits enabled by default in 0.247). A validation
/// failure here means the backend emitted structurally invalid wasm
/// despite the upstream pipeline's checks and `wit-component`'s
/// internal validation — surface it as `WasmBackendError::Validation`.
fn validate_component(bytes: &[u8]) -> Result<(), WasmBackendError> {
    use wasmparser::{Validator, WasmFeatures};
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator
        .validate_all(bytes)
        .map_err(|e| WasmBackendError::Validation {
            reason: format!("{e}"),
        })?;
    Ok(())
}

/// Run binaryen's wasm-opt over a core-Wasm byte slice and return
/// the optimized output.
///
/// `WasmBackend::generate` calls this internally between core-module
/// emission and component wrapping when the `wasm-opt` cargo feature
/// is on. The function is also exposed publicly so callers can
/// invoke the same post-pass on bytes they obtained another way
/// (e.g. by calling `module_lowering::lower_module` directly), or
/// run size-comparison benchmarks against the unoptimized output.
///
/// Optimization runs over core wasm only — binaryen's component-
/// model support is still young, and the canonical-ABI wrappers /
/// `cabi_realloc` export the component layer relies on are easier
/// to keep intact when wrapping happens after.
///
/// Our emitted modules use multi-table (closure + method funcref
/// tables), reference types (`funcref` element type), and bulk-
/// memory (`memory.copy` in `__str_concat`). Binaryen's default
/// baseline rejects some of those, so we enable the full feature
/// set up-front.
#[cfg(feature = "wasm-opt")]
pub fn optimize_core_module(core_bytes: &[u8]) -> Result<Vec<u8>, WasmBackendError> {
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
