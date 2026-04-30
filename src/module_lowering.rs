//! Bridge between the formalang [`IrModule`] and the core-Wasm
//! [`ModuleBuilder`].
//!
//! [`lower_module`] walks the IR module, builds a [`FunctionMap`]
//! ahead of time so recursive calls resolve, then lowers each
//! function body and plugs it into a fresh `ModuleBuilder` before
//! returning the encoded module bytes.

use std::collections::HashMap;

use formalang::ir::{
    BindingId, FunctionId, ImplId, ImplTarget, IrBlockStatement, IrExpr, IrFunction,
    IrFunctionParam, IrImpl, IrModule, MethodIdx, ResolvedType, StructId,
};
use wasm_encoder::ValType;

use crate::ident::kebab_case;
use crate::lower::{
    ClosureCallContext, FunctionMap, LowerError, MethodMap, lower_function_body_in_module,
};
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

    let closure_plumbing = build_closure_plumbing(&mut builder, module, &function_map)?;
    let closure_ctx = closure_plumbing.as_ref().map(|p| ClosureCallContext {
        table_idx: p.table_idx,
        funcref_indices: &p.funcref_indices,
        type_indices: &p.type_indices,
    });

    for f in &module.functions {
        emit_function(
            f,
            &mut builder,
            &function_map,
            &method_map,
            module,
            bump_idx,
            None,
            closure_ctx.as_ref(),
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
            closure_ctx.as_ref(),
        )?;
    }
    Ok(builder.finish())
}

/// Owned plumbing for indirect closure invocation.
///
/// `lower_module` builds this once after function-index resolution
/// and before any function bodies emit instructions. The returned
/// owned maps are the storage backing the
/// [`ClosureCallContext`] borrows that flow into per-function
/// lowering — keeping the maps owned here avoids dangling-borrow
/// problems with the `ModuleBuilder` field accesses that come later.
struct ClosurePlumbing {
    table_idx: u32,
    funcref_indices: HashMap<FunctionId, u32>,
    type_indices: HashMap<ResolvedType, u32>,
}

/// Walk `module` and:
///
/// 1. Find every closure-callable function (named `__closure*` per
///    the closure-conversion convention) and register it in a fresh
///    funcref table.
/// 2. Find every `IrExpr::CallClosure` site and pre-register a wasm
///    type signature for each unique closure type seen, with an i32
///    `env_ptr` prepended to the declared parameters.
/// 3. Return the per-module plumbing the per-function lowerings need
///    to emit `call_indirect`.
///
/// Returns `None` when the module has no closure-callable functions
/// — the caller leaves the `ClosureCallContext` unset, and a stray
/// `IrExpr::CallClosure` falls through to a typed
/// [`LowerError::MissingContext`] downstream.
fn build_closure_plumbing(
    builder: &mut ModuleBuilder,
    module: &IrModule,
    function_map: &FunctionMap,
) -> Result<Option<ClosurePlumbing>, ModuleLowerError> {
    let closure_funcs: Vec<(FunctionId, u32)> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name.starts_with("__closure"))
        .map(|(i, _)| -> Result<(FunctionId, u32), ModuleLowerError> {
            let id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let id = FunctionId(id_raw);
            let wasm_idx = function_map
                .get(id)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            Ok((id, wasm_idx))
        })
        .collect::<Result<_, _>>()?;

    if closure_funcs.is_empty() {
        return Ok(None);
    }

    let num_closures =
        u32::try_from(closure_funcs.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
    let table_idx = builder.declare_closure_table(num_closures);
    let wasm_func_indices: Vec<u32> = closure_funcs.iter().map(|(_, w)| *w).collect();
    builder.populate_closure_table(&wasm_func_indices);

    let mut funcref_indices = HashMap::new();
    for (slot, (id, _)) in closure_funcs.into_iter().enumerate() {
        let slot_u32 = u32::try_from(slot).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        funcref_indices.insert(id, slot_u32);
    }

    let mut type_indices: HashMap<ResolvedType, u32> = HashMap::new();
    for f in &module.functions {
        if let Some(body) = &f.body {
            collect_call_closure_types(body, builder, &mut type_indices)?;
        }
    }
    for imp in &module.impls {
        if imp.is_extern {
            continue;
        }
        for f in &imp.functions {
            if let Some(body) = &f.body {
                collect_call_closure_types(body, builder, &mut type_indices)?;
            }
        }
    }

    Ok(Some(ClosurePlumbing {
        table_idx,
        funcref_indices,
        type_indices,
    }))
}

/// Walk `expr`, register a wasm type for every unique closure type
/// seen at an `IrExpr::CallClosure` site, and update `out` with the
/// `(closure_ty, type_idx)` pair.
fn collect_call_closure_types(
    expr: &IrExpr,
    builder: &mut ModuleBuilder,
    out: &mut HashMap<ResolvedType, u32>,
) -> Result<(), ModuleLowerError> {
    if let IrExpr::CallClosure { closure, .. } = expr {
        let ty = closure.ty().clone();
        if let std::collections::hash_map::Entry::Vacant(slot) = out.entry(ty.clone()) {
            let type_idx = register_closure_call_type(&ty, builder)?;
            slot.insert(type_idx);
        }
    }
    walk_children(expr, &mut |child| {
        collect_call_closure_types(child, builder, out)
    })
}

