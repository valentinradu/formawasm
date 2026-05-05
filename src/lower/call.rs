//! Lowering of [`IrExpr::FunctionCall`] (direct calls) and
//! [`IrExpr::MethodCall`] static + virtual dispatch.
//!
//! Static dispatch resolves to a direct `call <wasm_index>`. Virtual
//! dispatch loads a funcref-table slot index from the receiver's
//! per-trait vtable in linear memory, then `call_indirect`s against
//! the module's method funcref table. Indirect closure calls live
//! alongside in [`super::call::lower_call_closure`].

use formalang::ir::{DispatchKind, ImplTarget, IrExpr, ResolvedType};
use wasm_encoder::{InstructionSink, MemArg};

use super::{LowerContext, LowerError, lower_expr};
use crate::layout::VTABLE_SLOT_SIZE;
use crate::module::MEMORY_INDEX;
use crate::module_lowering::impl_target_key;
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

    // Frontend passes (DeadCodeElimination, MonomorphisePass) can
    // renumber `module.functions` and leave the IR's `function_id`
    // pointing at a stale slot — *and* overloads compound the
    // problem because every overload shares the same source name.
    // When a module is in context, resolve by
    // *(name, argument-label set)*: a candidate is the function
    // whose name matches the path's last segment AND whose parameter
    // names cover every labelled argument the call passes. Validate
    // `function_id` against that filter first; fall back to a search
    // when it disagrees. Without module context (legacy hand-built
    // tests), trust `function_id` as authoritative.
    let module = ctx.module().ok();
    let id = if let Some(m) = module {
        let path_last = path.last().map(String::as_str);
        let arg_labels: Vec<&str> = args
            .iter()
            .filter_map(|(label, _)| label.as_deref())
            .collect();
        let matches_signature = |f: &formalang::ir::IrFunction| -> bool {
            Some(f.name.as_str()) == path_last
                && f.params.len() == args.len()
                && arg_labels
                    .iter()
                    .all(|label| f.params.iter().any(|p| p.name == *label))
        };
        let id_valid = function_id.and_then(|id| {
            let f = m.functions.get(id.0 as usize)?;
            if matches_signature(f) { Some(id) } else { None }
        });
        let resolved_id = id_valid.or_else(|| {
            m.functions.iter().enumerate().find_map(|(i, f)| {
                if matches_signature(f) {
                    u32::try_from(i).ok().map(formalang::ir::FunctionId)
                } else {
                    None
                }
            })
        });
        resolved_id
            .or(*function_id)
            .ok_or_else(|| LowerError::UnresolvedFunctionCall { path: path.clone() })?
    } else {
        function_id.ok_or_else(|| LowerError::UnresolvedFunctionCall { path: path.clone() })?
    };
    let wasm_idx = ctx
        .functions
        .get(id)
        .ok_or(LowerError::UnknownFunction(id))?;

    // Look up the callee in the IR module so each argument can coerce
    // to its parameter's declared type (Some-wrap widens a plain T
    // into an Optional<T> param at the call site).
    let callee = module.and_then(|m| m.functions.get(id.0 as usize));
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

/// Lower an [`IrExpr::MethodCall`] onto `sink`.
///
/// Static dispatch pushes the receiver pointer (implicit first
/// parameter), then explicit args in declaration order, then emits a
/// single `call <wasm_index>` resolved through the [`super::MethodMap`]
/// in `ctx`.
///
/// Virtual dispatch resolves the receiver's concrete type at compile
/// time (`Struct` / `Enum`), looks up the matching `(trait_id, target)`
/// vtable's absolute byte offset, loads the funcref-table slot at
/// `vtable_base + method_idx * VTABLE_SLOT_SIZE`, pushes the receiver
/// alongside its explicit args (with Optional coercion against the
/// trait method's declared parameter types), then emits `call_indirect`
/// against the module's method funcref table. The trait method's wasm
/// signature was pre-registered in the type section by the module-
/// level pass.
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

    match dispatch {
        DispatchKind::Static { impl_id } => {
            lower_static_method_call(*impl_id, *method_idx, receiver, args, sink, ctx)
        }
        DispatchKind::Virtual {
            trait_id,
            method_name: _,
        } => lower_virtual_method_call(*trait_id, *method_idx, receiver, args, sink, ctx),
    }
}

