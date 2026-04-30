//! Lowering of `Optional<T>` value coercions.
//!
//! Phase 2 mc2 already lowers the `nil` literal end-to-end. This
//! module supplies the matching `Some` direction: when an expression
//! of static type `T` flows into a slot typed `Optional<T>`, allocate
//! a full-size `Optional<T>` cell, store the `OPTIONAL_TAG_SOME`
//! discriminant at offset 0 and the payload at `payload_offset`, and
//! leave the cell's pointer on the stack. Aggregate payloads (struct,
//! enum, tuple, array) ride later mcs — for now `wrap_some` only
//! handles primitive payload types via [`super::aggregate::store_primitive`].

use formalang::ir::{IrExpr, ResolvedType};
use wasm_encoder::{InstructionSink, MemArg, ValType};

use super::aggregate::{allocate_aggregate, primitive_of, store_primitive};
use super::block::{ScratchCounts, bump_count};
use super::{LowerContext, LowerError, lower_expr};
use crate::layout::{FieldLayout, OPTIONAL_TAG_ALIGN, OPTIONAL_TAG_SOME, plan_optional};
use crate::module::MEMORY_INDEX;

/// Whether `target_ty` is an `Optional<U>` and `value_ty` is exactly
/// `U` (the Some-wrap shape). Returns `Some(U)` for the wrap case so
/// callers can plan layouts and scratch slots ahead of emission;
/// returns `None` for every other type combination — including the
/// already-Optional `value_ty == target_ty` case and the
/// `Optional<Never>` "nil widens to Optional<T>" shortcut, both of
/// which travel through the existing pointer pass-through path.
#[must_use]
pub(super) fn some_wrap_payload<'a>(
    target_ty: &'a ResolvedType,
    value_ty: &ResolvedType,
) -> Option<&'a ResolvedType> {
    let ResolvedType::Optional(inner) = target_ty else {
        return None;
    };
    if value_ty == inner.as_ref() {
        Some(inner.as_ref())
    } else {
        None
    }
}

/// Lower a Some-wrap of `value_expr`'s value into a fresh
/// `Optional<payload_ty>` cell.
///
/// The value is evaluated first and stashed in a typed scratch local
/// so the bump-allocator call doesn't disturb the operand stack. The
/// emitted sequence is:
///
/// ```text
/// <value_expr>          ; stack: [value]
/// local.set value_scratch
/// i32.const size
/// call __alloc          ; stack: [ptr]
/// local.set ptr_scratch
/// local.get ptr_scratch
/// i32.const OPTIONAL_TAG_SOME
/// i32.store offset=0
/// local.get ptr_scratch
/// local.get value_scratch
/// <store_primitive>     ; payload at payload_offset
/// local.get ptr_scratch ; stack: [ptr]
/// ```
pub(super) fn lower_some_wrap(
    value_expr: &IrExpr,
    payload_ty: &ResolvedType,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let prim = primitive_of(payload_ty).map_err(|_| LowerError::NotYetImplemented {
        what: format!("Some-wrap of non-primitive payload type {payload_ty:?}"),
    })?;
    let module = ctx.module()?;
    let layout = plan_optional(payload_ty, module)?;

    let value_vt = primitive_to_valtype(payload_ty)?;
    let value_scratch = ctx.next_scratch_local(value_vt)?;

    lower_expr(value_expr, sink, ctx)?;
    sink.local_set(value_scratch);

    let base_local = allocate_aggregate(layout.size, sink, ctx)?;

    sink.local_get(base_local);
    sink.i32_const(i32::try_from(OPTIONAL_TAG_SOME).unwrap_or(i32::MAX));
    sink.i32_store(MemArg {
        offset: u64::from(layout.tag_offset),
        align: OPTIONAL_TAG_ALIGN.trailing_zeros(),
        memory_index: MEMORY_INDEX,
    });

    sink.local_get(base_local);
    sink.local_get(value_scratch);
    let payload_field = FieldLayout {
        offset: layout.payload_offset,
        size: layout.payload_size,
        align: layout.payload_align,
    };
    store_primitive(prim, payload_field, sink);

    sink.local_get(base_local);
    Ok(())
}

