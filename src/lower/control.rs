//! Lowering of control-flow expressions: [`IrExpr::If`],
//! [`IrExpr::Match`], and [`IrExpr::For`].

use formalang::ast::PrimitiveType;
use formalang::ir::{IrExpr, IrMatchArm, ResolvedType};
use wasm_encoder::{BlockType, InstructionSink, MemArg};

use super::aggregate::{
    HeaderLen, field_mem_arg, finalize_array_header, load_primitive, primitive_of, store_primitive,
};
use super::{LowerContext, LowerError, lower_expr};
use crate::layout::{
    ARRAY_HEADER_ALIGN, ENUM_TAG_ALIGN, FieldLayout, plan_array, plan_enum, plan_range,
};
use crate::module::MEMORY_INDEX;
use crate::types::{body_value_type, resolved_value_type};

/// Number of i32 scratch locals each `IrExpr::For` allocates. Both
/// the `Range<I32>` and `Array<T>` source paths reserve the same count
/// so [`super::block::walk_count`] stays source-agnostic — the array
/// path uses six and intentionally skips the seventh slot to keep
/// reservation and consumption uniform across both shapes.
///
/// Range layout (all seven consumed): `range`, `start`, `end`, `len`,
/// `out_buf`, `out_header`, `i`.
///
/// Array layout (six consumed, one skipped): `arr`, `in_buf`,
/// `<skipped>`, `len`, `out_buf`, `out_header`, `i`.
pub(super) const FOR_SCRATCH_LOCAL_COUNT: u32 = 7;

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

/// Iteration source classified after [`check_for_types`] runs. Each
/// shape carries the resolved element type lifted out of the wrapping
/// `Range` / `Array` constructor — the lowerer needs it for layout
/// planning and per-element load/store opcodes.
enum ForSource<'a> {
    /// `for var in start..end` — `bound_ty` is the range's `T` and
    /// must be `I32` in Phase 1c.
    Range(&'a ResolvedType),
    /// `for var in arr` — `elem_ty` is the array's element type and
    /// drives both the per-iteration load opcode for `var` and the
    /// element stride.
    Array(&'a ResolvedType),
}

/// Scratch + binding locals reserved up-front for a Range-sourced
/// For loop. All seven scratch slots are i32; `var` is allocated
/// through the binding map at the loop variable's value type.
struct RangeForLocals {
    range: u32,
    start: u32,
    end: u32,
    len: u32,
    out_buf: u32,
    out_header: u32,
    i: u32,
    var: u32,
}

/// Scratch + binding locals reserved for an Array-sourced For loop.
/// Six scratch slots consumed; the seventh is intentionally skipped
/// (see [`FOR_SCRATCH_LOCAL_COUNT`]) so the reservation count stays
/// uniform across both source shapes.
struct ArrayForLocals {
    arr: u32,
    in_buf: u32,
    len: u32,
    out_buf: u32,
    out_header: u32,
    i: u32,
    var: u32,
}

/// Lower an [`IrExpr::For`] onto `sink`.
///
/// Phase 1c accepts two iteration sources:
///
/// * `for var in start..end` — `Range<I32>` only. `var = start + i`
///   each iteration.
/// * `for var in arr` — `Array<T>` for any element type the layout
///   planner accepts. `var` is loaded from `arr`'s element buffer at
///   `i * elem_size` each iteration.
///
/// In both cases the For expression evaluates to `Array<body_ty>`,
/// one entry per iteration. The lowering splits cleanly into setup
/// (resolve the source, allocate output), the main `block`/`loop`
/// body, and a final header write — see the per-source helpers.
pub fn lower_for(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::For {
        var_ty,
        var_binding_id,
        collection,
        body,
        ty,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_for called with non-For expression".to_owned(),
        });
    };

    let (source, body_ty) = check_for_types(collection, var_ty, ty)?;
    let var_local = ctx
        .bindings
        .get(*var_binding_id)
        .ok_or(LowerError::UnknownBinding(*var_binding_id))?;

    match source {
        ForSource::Range(bound_ty) => {
            lower_for_range(collection, body, bound_ty, body_ty, var_local, ctx, sink)
        }
        ForSource::Array(elem_ty) => {
            lower_for_array(collection, body, elem_ty, body_ty, var_local, ctx, sink)
        }
    }
}

