//! formawasm — WebAssembly component backend for the formalang IR.
//!
//! See `README.md` for the project spec and `CHANGELOG.md` for the
//! phase-by-phase history. This crate re-exports the upstream IR
//! types it consumes so callers do not need a separate `formalang`
//! dependency to use the backend.

mod backend;
pub(crate) mod compound;
pub mod component;
#[cfg(feature = "dwarf")]
pub mod dwarf;
pub(crate) mod ident;
pub mod layout;
pub mod lower;
pub mod module;
pub mod module_lowering;
pub mod preflight;
pub(crate) mod string_pool;
pub mod survey;
pub mod types;
pub mod wit;

#[cfg(feature = "wasm-opt")]
pub use backend::optimize_core_module;
pub use backend::{WasmBackend, WasmBackendError};
pub use component::{ComponentWrapError, wrap_component};
pub use layout::{
    ArrayLayout, EnumLayout, FieldLayout, LayoutError, RangeLayout, StructLayout, VTableLayout,
    VariantLayout, plan_array, plan_enum, plan_range, plan_struct, plan_vtable,
};
pub use lower::{BindingMap, FunctionMap, LowerContext, LowerError};
pub use module::ModuleBuilder;
pub use module_lowering::{ModuleLowerError, lower_module};
pub use preflight::PreflightError;
pub use survey::PublicSurface;
pub use types::TypeMapError;
pub use wit::{WitEmitError, emit_wit};

pub use formalang::ir::{
    EnumId, FunctionId, IrFunction, IrFunctionParam, IrFunctionSig, IrImport, IrImportItem,
    IrModule, IrTrait, ResolvedType, StructId, TraitId,
};
pub use formalang::pipeline::{Backend, IrPass, Pipeline, PipelineError};
