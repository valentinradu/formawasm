//! formawasm — WebAssembly component backend for the formalang IR.
//!
//! See `README.md` for the project spec and `PLAN.md` for what to do
//! next. This crate re-exports the upstream IR types it consumes so
//! callers do not need a separate `formalang` dependency to use the
//! backend.

mod backend;
pub mod module;
pub mod preflight;
pub mod survey;
pub mod types;

pub use backend::{WasmBackend, WasmBackendError};
pub use module::ModuleBuilder;
pub use preflight::PreflightError;
pub use survey::PublicSurface;
pub use types::TypeMapError;

pub use formalang::ir::{
    EnumId, FunctionId, IrFunction, IrFunctionParam, IrFunctionSig, IrImport, IrImportItem,
    IrModule, ResolvedType, StructId, TraitId,
};
pub use formalang::pipeline::{Backend, IrPass, Pipeline, PipelineError};
