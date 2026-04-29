//! formawasm — WebAssembly component backend for the formalang IR.
//!
//! See `README.md` for the project spec and `PLAN.md` for what to do
//! next. This crate re-exports the upstream IR types it consumes so
//! callers do not need a separate `formalang` dependency to use the
//! backend.

mod backend;
pub mod lower;
pub mod module;
pub mod module_lowering;
pub mod preflight;
pub mod survey;
pub mod types;
pub mod wit;

pub use backend::{WasmBackend, WasmBackendError};
pub use lower::{BindingMap, FunctionMap, LowerContext, LowerError};
pub use module::ModuleBuilder;
pub use module_lowering::{ModuleLowerError, lower_module};
pub use preflight::PreflightError;
pub use survey::PublicSurface;
pub use types::TypeMapError;
pub use wit::{WitEmitError, emit_wit};

pub use formalang::ir::{
    EnumId, FunctionId, IrFunction, IrFunctionParam, IrFunctionSig, IrImport, IrImportItem,
    IrModule, ResolvedType, StructId, TraitId,
};
pub use formalang::pipeline::{Backend, IrPass, Pipeline, PipelineError};
