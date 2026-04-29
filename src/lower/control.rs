//! Lowering of control-flow expressions: [`IrExpr::If`],
//! [`IrExpr::Match`], and [`IrExpr::For`].

use formalang::ast::PrimitiveType;
use formalang::ir::{IrExpr, IrMatchArm, ResolvedType};
use wasm_encoder::{BlockType, InstructionSink, MemArg};

use super::aggregate::{field_mem_arg, load_primitive, primitive_of, store_primitive};
use super::{LowerContext, LowerError, lower_expr};
use crate::layout::{
    ARRAY_HEADER_ALIGN, ENUM_TAG_ALIGN, FieldLayout, plan_array, plan_enum, plan_range,
};
use crate::module::MEMORY_INDEX;
use crate::types::{body_value_type, resolved_value_type};

/// Lower an [`IrExpr::If`] onto `sink`.
///
/// Emits a wasm `if BLOCKTY` framed by the branches and a closing
/// `end`. The block type is derived from the resolved `If.ty`:
/// `Never` and unit map to `BlockType::Empty`, scalar primitives map
/// to `BlockType::Result(ValType)`. Aggregate result types are
/// rejected as `NotYetImplemented` until the runtime aggregate ABI
/// lands.
///
/// An if without an `else` branch requires a unit/`Never` result —
/// otherwise the wasm validator would reject the missing else arm
/// for a non-empty block type.
pub fn lower_if(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::If {
        condition,
        then_branch,
        else_branch,
        ty,
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_if called with non-If expression".to_owned(),
        });
    };

    let block_ty = resolved_value_type(ty)?.map_or(BlockType::Empty, BlockType::Result);

    lower_expr(condition, sink, ctx)?;
    sink.if_(block_ty);
    lower_expr(then_branch, sink, ctx)?;
    if let Some(else_branch) = else_branch {
        sink.else_();
        lower_expr(else_branch, sink, ctx)?;
    }
    sink.end();
    Ok(())
}

