//! Lowering of [`IrExpr::FunctionCall`] (direct calls) and
//! [`IrExpr::MethodCall`] static dispatch.
//!
//! Indirect calls (closures, vtables) live in Phase 1b/3 alongside
//! `ClosureRef` and virtual dispatch.

use formalang::ir::{DispatchKind, IrExpr};
use wasm_encoder::InstructionSink;

use super::{LowerContext, LowerError, lower_expr};
use crate::module::MEMORY_INDEX;
use crate::types::{CLOSURE_ENV_OFFSET, CLOSURE_FUNCREF_OFFSET};

/// Lower an [`IrExpr::FunctionCall`] onto `sink`.
///
/// Each argument is lowered in declaration order, then a
/// `call <wasm_index>` instruction is emitted. The wasm index comes
/// from the [`super::FunctionMap`] in `ctx`.
pub fn lower_function_call(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::FunctionCall {
        path,
        function_id,
        args,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_function_call called with non-FunctionCall expression".to_owned(),
        });
    };

    let id =
        function_id.ok_or_else(|| LowerError::UnresolvedFunctionCall { path: path.clone() })?;
    let wasm_idx = ctx
        .functions
        .get(id)
        .ok_or(LowerError::UnknownFunction(id))?;

    // Look up the callee in the IR module so each argument can coerce
    // to its parameter's declared type (Some-wrap widens a plain T
    // into an Optional<T> param at the call site).
    let callee = ctx
        .module()
        .ok()
        .and_then(|m| m.functions.get(id.0 as usize));
    for (param_name, arg) in args {
        let target = callee.and_then(|f| {
            param_name.as_ref().and_then(|n| {
                f.params
                    .iter()
                    .find(|p| p.name == *n)
                    .and_then(|p| p.ty.as_ref())
            })
        });
        if let Some(t) = target {
            super::optional::lower_coerced(arg, t, sink, ctx)?;
        } else {
            lower_expr(arg, sink, ctx)?;
        }
    }
    sink.call(wasm_idx);
    Ok(())
}

/// Lower an [`IrExpr::CallClosure`] onto `sink`.
///
/// The `closure` sub-expression evaluates to a base pointer for an
/// 8-byte `(funcref_idx: i32, env_ptr: i32)` value built by
/// [`super::lower_closure_ref`]. The lowering:
///
/// 1. Park the closure value's base pointer in a fresh i32 scratch
///    local so we can read from it twice.
/// 2. Push `env_ptr` (loaded from offset 4) as the first argument —
///    the lifted top-level function takes the env struct in slot 0.
/// 3. Lower each explicit argument in declaration order.
/// 4. Push `funcref_idx` (loaded from offset 0) as the table index.
/// 5. Emit `call_indirect` against the funcref table, with a wasm
///    type signature derived from the closure's `ResolvedType::Closure`.
///
/// The funcref table and per-closure type signatures are wired by
/// [`crate::module_lowering`] before any function bodies are
/// lowered; this helper looks up the matching type index through
/// [`LowerContext::closure_type_index`].
pub fn lower_call_closure(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    use formalang::ir::ResolvedType;
    use wasm_encoder::{MemArg, ValType};

    let IrExpr::CallClosure { closure, args, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_call_closure called with non-CallClosure expression".to_owned(),
        });
    };

    let closure_ty = closure.ty().clone();
    let ResolvedType::Closure { .. } = &closure_ty else {
        return Err(LowerError::NotYetImplemented {
            what: format!("CallClosure on non-closure-typed value {closure_ty:?}"),
        });
    };

    let table_idx = ctx.closure_table_index()?;
    let type_idx = ctx.closure_type_index(&closure_ty)?;

    let base_local = ctx.next_scratch_local(ValType::I32)?;
    lower_expr(closure, sink, ctx)?;
    sink.local_set(base_local);

    // env_ptr arg first (the lifted function's first parameter).
    sink.local_get(base_local);
    sink.i32_load(MemArg {
        offset: u64::from(CLOSURE_ENV_OFFSET),
        align: 2, // log2(4)
        memory_index: MEMORY_INDEX,
    });

    for (_, arg) in args {
        lower_expr(arg, sink, ctx)?;
    }

    // Funcref index for call_indirect.
    sink.local_get(base_local);
    sink.i32_load(MemArg {
        offset: u64::from(CLOSURE_FUNCREF_OFFSET),
        align: 2,
        memory_index: MEMORY_INDEX,
    });
    sink.call_indirect(table_idx, type_idx);
    Ok(())
}

/// Lower an [`IrExpr::MethodCall`] with static dispatch onto `sink`.
///
/// Pushes the receiver pointer (the implicit first parameter), then
/// the explicit args in declaration order, then emits a single
/// `call <wasm_index>` resolved through the [`super::MethodMap`] in
/// `ctx`. Virtual dispatch lands in Phase 3.
pub fn lower_method_call(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::MethodCall {
        receiver,
        method_idx,
        args,
        dispatch,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_method_call called with non-MethodCall expression".to_owned(),
        });
    };

    let impl_id = match dispatch {
        DispatchKind::Static { impl_id } => *impl_id,
        DispatchKind::Virtual { .. } => return Err(LowerError::VirtualMethodCall),
    };
    let methods = ctx
        .methods
        .ok_or(LowerError::MissingContext { what: "methods" })?;
    let wasm_idx = methods
        .get((impl_id, *method_idx))
        .ok_or(LowerError::UnknownMethod {
            impl_id,
            method_idx: *method_idx,
        })?;

    lower_expr(receiver, sink, ctx)?;
    // Look up the method's signature in the impl block so each argument
    // can coerce to its parameter's declared type.
    let method_sig = ctx
        .module()
        .ok()
        .and_then(|m| m.impls.get(impl_id.0 as usize))
        .and_then(|i| i.functions.get(method_idx.0 as usize));
    for (param_name, arg) in args {
        let target = method_sig.and_then(|sig| {
            param_name.as_ref().and_then(|n| {
                sig.params
                    .iter()
                    .find(|p| p.name == *n)
                    .and_then(|p| p.ty.as_ref())
            })
        });
        if let Some(t) = target {
            super::optional::lower_coerced(arg, t, sink, ctx)?;
        } else {
            lower_expr(arg, sink, ctx)?;
        }
    }
    sink.call(wasm_idx);
    Ok(())
}