/// Map a primitive payload type to the wasm value type its scratch
/// slot needs. Aggregates won't reach this helper — `lower_some_wrap`
/// rejects non-primitive payloads with `NotYetImplemented` ahead of
/// any scratch-local allocation.
fn primitive_to_valtype(ty: &ResolvedType) -> Result<ValType, LowerError> {
    let prim = primitive_of(ty).map_err(|_| LowerError::NotYetImplemented {
        what: format!("Some-wrap scratch slot for non-primitive payload {ty:?}"),
    })?;
    match prim {
        formalang::ast::PrimitiveType::I32 | formalang::ast::PrimitiveType::Boolean => {
            Ok(ValType::I32)
        }
        formalang::ast::PrimitiveType::I64 => Ok(ValType::I64),
        formalang::ast::PrimitiveType::F32 => Ok(ValType::F32),
        formalang::ast::PrimitiveType::F64 => Ok(ValType::F64),
        // Never / String / Path / Regex have no inline-storable size
        // yet; layout::plan_optional rejects these earlier so we should
        // never reach this arm in practice.
        formalang::ast::PrimitiveType::Never
        | formalang::ast::PrimitiveType::String
        | formalang::ast::PrimitiveType::Path
        | formalang::ast::PrimitiveType::Regex
        | _ => Err(LowerError::NotYetImplemented {
            what: format!("Some-wrap scratch slot for {prim:?} payload"),
        }),
    }
}

/// Wasm value-type used for the typed scratch slot a Some-wrap
/// reserves to stash its payload across the bump-allocator call. The
/// pre-walk in `block::walk_count` consults this so the per-type
/// scratch counts match the lowering walker's consumption.
pub(super) fn some_wrap_scratch_valtype(payload_ty: &ResolvedType) -> Result<ValType, LowerError> {
    primitive_to_valtype(payload_ty)
}

/// Lower `value_expr` into a slot of static type `target_ty`,
/// inserting an `Optional<T>` Some-wrap when the slot widens the
/// value's type. Sites that own a known target type (function
/// returns, if branches, match arm bodies) call this in place of the
/// bare [`lower_expr`] so a primitive `T` flowing into an
/// `Optional<T>` slot materializes as a tagged-Some cell rather than
/// a bare `T`. Other type combinations (exact match,
/// Optional<Never> -> Optional<T>) flow through unchanged via the
/// regular lowering path.
pub(super) fn lower_coerced(
    value_expr: &IrExpr,
    target_ty: &ResolvedType,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    if let Some(payload_ty) = some_wrap_payload(target_ty, value_expr.ty()) {
        lower_some_wrap(value_expr, payload_ty, sink, ctx)
    } else {
        lower_expr(value_expr, sink, ctx)
    }
}

/// Add the scratch-slot reservations a Some-wrap coercion at this
/// site requires, on top of whatever the inner expression already
/// counts. Callers that own a known target type pair this with the
/// regular `walk_count` recursion so the pre-walk's totals match the
/// lowering walker's consumption.
pub(super) fn coercion_scratch_counts(
    target_ty: &ResolvedType,
    value_ty: &ResolvedType,
    out: &mut ScratchCounts,
) -> Result<(), LowerError> {
    let Some(payload_ty) = some_wrap_payload(target_ty, value_ty) else {
        return Ok(());
    };
    bump_count(&mut out.i32)?;
    let vt = some_wrap_scratch_valtype(payload_ty)?;
    match vt {
        ValType::I32 => bump_count(&mut out.i32)?,
        ValType::I64 => bump_count(&mut out.i64)?,
        ValType::F32 => bump_count(&mut out.f32)?,
        ValType::F64 => bump_count(&mut out.f64)?,
        ValType::V128 | ValType::Ref(_) => {
            return Err(LowerError::NotYetImplemented {
                what: format!("Some-wrap scratch slot of value type {vt:?}"),
            });
        }
    }
    Ok(())
}