/// Validate the For-expression's collection / variable / result types
/// and return the iteration source plus the output element type so
/// [`lower_for`] can dispatch and plan layouts before emitting any
/// code. The result type must be `Array<body_ty>` per the IR
/// contract.
fn check_for_types<'a>(
    collection: &'a IrExpr,
    var_ty: &ResolvedType,
    ty: &'a ResolvedType,
) -> Result<(ForSource<'a>, &'a ResolvedType), LowerError> {
    let ResolvedType::Array(body_box) = ty else {
        return Err(LowerError::NotYetImplemented {
            what: format!("for-loop carrying non-Array result type {ty:?}"),
        });
    };
    let body_ty = body_box.as_ref();
    let coll_ty = collection.ty();
    match coll_ty {
        ResolvedType::Range(bound_box) => {
            let bound_ty = bound_box.as_ref();
            if !matches!(bound_ty, ResolvedType::Primitive(PrimitiveType::I32)) {
                return Err(LowerError::NotYetImplemented {
                    what: format!(
                        "for-loop over Range<{bound_ty:?}> (only Range<I32> supported in Phase 1c)"
                    ),
                });
            }
            if !matches!(var_ty, ResolvedType::Primitive(PrimitiveType::I32)) {
                return Err(LowerError::NotYetImplemented {
                    what: format!(
                        "for-loop variable of type {var_ty:?} for Range<I32> (only I32 supported)"
                    ),
                });
            }
            Ok((ForSource::Range(bound_ty), body_ty))
        }
        ResolvedType::Array(elem_box) => Ok((ForSource::Array(elem_box.as_ref()), body_ty)),
        ResolvedType::Primitive(_)
        | ResolvedType::Struct(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Trait(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Error => Err(LowerError::NotYetImplemented {
            what: format!("for-loop over collection type {coll_ty:?}"),
        }),
    }
}

/// Range-sourced For-loop: allocate seven scratch locals, lower
/// `collection` to a range pointer, compute `len = end - start`, then
/// loop with `var = start + i`.
fn lower_for_range(
    collection: &IrExpr,
    body: &IrExpr,
    bound_ty: &ResolvedType,
    body_ty: &ResolvedType,
    var_local: u32,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let module = ctx.module()?;
    let range_layout = plan_range(bound_ty, module)?;
    let array_layout = plan_array(body_ty, module)?;

    let locals = RangeForLocals {
        range: ctx.next_scratch_local()?,
        start: ctx.next_scratch_local()?,
        end: ctx.next_scratch_local()?,
        len: ctx.next_scratch_local()?,
        out_buf: ctx.next_scratch_local()?,
        out_header: ctx.next_scratch_local()?,
        i: ctx.next_scratch_local()?,
        var: var_local,
    };

    emit_for_range_setup(collection, range_layout, array_layout, &locals, ctx, sink)?;
    emit_for_range_loop(body, body_ty, array_layout, &locals, ctx, sink)?;
    finalize_array_header(
        locals.out_header,
        locals.out_buf,
        HeaderLen::Local(locals.len),
        sink,
    );
    Ok(())
}

/// Array-sourced For-loop: lower `collection` to an array header
/// pointer, pull `in_buf` and `len` out of the header, allocate the
/// output, then loop loading `var` from `in_buf + i * elem_size` each
/// iteration.
fn lower_for_array(
    collection: &IrExpr,
    body: &IrExpr,
    elem_ty: &ResolvedType,
    body_ty: &ResolvedType,
    var_local: u32,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let module = ctx.module()?;
    let in_layout = plan_array(elem_ty, module)?;
    let out_layout = plan_array(body_ty, module)?;

    let arr = ctx.next_scratch_local()?;
    let in_buf = ctx.next_scratch_local()?;
    // Skip one scratch slot to keep the per-For reservation count
    // uniform with the Range path; see FOR_SCRATCH_LOCAL_COUNT.
    let _skipped = ctx.next_scratch_local()?;
    let locals = ArrayForLocals {
        arr,
        in_buf,
        len: ctx.next_scratch_local()?,
        out_buf: ctx.next_scratch_local()?,
        out_header: ctx.next_scratch_local()?,
        i: ctx.next_scratch_local()?,
        var: var_local,
    };

    emit_for_array_setup(collection, out_layout, &locals, ctx, sink)?;
    emit_for_array_loop(
        body, elem_ty, body_ty, in_layout, out_layout, &locals, ctx, sink,
    )?;
    finalize_array_header(
        locals.out_header,
        locals.out_buf,
        HeaderLen::Local(locals.len),
        sink,
    );
    Ok(())
}

/// Emit setup for the Range path: lower `collection` to a range
/// pointer, pull `start` and `end` into scratch locals, compute
/// `len = end - start`, and bump-allocate the output element buffer
/// and header. After this helper returns, `i` is pre-set to 0 and the
/// wasm stack is empty.
fn emit_for_range_setup(
    collection: &IrExpr,
    range_layout: crate::layout::RangeLayout,
    array_layout: crate::layout::ArrayLayout,
    locals: &RangeForLocals,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    lower_expr(collection, sink, ctx)?;
    sink.local_set(locals.range);

    let bound_align_log2 = align_log2(range_layout.bound_align);
    sink.local_get(locals.range);
    sink.i32_load(MemArg {
        offset: 0,
        align: bound_align_log2,
        memory_index: MEMORY_INDEX,
    });
    sink.local_set(locals.start);

    sink.local_get(locals.range);
    sink.i32_load(MemArg {
        offset: u64::from(range_layout.end_offset),
        align: bound_align_log2,
        memory_index: MEMORY_INDEX,
    });
    sink.local_set(locals.end);

    sink.local_get(locals.end);
    sink.local_get(locals.start);
    sink.i32_sub();
    sink.local_set(locals.len);

    allocate_for_output(
        array_layout,
        locals.out_buf,
        locals.out_header,
        locals.len,
        ctx,
        sink,
    )?;

    sink.i32_const(0);
    sink.local_set(locals.i);

    Ok(())
}

/// Emit the Range path's `block` / `loop` pair: per-iteration write
/// `var = start + i`, compute the output address `out_buf + i *
/// elem_size`, lower `body` onto that address, store the body value,
/// then `i += 1` and branch to the loop top. Exit on `i >= len`.
fn emit_for_range_loop(
    body: &IrExpr,
    body_ty: &ResolvedType,
    array_layout: crate::layout::ArrayLayout,
    locals: &RangeForLocals,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let elem_size_signed = elem_size_signed(array_layout)?;

    sink.block(BlockType::Empty);
    sink.loop_(BlockType::Empty);

    sink.local_get(locals.i);
    sink.local_get(locals.len);
    sink.i32_ge_s();
    sink.br_if(1); // exit the surrounding $end block

    sink.local_get(locals.start);
    sink.local_get(locals.i);
    sink.i32_add();
    sink.local_set(locals.var);

    sink.local_get(locals.out_buf);
    sink.local_get(locals.i);
    sink.i32_const(elem_size_signed);
    sink.i32_mul();
    sink.i32_add();

    lower_expr(body, sink, ctx)?;
    store_for_body_value(body_ty, output_field_layout(array_layout), sink)?;

    advance_index(locals.i, sink);

    sink.br(0);
    sink.end(); // close loop
    sink.end(); // close $end block

    Ok(())
}

/// Emit setup for the Array path: lower `collection` to the input
/// array's header pointer, read `in_buf` from offset 0 and `len` from
/// offset 4, and bump-allocate the output element buffer and header.
/// After this helper returns, `i` is pre-set to 0 and the wasm stack
/// is empty.
fn emit_for_array_setup(
    collection: &IrExpr,
    out_layout: crate::layout::ArrayLayout,
    locals: &ArrayForLocals,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    lower_expr(collection, sink, ctx)?;
    sink.local_set(locals.arr);

    let header_align_log2 = align_log2(ARRAY_HEADER_ALIGN);
    sink.local_get(locals.arr);
    sink.i32_load(MemArg {
        offset: 0,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });
    sink.local_set(locals.in_buf);

    sink.local_get(locals.arr);
    sink.i32_load(MemArg {
        offset: 4,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });
    sink.local_set(locals.len);

    allocate_for_output(
        out_layout,
        locals.out_buf,
        locals.out_header,
        locals.len,
        ctx,
        sink,
    )?;

    sink.i32_const(0);
    sink.local_set(locals.i);

    Ok(())
}

/// Emit the Array path's `block` / `loop` pair: per-iteration load
/// `var = *(in_buf + i * in_elem_size)`, compute the output address
/// `out_buf + i * out_elem_size`, lower `body` onto that address,
/// store the body value, then `i += 1` and branch to the loop top.
/// Exit on `i >= len`.
#[expect(
    clippy::too_many_arguments,
    reason = "two array layouts (input + output) plus locals/body context kept explicit so the per-iteration loads and stores stay readable"
)]
fn emit_for_array_loop(
    body: &IrExpr,
    elem_ty: &ResolvedType,
    body_ty: &ResolvedType,
    in_layout: crate::layout::ArrayLayout,
    out_layout: crate::layout::ArrayLayout,
    locals: &ArrayForLocals,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let in_elem_size_signed = elem_size_signed(in_layout)?;
    let out_elem_size_signed = elem_size_signed(out_layout)?;
    let in_field_layout = FieldLayout {
        offset: 0,
        size: in_layout.element_size,
        align: in_layout.element_align,
    };

    sink.block(BlockType::Empty);
    sink.loop_(BlockType::Empty);

    sink.local_get(locals.i);
    sink.local_get(locals.len);
    sink.i32_ge_s();
    sink.br_if(1); // exit the surrounding $end block

    // var = *(in_buf + i * in_elem_size)
    sink.local_get(locals.in_buf);
    sink.local_get(locals.i);
    sink.i32_const(in_elem_size_signed);
    sink.i32_mul();
    sink.i32_add();
    load_for_element(elem_ty, in_field_layout, sink)?;
    sink.local_set(locals.var);

    // Pre-compute the output element address before lowering body.
    sink.local_get(locals.out_buf);
    sink.local_get(locals.i);
    sink.i32_const(out_elem_size_signed);
    sink.i32_mul();
    sink.i32_add();

    lower_expr(body, sink, ctx)?;
    store_for_body_value(body_ty, output_field_layout(out_layout), sink)?;

    advance_index(locals.i, sink);

    sink.br(0);
    sink.end(); // close loop
    sink.end(); // close $end block

    Ok(())
}