/// Register a wasm `func` type for `closure_ty`'s `call_indirect`
/// signature. Prepends one i32 (`env_ptr`) to the closure's declared
/// parameters and uses the body-side wasm valtype for each
/// (aggregates lower as i32 pointers).
fn register_closure_call_type(
    closure_ty: &ResolvedType,
    builder: &mut ModuleBuilder,
) -> Result<u32, ModuleLowerError> {
    let ResolvedType::Closure {
        param_tys,
        return_ty,
    } = closure_ty
    else {
        return Err(ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("call_indirect type for non-Closure {closure_ty:?}"),
        }));
    };
    let mut params: Vec<ValType> = Vec::with_capacity(param_tys.len().saturating_add(1));
    params.push(ValType::I32); // env_ptr
    for (_, ty) in param_tys {
        let vt = body_value_type(ty)?.ok_or_else(|| {
            ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                kind: format!("closure parameter of type {ty:?} (Never)"),
            })
        })?;
        params.push(vt);
    }
    let results: Vec<ValType> = body_value_type(return_ty)?
        .map(|vt| vec![vt])
        .unwrap_or_default();
    Ok(builder.declare_type(&params, &results))
}

/// Walk every child sub-expression of `expr` and apply `visit` to
/// each. Used by [`collect_call_closure_types`] so the per-closure
/// pre-registration walk doesn't need its own exhaustive variant
/// match.
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive walk over every IrExpr variant — extracting arms hides which variants are leaves vs. recursive"
)]
fn walk_children<F>(expr: &IrExpr, visit: &mut F) -> Result<(), ModuleLowerError>
where
    F: FnMut(&IrExpr) -> Result<(), ModuleLowerError>,
{
    match expr {
        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::Closure { .. } => Ok(()),
        IrExpr::StructInst { fields, .. } | IrExpr::EnumInst { fields, .. } => {
            for (_, _, e) in fields {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::Tuple { fields, .. } => {
            for (_, e) in fields {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::Array { elements, .. } => {
            for e in elements {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::FieldAccess { object, .. } => visit(object),
        IrExpr::BinaryOp { left, right, .. } => {
            visit(left)?;
            visit(right)
        }
        IrExpr::UnaryOp { operand, .. } => visit(operand),
        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit(condition)?;
            visit(then_branch)?;
            if let Some(e) = else_branch {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::For {
            collection, body, ..
        } => {
            visit(collection)?;
            visit(body)
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            visit(scrutinee)?;
            for arm in arms {
                visit(&arm.body)?;
            }
            Ok(())
        }
        IrExpr::FunctionCall { args, .. } => {
            for (_, a) in args {
                visit(a)?;
            }
            Ok(())
        }
        IrExpr::CallClosure { closure, args, .. } => {
            visit(closure)?;
            for (_, a) in args {
                visit(a)?;
            }
            Ok(())
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            visit(receiver)?;
            for (_, a) in args {
                visit(a)?;
            }
            Ok(())
        }
        IrExpr::ClosureRef { env_struct, .. } => visit(env_struct),
        IrExpr::DictLiteral { entries, .. } => {
            for (k, v) in entries {
                visit(k)?;
                visit(v)?;
            }
            Ok(())
        }
        IrExpr::DictAccess { dict, key, .. } => {
            visit(dict)?;
            visit(key)
        }
        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                match stmt {
                    IrBlockStatement::Let { value, .. } => visit(value)?,
                    IrBlockStatement::Assign { target, value } => {
                        visit(target)?;
                        visit(value)?;
                    }
                    IrBlockStatement::Expr(e) => visit(e)?,
                }
            }
            visit(result)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "module-aware impl emission already wires every method/module/closure dependency through; bundling hides which inputs the called helpers actually read"
)]
fn emit_impl(
    imp: &IrImpl,
    _impl_id: ImplId,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
    method_map: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    closure_ctx: Option<&ClosureCallContext<'_>>,
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
            closure_ctx,
        )?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "module-aware function emission needs every map and closure context the body lowering reads"
)]
fn emit_function(
    f: &IrFunction,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
    method_map: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    impl_self_struct_id: Option<StructId>,
    closure_ctx: Option<&ClosureCallContext<'_>>,
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
        f.return_type.as_ref(),
        &param_bindings,
        function_map,
        method_map,
        module,
        bump_allocator,
        self_struct_id,
        closure_ctx,
    )?;
    let wasm_idx = builder.declare_function_with_body(&param_valtypes, &result_valtypes, &body);
    // Phase 1a: every non-extern top-level function is exported by
    // its source-level name. Survey-driven export filtering lands
    // alongside WIT generation in the next mc. Impl methods do NOT
    // get exported — name collisions between different impls would
    // otherwise produce a malformed module.
    if impl_self_struct_id.is_none() {
        builder.export_function(&kebab_case(&f.name), wasm_idx);
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