fn lower_static_method_call(
    impl_id: formalang::ir::ImplId,
    method_idx: formalang::ir::MethodIdx,
    receiver: &IrExpr,
    args: &[(Option<String>, IrExpr)],
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let methods = ctx
        .methods
        .ok_or(LowerError::MissingContext { what: "methods" })?;
    let wasm_idx = methods
        .get((impl_id, method_idx))
        .ok_or(LowerError::UnknownMethod {
            impl_id,
            method_idx,
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

/// Dispatch a method call where the receiver carries a trait
/// type — its concrete impl was erased at the assignment site.
/// The receiver evaluates to a pointer at an 8-byte fat-pointer
/// cell `(vtable_offset, data_ptr)` materialised by
/// `super::optional::materialize_trait_fat_pointer`. We load
/// both fields, push `data_ptr` as the call's first arg, lower
/// the explicit args, then load the funcref at
/// `vtable_offset + method_idx * VTABLE_SLOT_SIZE` and
/// `call_indirect`.
fn lower_trait_typed_dispatch(
    method_idx: formalang::ir::MethodIdx,
    receiver: &IrExpr,
    args: &[(Option<String>, IrExpr)],
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
    trait_id: formalang::ir::TraitId,
) -> Result<(), LowerError> {
    use crate::layout::{POINTER_SIZE, VTABLE_SLOT_SIZE};
    use crate::module::MEMORY_INDEX;
    use wasm_encoder::{MemArg, ValType};

    let table_idx = ctx.method_table_index()?;
    let type_idx = ctx.virtual_call_type_index(trait_id, method_idx)?;

    // Park the cell pointer in a scratch local so we can read
    // both fields and re-push the data pointer for the call.
    let cell_scratch = ctx.next_scratch_local(ValType::I32)?;
    lower_expr(receiver, sink, ctx)?;
    sink.local_set(cell_scratch);

    let mem_arg = |off: u64| MemArg {
        offset: off,
        align: 2,
        memory_index: MEMORY_INDEX,
    };

    // Push data_ptr (the actual struct/enum receiver).
    sink.local_get(cell_scratch);
    sink.i32_load(mem_arg(u64::from(POINTER_SIZE)));

    // Lower explicit args, coercing against the trait method's
    // declared param types (mirrors the static path).
    let trait_method_sig = ctx
        .module()
        .ok()
        .and_then(|m| m.traits.get(trait_id.0 as usize))
        .and_then(|t| t.methods.get(method_idx.0 as usize));
    for (param_name, arg) in args {
        let target_ty = trait_method_sig.and_then(|sig| {
            param_name.as_ref().and_then(|n| {
                sig.params
                    .iter()
                    .find(|p| p.name == *n)
                    .and_then(|p| p.ty.as_ref())
            })
        });
        if let Some(t) = target_ty {
            super::optional::lower_coerced(arg, t, sink, ctx)?;
        } else {
            lower_expr(arg, sink, ctx)?;
        }
    }

    // Load funcref at vtable_offset + method_idx * VTABLE_SLOT_SIZE.
    sink.local_get(cell_scratch);
    sink.i32_load(mem_arg(0)); // vtable_offset
    let method_byte_off = u64::from(method_idx.0)
        .checked_mul(u64::from(VTABLE_SLOT_SIZE))
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: "vtable slot byte offset overflow".to_owned(),
        })?;
    let method_byte_off_signed = i32::try_from(method_byte_off).unwrap_or(i32::MAX);
    sink.i32_const(method_byte_off_signed);
    sink.i32_add();
    sink.i32_load(MemArg {
        offset: 0,
        align: 2,
        memory_index: MEMORY_INDEX,
    });
    sink.call_indirect(table_idx, type_idx);
    Ok(())
}

fn lower_virtual_method_call(
    trait_id: formalang::ir::TraitId,
    method_idx: formalang::ir::MethodIdx,
    receiver: &IrExpr,
    args: &[(Option<String>, IrExpr)],
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    // A trait-typed receiver carries an 8-byte fat-pointer cell
    // `(vtable_offset, data_ptr)`. Detect it and dispatch via a
    // dynamic vtable load: load the cell pointer's first i32 to
    // get vtable_offset, the second to get data_ptr, then walk
    // method_idx slots into the vtable. This is the only path
    // where vtable_base isn't statically known at compile time.
    if let ResolvedType::Trait(_) = receiver.ty() {
        return lower_trait_typed_dispatch(method_idx, receiver, args, sink, ctx, trait_id);
    }
    let target = if let Some(sid) = crate::compound::struct_id_of(receiver.ty()) {
        // Covers both `ResolvedType::Struct(sid)` and the
        // `Generic { base: Struct(sid), .. }` form, so a monomorphic
        // instantiation dispatches through the same vtable as its
        // declaration.
        ImplTarget::Struct(sid)
    } else if let Some(eid) = crate::compound::enum_id_of(receiver.ty()) {
        ImplTarget::Enum(eid)
    } else {
        return Err(LowerError::UnsupportedVirtualReceiver {
            ty: receiver.ty().clone(),
        });
    };
    let table_idx = ctx.method_table_index()?;
    let type_idx = ctx.virtual_call_type_index(trait_id, method_idx)?;
    let vtable_base = ctx.vtable_offset(trait_id, impl_target_key(target))?;

    // Slot byte offset = vtable_base + method_idx * VTABLE_SLOT_SIZE.
    // Computed at compile time so the runtime cost is a single
    // i32.const + i32.load before the call_indirect.
    let slot_offset = u64::from(vtable_base)
        .checked_add(
            u64::from(method_idx.0)
                .checked_mul(u64::from(VTABLE_SLOT_SIZE))
                .ok_or_else(|| LowerError::NotYetImplemented {
                    what: "vtable slot offset overflow".to_owned(),
                })?,
        )
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: "vtable slot offset overflow".to_owned(),
        })?;

    // Push the receiver pointer (implicit first arg of every trait
    // method) and the explicit args, coercing each against the
    // trait method's declared parameter type so Optional widening
    // matches static dispatch.
    lower_expr(receiver, sink, ctx)?;
    let trait_method_sig = ctx
        .module()
        .ok()
        .and_then(|m| m.traits.get(trait_id.0 as usize))
        .and_then(|t| t.methods.get(method_idx.0 as usize));
    for (param_name, arg) in args {
        let target_ty = trait_method_sig.and_then(|sig| {
            param_name.as_ref().and_then(|n| {
                sig.params
                    .iter()
                    .find(|p| p.name == *n)
                    .and_then(|p| p.ty.as_ref())
            })
        });
        if let Some(t) = target_ty {
            super::optional::lower_coerced(arg, t, sink, ctx)?;
        } else {
            lower_expr(arg, sink, ctx)?;
        }
    }

    // Load the funcref-table slot from the vtable cell, then
    // call_indirect.
    sink.i32_const(0)
        .i32_load(MemArg {
            offset: slot_offset,
            align: 2, // log2(4)
            memory_index: MEMORY_INDEX,
        })
        .call_indirect(table_idx, type_idx);
    Ok(())
}