/// Emit the right `load` opcode for one element of an Array-sourced
/// For loop's input. Mirrors the dispatch in
/// [`crate::lower::aggregate::lower_dict_access`] but kept local so
/// the control-flow module doesn't depend on the sibling helper.
fn load_for_element(
    elem_ty: &ResolvedType,
    field_layout: FieldLayout,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    match elem_ty {
        ResolvedType::Primitive(p) => {
            load_primitive(*p, field_layout, sink);
            Ok(())
        }
        ResolvedType::Struct(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Array(_) => {
            sink.i32_load(field_mem_arg(field_layout));
            Ok(())
        }
        ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Trait(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Error => Err(LowerError::NotYetImplemented {
            what: format!("for-loop over Array<{elem_ty:?}>"),
        }),
    }
}

/// Bump-allocate the element buffer (`len * elem_size` bytes) and the
/// 12-byte header, parking each pointer in the supplied scratch local.
fn allocate_for_output(
    array_layout: crate::layout::ArrayLayout,
    out_buf: u32,
    out_header: u32,
    len_local: u32,
    ctx: &LowerContext<'_>,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let alloc_idx = ctx.bump_allocator()?;
    let elem_size_signed = elem_size_signed(array_layout)?;
    sink.local_get(len_local);
    sink.i32_const(elem_size_signed);
    sink.i32_mul();
    sink.call(alloc_idx);
    sink.local_set(out_buf);

    sink.i32_const(i32::try_from(array_layout.header_size).map_err(|_| {
        LowerError::Layout(crate::layout::LayoutError::SizeOverflow {
            name: "<for-output header>".to_owned(),
        })
    })?);
    sink.call(alloc_idx);
    sink.local_set(out_header);
    Ok(())
}

/// `i += 1` — shared between both source paths' loop emitters.
fn advance_index(i: u32, sink: &mut InstructionSink<'_>) {
    sink.local_get(i);
    sink.i32_const(1);
    sink.i32_add();
    sink.local_set(i);
}

/// Build the [`FieldLayout`] used for a per-iteration store at
/// `out_buf + i * out_elem_size`. The offset is zero because the
/// caller already added the per-iteration offset onto the address.
const fn output_field_layout(array_layout: crate::layout::ArrayLayout) -> FieldLayout {
    FieldLayout {
        offset: 0,
        size: array_layout.element_size,
        align: array_layout.element_align,
    }
}

fn elem_size_signed(layout: crate::layout::ArrayLayout) -> Result<i32, LowerError> {
    i32::try_from(layout.element_size).map_err(|_| {
        LowerError::Layout(crate::layout::LayoutError::SizeOverflow {
            name: "<for-output element>".to_owned(),
        })
    })
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
