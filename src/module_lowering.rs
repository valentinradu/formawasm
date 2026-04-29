//! Bridge between the formalang [`IrModule`] and the core-Wasm
//! [`ModuleBuilder`].
//!
//! [`lower_module`] walks the IR module, builds a [`FunctionMap`]
//! ahead of time so recursive calls resolve, then lowers each
//! function body and plugs it into a fresh `ModuleBuilder` before
//! returning the encoded module bytes.

use formalang::ir::{
    BindingId, FunctionId, ImplId, ImplTarget, IrFunction, IrFunctionParam, IrImpl, IrModule,
    MethodIdx, ResolvedType, StructId,
};
use wasm_encoder::ValType;

use crate::lower::{FunctionMap, LowerError, MethodMap, lower_function_body_in_module};
use crate::module::ModuleBuilder;
use crate::types::{TypeMapError, body_result_types, body_value_type};

/// `(BindingId, ValType)` pair recording one function parameter's
/// binding identity alongside its wasm value type.
type ParamBinding = (BindingId, ValType);

/// Errors produced by [`lower_module`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModuleLowerError {
    /// An expression-level lowering failure surfaced from
    /// [`lower_function_body_in_module`] or one of its callees.
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
/// Top-level non-extern functions are emitted after the bump-
/// allocator runtime helper, so `FunctionId(i)` maps to wasm index
/// `i + 1` (the helper occupies wasm index 0). Methods declared in
/// `module.impls` follow at wasm indices past the top-level
/// functions; the [`MethodMap`] hides the per-impl indexing from
/// call-site lowerings. [`FunctionMap`] hides the user-offset shift.
///
/// Nested `module.modules` are still not walked.
pub fn lower_module(module: &IrModule) -> Result<Vec<u8>, ModuleLowerError> {
    let mut builder = ModuleBuilder::new();
    let bump_idx = builder.declare_bump_allocator();
    let user_offset = bump_idx
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;

    let mut function_map = FunctionMap::new();
    for (i, _) in module.functions.iter().enumerate() {
        let id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let wasm_idx = id_raw
            .checked_add(user_offset)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        function_map.insert(FunctionId(id_raw), wasm_idx);
    }

    // Pre-assign wasm function indices for every method in every
    // impl, so MethodCall lowering can resolve targets without
    // re-walking the impls.
    let methods_offset = user_offset
        .checked_add(
            u32::try_from(module.functions.len())
                .map_err(|_| ModuleLowerError::TooManyFunctions)?,
        )
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    let mut method_map = MethodMap::new();
    let mut method_counter: u32 = 0;
    for (i, imp) in module.impls.iter().enumerate() {
        if imp.is_extern {
            continue;
        }
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        for (j, _) in imp.functions.iter().enumerate() {
            let m_idx_raw = u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let wasm_idx = methods_offset
                .checked_add(method_counter)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            method_map.insert((ImplId(impl_id_raw), MethodIdx(m_idx_raw)), wasm_idx);
            method_counter = method_counter
                .checked_add(1)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
        }
    }

    for f in &module.functions {
        emit_function(
            f,
            &mut builder,
            &function_map,
            &method_map,
            module,
            bump_idx,
            None,
        )?;
    }
    for (i, imp) in module.impls.iter().enumerate() {
        if imp.is_extern {
            continue;
        }
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        emit_impl(
            imp,
            ImplId(impl_id_raw),
            &mut builder,
            &function_map,
            &method_map,
            module,
            bump_idx,
        )?;
    }
    Ok(builder.finish())
}

fn emit_impl(
    imp: &IrImpl,
    _impl_id: ImplId,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
    method_map: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
) -> Result<(), ModuleLowerError> {
    let self_struct_id = match imp.target {
        ImplTarget::Struct(id) => Some(id),
        ImplTarget::Enum(_) => None, // enum methods land later; tests cover struct methods
    };
    for f in &imp.functions {
        emit_function(
            f,
            builder,
            function_map,
            method_map,
            module,
            bump_allocator,
            self_struct_id,
        )?;
    }
    Ok(())
}

fn emit_function(
    f: &IrFunction,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
    method_map: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    impl_self_struct_id: Option<StructId>,
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
    let result_valtypes = body_result_types(f.return_type.as_ref())?;

    let self_struct_id = impl_self_struct_id.or_else(|| detect_self_struct(f));
    let body = lower_function_body_in_module(
        body_expr,
        &param_bindings,
        function_map,
        method_map,
        module,
        bump_allocator,
        self_struct_id,
    )?;
    let wasm_idx = builder.declare_function_with_body(&param_valtypes, &result_valtypes, &body);
    // Phase 1a: every non-extern top-level function is exported by
    // its source-level name. Survey-driven export filtering lands
    // alongside WIT generation in the next mc. Impl methods do NOT
    // get exported — name collisions between different impls would
    // otherwise produce a malformed module.
    if impl_self_struct_id.is_none() {
        builder.export_function(&f.name, wasm_idx);
    }
    Ok(())
}

/// Identify the enclosing impl's struct id when `f` is a method.
///
/// Phase 1b mc7 detects methods purely by shape: the first parameter
/// is named `self` and has a `ResolvedType::Struct(_)` type. Once impl
/// walking lands the right answer will come from the surrounding
/// `IrImpl`, but for now this is enough to lower `SelfFieldRef`
/// inside hand-built `module.functions` test fixtures.
fn detect_self_struct(f: &IrFunction) -> Option<StructId> {
    let first = f.params.first()?;
    if first.name != "self" {
        return None;
    }
    match first.ty.as_ref()? {
        ResolvedType::Struct(id) => Some(*id),
        ResolvedType::Primitive(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => None,
    }
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
    body_value_type(ty)?.ok_or_else(|| ModuleLowerError::MissingParamType {
        function: function.to_owned(),
        name: param.name.clone(),
    })
}
