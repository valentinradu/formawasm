//! Bridge between the formalang [`IrModule`] and the core-Wasm
//! [`ModuleBuilder`].
//!
//! [`lower_module`] walks the IR module, builds a [`FunctionMap`]
//! ahead of time so recursive calls resolve, then lowers each
//! function body and plugs it into a fresh `ModuleBuilder` before
//! returning the encoded module bytes.

use formalang::ir::{BindingId, FunctionId, IrFunction, IrFunctionParam, IrModule, ResolvedType};
use wasm_encoder::ValType;

use crate::lower::{FunctionMap, LowerError, lower_function_body};
use crate::module::ModuleBuilder;
use crate::types::{TypeMapError, resolved_value_type, result_types};

/// `(BindingId, ValType)` pair recording one function parameter's
/// binding identity alongside its wasm value type.
type ParamBinding = (BindingId, ValType);

/// Errors produced by [`lower_module`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModuleLowerError {
    /// An expression-level lowering failure surfaced from
    /// [`lower_function_body`] or one of its callees.
    #[error(transparent)]
    Lower(#[from] LowerError),

    /// A function carries no body (likely an `extern` declaration).
    /// Externs land in Phase 4 alongside cross-module imports.
    #[error("function '{name}' has no body — extern functions are not yet supported (Phase 4)")]
    ExternFunction {
        /// Source-level name of the offending function.
        name: String,
    },

    /// A function parameter is missing its type annotation. Only
    /// `self` parameters legitimately omit the type, and Phase 1a
    /// does not yet emit methods.
    #[error("parameter '{name}' on function '{function}' is missing a type annotation")]
    MissingParamType {
        /// Containing function name.
        function: String,
        /// Parameter name.
        name: String,
    },

    /// A type-mapping failure surfaced while lowering a function
    /// signature.
    #[error(transparent)]
    TypeMap(#[from] TypeMapError),

    /// More than `u32::MAX` functions in a module — a wasm-encoding
    /// limit, not a realistic case for hand-written code.
    #[error("module has more than u32::MAX functions")]
    TooManyFunctions,
}

/// Walk `module` and return the encoded core-Wasm bytes.
///
/// Top-level non-extern functions are emitted in declaration order;
/// each function's wasm index equals its position in
/// `module.functions` (since Phase 1a has no imports). Nested
/// `module.modules` and `module.impls` are not yet walked — they
/// land in Phase 1b alongside method dispatch.
pub fn lower_module(module: &IrModule) -> Result<Vec<u8>, ModuleLowerError> {
    let mut function_map = FunctionMap::new();

    for (i, _) in module.functions.iter().enumerate() {
        let wasm_idx = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        function_map.insert(FunctionId(wasm_idx), wasm_idx);
    }

    let mut builder = ModuleBuilder::new();
    for f in &module.functions {
        emit_function(f, &mut builder, &function_map)?;
    }
    Ok(builder.finish())
}

fn emit_function(
    f: &IrFunction,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
) -> Result<(), ModuleLowerError> {
    if f.is_extern() {
        return Err(ModuleLowerError::ExternFunction {
            name: f.name.clone(),
        });
    }
    let body_expr = f
        .body
        .as_ref()
        .ok_or_else(|| ModuleLowerError::ExternFunction {
            name: f.name.clone(),
        })?;

    let (param_valtypes, param_bindings) = lower_params(f)?;
    let result_valtypes = result_types(f.return_type.as_ref())?;

    let body = lower_function_body(body_expr, &param_bindings, function_map)?;
    let wasm_idx = builder.declare_function_with_body(&param_valtypes, &result_valtypes, &body);
    // Phase 1a: every non-extern top-level function is exported by
    // its source-level name. Survey-driven export filtering lands
    // alongside WIT generation in the next mc.
    builder.export_function(&f.name, wasm_idx);
    Ok(())
}

fn lower_params(f: &IrFunction) -> Result<(Vec<ValType>, Vec<ParamBinding>), ModuleLowerError> {
    let mut valtypes = Vec::with_capacity(f.params.len());
    let mut bindings = Vec::with_capacity(f.params.len());

    for param in &f.params {
        let ty = param
            .ty
            .as_ref()
            .ok_or_else(|| ModuleLowerError::MissingParamType {
                function: f.name.clone(),
                name: param.name.clone(),
            })?;
        let vt = param_value_type(param, ty, &f.name)?;
        valtypes.push(vt);
        bindings.push((param.binding_id, vt));
    }

    Ok((valtypes, bindings))
}

fn param_value_type(
    param: &IrFunctionParam,
    ty: &ResolvedType,
    function: &str,
) -> Result<ValType, ModuleLowerError> {
    resolved_value_type(ty)?.ok_or_else(|| ModuleLowerError::MissingParamType {
        function: function.to_owned(),
        name: param.name.clone(),
    })
}
