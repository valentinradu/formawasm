//! Lowering of control-flow expressions: [`IrExpr::If`] and
//! [`IrExpr::Match`]. [`IrExpr::For`] lands in Phase 1c.

use formalang::ir::{IrExpr, IrMatchArm, ResolvedType};
use wasm_encoder::{BlockType, InstructionSink, MemArg};

use super::aggregate::{load_primitive, primitive_of};
use super::{LowerContext, LowerError, lower_expr};
use crate::layout::{ENUM_TAG_ALIGN, plan_enum};
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