/// Lower an [`IrExpr::Match`] onto `sink`.
///
/// Pattern: save the scrutinee pointer in a scratch local, build a
/// nested-block + `br_table` structure that dispatches on the
/// discriminant tag, emit each arm's payload bindings (loads from the
/// variant's field offsets into wasm locals indexed by `BindingId`),
/// then lower the arm body. The default case traps with `unreachable`
/// when no wildcard arm is present; the wildcard's body otherwise
/// runs there. The match's overall result type comes from the IR
/// `Match.ty`.
pub fn lower_match(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::Match {
        scrutinee,
        arms,
        ty,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_match called with non-Match expression".to_owned(),
        });
    };

    let module = ctx.module()?;
    let enum_id = match scrutinee.ty() {
        ResolvedType::Enum(id) => *id,
        ResolvedType::Primitive(_)
        | ResolvedType::Struct(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => {
            return Err(LowerError::FieldAccessOnNonAggregate {
                ty: scrutinee.ty().clone(),
            });
        }
    };
    let e = module
        .enums
        .get(enum_id.0 as usize)
        .ok_or(LowerError::UnknownEnum(enum_id))?;
    let layout = plan_enum(e, module)?;
    let num_variants = layout.variants.len();
    let num_arms = arms.len();

    // Save scrutinee pointer in a scratch local so each arm can re-
    // read fields from it without re-evaluating the scrutinee
    // expression.
    let scrutinee_local = ctx.next_scratch_local()?;
    lower_expr(scrutinee, sink, ctx)?;
    sink.local_set(scrutinee_local);

    let outer_block_ty = body_value_type(ty)?.map_or(BlockType::Empty, BlockType::Result);

    // Open the outer $end block.
    sink.block(outer_block_ty);
    // Open the $default block (always — even with a wildcard, the
    // default arm holds the wildcard body or an `unreachable`).
    sink.block(BlockType::Empty);
    // Open one block per arm (outermost first; innermost is for arm
    // index 0). After all arms are open, the innermost holds the
    // `br_table` instruction.
    for _ in 0..num_arms {
        sink.block(BlockType::Empty);
    }

    // Emit the dispatch: load the tag, br_table to the right depth.
    let arm_count_u32 = u32::try_from(num_arms).map_err(|_| LowerError::NotYetImplemented {
        what: "more than u32::MAX match arms in a single function".to_owned(),
    })?;
    let mut targets = vec![arm_count_u32; num_variants];
    let mut wildcard_idx: Option<usize> = None;
    for (p, arm) in arms.iter().enumerate() {
        if arm.is_wildcard {
            wildcard_idx = Some(p);
            continue;
        }
        let p_u32 = u32::try_from(p).map_err(|_| LowerError::NotYetImplemented {
            what: "more than u32::MAX match arms in a single function".to_owned(),
        })?;
        let tag = arm.variant_idx.0 as usize;
        if let Some(slot) = targets.get_mut(tag) {
            *slot = p_u32;
        }
    }
    sink.local_get(scrutinee_local);
    sink.i32_load(MemArg {
        offset: u64::from(layout.tag_offset),
        align: align_log2(ENUM_TAG_ALIGN),
        memory_index: MEMORY_INDEX,
    });
    sink.br_table(targets.iter().copied(), arm_count_u32);
    sink.end(); // closes innermost arm block

    // Emit each arm's body. After arm p closes, the depth from inside
    // the body to $end is `num_arms - p`.
    for (p, arm) in arms.iter().enumerate() {
        if !arm.is_wildcard {
            emit_arm_bindings(arm, scrutinee_local, &layout, sink, ctx)?;
        }
        // Wildcard arms in the regular arm slots are still reachable
        // via the br_table only if they have a real `variant_idx`
        // matching some variant; we keep them as fall-throughs but
        // their body still needs to run. Treat them like any other
        // arm here.
        lower_expr(&arm.body, sink, ctx)?;
        let depth = arm_count_u32
            .checked_sub(u32::try_from(p).unwrap_or(u32::MAX))
            .ok_or_else(|| LowerError::NotYetImplemented {
                what: "match arm depth underflow".to_owned(),
            })?;
        sink.br(depth);
        sink.end(); // closes this arm's outer block (or $default after the last one)
    }

    // The loop emitted one `end` per arm body, closing $arm_0,
    // $arm_1, …, $arm_{N-1}, $default in turn. After the loop we
    // are inside $end. The br_table's default target jumped past
    // $default's `end`, so any wildcard / fall-through body lives
    // here, and its value (or `unreachable`) is what $end produces.
    if let Some(p) = wildcard_idx {
        let arm = arms.get(p).ok_or_else(|| LowerError::NotYetImplemented {
            what: "wildcard arm index out of range (compiler bug)".to_owned(),
        })?;
        lower_expr(&arm.body, sink, ctx)?;
    } else {
        sink.unreachable();
    }
    sink.end(); // closes $end
    Ok(())
}

/// Emit field-load + local.set for each binding declared by `arm`.
/// The scrutinee's base pointer lives in `scrutinee_local`; each
/// binding's wasm-local index comes from `ctx.bindings`, which the
/// function-body planner already populated from the arm's
/// `bindings` vector.
fn emit_arm_bindings(
    arm: &IrMatchArm,
    scrutinee_local: u32,
    layout: &crate::layout::EnumLayout,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let variant_layout = layout
        .variants
        .get(arm.variant_idx.0 as usize)
        .ok_or_else(|| LowerError::UnknownVariant {
            enum_name: "<scrutinee enum>".to_owned(),
            variant: arm.variant.clone(),
        })?;

    for (i, (name, binding_id, ty)) in arm.bindings.iter().enumerate() {
        let primitive = primitive_of(ty)?;
        let field_layout =
            variant_layout
                .fields
                .get(i)
                .ok_or_else(|| LowerError::FieldIndexOutOfRange {
                    struct_name: arm.variant.clone(),
                    field_count: variant_layout.fields.len(),
                    field_idx: u32::try_from(i).unwrap_or(u32::MAX),
                })?;
        let local_idx = ctx
            .bindings
            .get(*binding_id)
            .ok_or(LowerError::UnknownBinding(*binding_id))?;
        let _ = name; // preserved on the IR for diagnostics only
        sink.local_get(scrutinee_local);
        load_primitive(primitive, *field_layout, sink);
        sink.local_set(local_idx);
    }
    Ok(())
}

/// Local copy of `align_to_log2`. The version in `aggregate.rs` is
/// `pub(super)` and reachable, but a `const fn` keeps the call site
/// in the `i32_load` / `i32_store` block tidy without crossing the
/// module boundary.
const fn align_log2(align: u32) -> u32 {
    match align {
        2 => 1,
        4 => 2,
        8 => 3,
        _ => 0,
    }
}

/// Lower an [`IrExpr::For`] onto `sink`.
///
/// Phase 1c restricts the iteration source to `Range<I32>`. Iterating
/// `Array<T>` and other primitive ranges rides later mcs.
///
/// Shape — `for var in start..end { body }` evaluates to
/// `Array<body_ty>`, one entry per iteration. The lowering:
///
/// 1. Lowers `collection` → pointer to the range struct, parks it,
///    then loads `start` and `end` into scratch locals.
/// 2. Computes `len = end - start` and pre-allocates the output
///    array's element buffer (`len * elem_size` bytes) plus its
///    12-byte `{ ptr, len, cap }` header.
/// 3. Loops `i = 0..len` writing `var = start + i` into the
///    var binding's wasm-local on each entry. The body's value is
///    stored at `out_buf + i * elem_size` using the right primitive
///    width (or `i32_store` for aggregate body types stored as
///    pointers).
/// 4. After the loop, fills the output header (`ptr`, `len`, `cap`)
///    and leaves the header pointer on the stack as the For
///    expression's value.
///
/// Scratch locals reserved by the function-body pre-walk in
/// `block::walk_count`: 7 (`range`, `start_save`, `end`, `len`,
/// `out_buf`, `out_header`, `i`) — `walk_count` for `IrExpr::For`
/// performs the matching count.
#[expect(
    clippy::too_many_lines,
    reason = "single-pass For lowering — splitting hides the wasm-stack discipline that ties the steps together"
)]
pub fn lower_for(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::For {
        var,
        var_ty,
        var_binding_id,
        collection,
        body,
        ty,
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_for called with non-For expression".to_owned(),
        });
    };

    // Phase 1c mc4 only supports Range<I32> collections. Array
    // iteration lands once index access is wired up.
    let coll_ty = collection.ty();
    let ResolvedType::Range(bound_box) = coll_ty else {
        return Err(LowerError::NotYetImplemented {
            what: format!(
                "for-loop over collection type {coll_ty:?} (only Range<I32> supported in mc4)"
            ),
        });
    };
    let bound_ty = bound_box.as_ref();
    if !matches!(bound_ty, ResolvedType::Primitive(PrimitiveType::I32)) {
        return Err(LowerError::NotYetImplemented {
            what: format!("for-loop over Range<{bound_ty:?}> (only Range<I32> supported in mc4)"),
        });
    }
    if !matches!(var_ty, ResolvedType::Primitive(PrimitiveType::I32)) {
        return Err(LowerError::NotYetImplemented {
            what: format!("for-loop variable of type {var_ty:?} (only I32 supported in mc4)"),
        });
    }
    let _ = var; // preserved on the IR for diagnostics only

    let module = ctx.module()?;
    let range_layout = plan_range(bound_ty, module)?;

    // Output array element type — derived from the For's overall ty,
    // which is `Array(body_ty)` per the IR contract.
    let ResolvedType::Array(body_box) = ty else {
        return Err(LowerError::NotYetImplemented {
            what: format!("for-loop carrying non-Array result type {ty:?}"),
        });
    };
    let body_ty = body_box.as_ref();
    let array_layout = plan_array(body_ty, module)?;

    // Reserve the seven i32 scratch locals up-front so emission is
    // straight-line.
    let range_local = ctx.next_scratch_local()?;
    let start_local = ctx.next_scratch_local()?;
    let end_local = ctx.next_scratch_local()?;
    let len_local = ctx.next_scratch_local()?;
    let out_buf_local = ctx.next_scratch_local()?;
    let out_header_local = ctx.next_scratch_local()?;
    let i_local = ctx.next_scratch_local()?;
    let var_local = ctx
        .bindings
        .get(*var_binding_id)
        .ok_or(LowerError::UnknownBinding(*var_binding_id))?;

    // ── 1. Lower the range collection and pull start / end out ──────
    lower_expr(collection, sink, ctx)?;
    sink.local_set(range_local);

    sink.local_get(range_local);
    sink.i32_load(MemArg {
        offset: 0,
        align: align_log2(range_layout.bound_align),
        memory_index: MEMORY_INDEX,
    });
    sink.local_set(start_local);

    sink.local_get(range_local);
    sink.i32_load(MemArg {
        offset: u64::from(range_layout.end_offset),
        align: align_log2(range_layout.bound_align),
        memory_index: MEMORY_INDEX,
    });
    sink.local_set(end_local);

    // ── 2. len = end - start ────────────────────────────────────────
    sink.local_get(end_local);
    sink.local_get(start_local);
    sink.i32_sub();
    sink.local_set(len_local);

    // ── 3. Allocate output buffer and header ───────────────────────
    // out_buf: len * elem_size bytes through the bump allocator.
    let alloc_idx = ctx.bump_allocator()?;
    let elem_size_signed = i32::try_from(array_layout.element_size).map_err(|_| {
        LowerError::Layout(crate::layout::LayoutError::SizeOverflow {
            name: "<for-output element>".to_owned(),
        })
    })?;
    sink.local_get(len_local);
    sink.i32_const(elem_size_signed);
    sink.i32_mul();
    sink.call(alloc_idx);
    sink.local_set(out_buf_local);

    // out_header: 12 bytes through the bump allocator. We reuse
    // `allocate_aggregate` for the header since it also stashes the
    // pointer in a fresh scratch local, but here we already reserved
    // out_header_local up-front — so call into the allocator manually.
    sink.i32_const(i32::try_from(array_layout.header_size).map_err(|_| {
        LowerError::Layout(crate::layout::LayoutError::SizeOverflow {
            name: "<for-output header>".to_owned(),
        })
    })?);
    sink.call(alloc_idx);
    sink.local_set(out_header_local);

    // ── 4. i = 0 ────────────────────────────────────────────────────
    sink.i32_const(0);
    sink.local_set(i_local);

    // ── 5. Main loop ────────────────────────────────────────────────
    sink.block(BlockType::Empty);
    sink.loop_(BlockType::Empty);

    // exit if i >= len
    sink.local_get(i_local);
    sink.local_get(len_local);
    sink.i32_ge_s();
    sink.br_if(1); // exit the surrounding $end block

    // var = start + i
    sink.local_get(start_local);
    sink.local_get(i_local);
    sink.i32_add();
    sink.local_set(var_local);

    // Push the address (out_buf + i * elem_size) for the upcoming
    // body store.
    sink.local_get(out_buf_local);
    sink.local_get(i_local);
    sink.i32_const(elem_size_signed);
    sink.i32_mul();
    sink.i32_add();

    // Lower the body — leaves body_val on top of the stack.
    lower_expr(body, sink, ctx)?;

    // Store at the address computed above.
    let body_field_layout = FieldLayout {
        offset: 0,
        size: array_layout.element_size,
        align: array_layout.element_align,
    };
    store_for_body_value(body_ty, body_field_layout, sink)?;

    // i += 1
    sink.local_get(i_local);
    sink.i32_const(1);
    sink.i32_add();
    sink.local_set(i_local);

    // br to loop top
    sink.br(0);
    sink.end(); // close loop
    sink.end(); // close $end block

    // ── 6. Finalize header: ptr, len, cap ───────────────────────────
    let header_align_log2 = align_log2(ARRAY_HEADER_ALIGN);
    sink.local_get(out_header_local);
    sink.local_get(out_buf_local);
    sink.i32_store(MemArg {
        offset: 0,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });
    sink.local_get(out_header_local);
    sink.local_get(len_local);
    sink.i32_store(MemArg {
        offset: 4,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });
    sink.local_get(out_header_local);
    sink.local_get(len_local);
    sink.i32_store(MemArg {
        offset: 8,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });

    // Leave the header pointer on the stack as the For value.
    sink.local_get(out_header_local);

    Ok(())
}

/// Emit the right `store` opcode for a For-loop body value at the
/// pre-computed `(buf + i * elem_size)` address that's already on the
/// stack just below the body value. Mirrors
/// [`crate::lower::aggregate::store_array_element`] but keeps the
/// dispatch local to the control-flow module.
fn store_for_body_value(
    body_ty: &ResolvedType,
    field_layout: FieldLayout,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    match body_ty {
        ResolvedType::Primitive(p) => {
            store_primitive(*p, field_layout, sink);
            Ok(())
        }
        ResolvedType::Struct(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_) => {
            sink.i32_store(field_mem_arg(field_layout));
            Ok(())
        }
        ResolvedType::Optional(_)
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Trait(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Error => Err(LowerError::NotYetImplemented {
            what: format!("for-loop body of type {body_ty:?}"),
        }),
    }
}
