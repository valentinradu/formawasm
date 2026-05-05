//! Lowering of [`IrExpr::BinaryOp`] (arithmetic, comparison,
//! logical) into core-wasm instructions.

use formalang::ast::{BinaryOperator, PrimitiveType};
use formalang::ir::{IrExpr, ResolvedType};
use wasm_encoder::InstructionSink;

use super::{LowerContext, LowerError, lower_expr, lower_range};

/// Lower an [`IrExpr::BinaryOp`] onto `sink`.
///
/// Operands are lowered recursively via [`super::lower_expr`]; the
/// operator dispatch reads the left operand's primitive type to
/// choose the right wasm instruction.
pub fn lower_binary_op(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::BinaryOp {
        left,
        right,
        op,
        ty,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_binary_op called with non-BinaryOp expression".to_owned(),
        });
    };

    // Range expressions allocate a `{ start, end }` aggregate in
    // linear memory and don't fit the operand-prim arithmetic path.
    if matches!(op, BinaryOperator::Range) {
        let module = ctx.module()?;
        let inner = crate::compound::range_bound(ty, module).ok_or_else(|| {
            LowerError::NotYetImplemented {
                what: format!("Range BinaryOp carrying non-Range type {ty:?}"),
            }
        })?;
        return lower_range(inner, left, right, sink, ctx);
    }

    let operand_prim = match left.ty() {
        ResolvedType::Primitive(p) => *p,
        ResolvedType::Struct(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => {
            return Err(LowerError::NotYetImplemented {
                what: format!("BinaryOp on non-primitive operand type {:?}", left.ty()),
            });
        }
    };

    // String operands route through the `__str_eq` runtime helper.
    // `Eq` calls it directly; `Ne` follows with `i32.eqz` to flip
    // the result. Other operators on strings are still
    // unimplemented.
    if matches!(operand_prim, PrimitiveType::String) {
        return lower_string_binary_op(*op, left, right, sink, ctx);
    }

    lower_expr(left, sink, ctx)?;
    lower_expr(right, sink, ctx)?;
    emit_binary_op(*op, operand_prim, sink)
}

/// Lower a `BinaryOp` whose operands are both `String`-typed.
///
/// `Eq` evaluates both header pointers, hands them to `__str_eq`,
/// and leaves the helper's `i32` result on the stack. `Ne` follows
/// with `i32.eqz` to flip the boolean. Any other operator is rejected
/// here — string concatenation lives in mc11.
fn lower_string_binary_op(
    op: BinaryOperator,
    left: &IrExpr,
    right: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    match op {
        BinaryOperator::Eq | BinaryOperator::Ne => {
            let helper_idx = ctx.str_eq_index()?;
            lower_expr(left, sink, ctx)?;
            lower_expr(right, sink, ctx)?;
            sink.call(helper_idx);
            if matches!(op, BinaryOperator::Ne) {
                sink.i32_eqz();
            }
            Ok(())
        }
        BinaryOperator::Add => {
            // String concatenation: hand both header pointers to
            // `__str_concat`, which allocates a fresh buffer + header
            // and returns the new header pointer.
            let helper_idx = ctx.str_concat_index()?;
            lower_expr(left, sink, ctx)?;
            lower_expr(right, sink, ctx)?;
            sink.call(helper_idx);
            Ok(())
        }
        BinaryOperator::Sub
        | BinaryOperator::Mul
        | BinaryOperator::Div
        | BinaryOperator::Mod
        | BinaryOperator::Lt
        | BinaryOperator::Gt
        | BinaryOperator::Le
        | BinaryOperator::Ge
        | BinaryOperator::And
        | BinaryOperator::Or
        | BinaryOperator::Range
        | _ => Err(LowerError::UnsupportedOperator {
            op: format!("{op:?}"),
            operand: PrimitiveType::String,
        }),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "type-dispatched operator table — splitting hides the per-(op, type) mapping"
)]
fn emit_binary_op(
    op: BinaryOperator,
    operand: PrimitiveType,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let unsupported = || LowerError::UnsupportedOperator {
        op: format!("{op:?}"),
        operand,
    };

    match (op, operand) {
        // ── Integer arithmetic ──────────────────────────────────────
        (BinaryOperator::Add, PrimitiveType::I32) => {
            sink.i32_add();
        }
        (BinaryOperator::Sub, PrimitiveType::I32) => {
            sink.i32_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::I32) => {
            sink.i32_mul();
        }
        (BinaryOperator::Div, PrimitiveType::I32) => {
            sink.i32_div_s();
        }
        (BinaryOperator::Mod, PrimitiveType::I32) => {
            sink.i32_rem_s();
        }
        (BinaryOperator::Add, PrimitiveType::I64) => {
            sink.i64_add();
        }
        (BinaryOperator::Sub, PrimitiveType::I64) => {
            sink.i64_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::I64) => {
            sink.i64_mul();
        }
        (BinaryOperator::Div, PrimitiveType::I64) => {
            sink.i64_div_s();
        }
        (BinaryOperator::Mod, PrimitiveType::I64) => {
            sink.i64_rem_s();
        }

        // ── Float arithmetic (no Mod — wasm has no f*.rem) ──────────
        (BinaryOperator::Add, PrimitiveType::F32) => {
            sink.f32_add();
        }
        (BinaryOperator::Sub, PrimitiveType::F32) => {
            sink.f32_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::F32) => {
            sink.f32_mul();
        }
        (BinaryOperator::Div, PrimitiveType::F32) => {
            sink.f32_div();
        }
        (BinaryOperator::Add, PrimitiveType::F64) => {
            sink.f64_add();
        }
        (BinaryOperator::Sub, PrimitiveType::F64) => {
            sink.f64_sub();
        }
        (BinaryOperator::Mul, PrimitiveType::F64) => {
            sink.f64_mul();
        }
        (BinaryOperator::Div, PrimitiveType::F64) => {
            sink.f64_div();
        }
        // ── Comparisons ─────────────────────────────────────────────
        (BinaryOperator::Eq, PrimitiveType::I32 | PrimitiveType::Boolean) => {
            sink.i32_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::I32 | PrimitiveType::Boolean) => {
            sink.i32_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::I32) => {
            sink.i32_lt_s();
        }
        (BinaryOperator::Gt, PrimitiveType::I32) => {
            sink.i32_gt_s();
        }
        (BinaryOperator::Le, PrimitiveType::I32) => {
            sink.i32_le_s();
        }
        (BinaryOperator::Ge, PrimitiveType::I32) => {
            sink.i32_ge_s();
        }
        (BinaryOperator::Eq, PrimitiveType::I64) => {
            sink.i64_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::I64) => {
            sink.i64_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::I64) => {
            sink.i64_lt_s();
        }
        (BinaryOperator::Gt, PrimitiveType::I64) => {
            sink.i64_gt_s();
        }
        (BinaryOperator::Le, PrimitiveType::I64) => {
            sink.i64_le_s();
        }
        (BinaryOperator::Ge, PrimitiveType::I64) => {
            sink.i64_ge_s();
        }
        (BinaryOperator::Eq, PrimitiveType::F32) => {
            sink.f32_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::F32) => {
            sink.f32_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::F32) => {
            sink.f32_lt();
        }
        (BinaryOperator::Gt, PrimitiveType::F32) => {
            sink.f32_gt();
        }
        (BinaryOperator::Le, PrimitiveType::F32) => {
            sink.f32_le();
        }
        (BinaryOperator::Ge, PrimitiveType::F32) => {
            sink.f32_ge();
        }
        (BinaryOperator::Eq, PrimitiveType::F64) => {
            sink.f64_eq();
        }
        (BinaryOperator::Ne, PrimitiveType::F64) => {
            sink.f64_ne();
        }
        (BinaryOperator::Lt, PrimitiveType::F64) => {
            sink.f64_lt();
        }
        (BinaryOperator::Gt, PrimitiveType::F64) => {
            sink.f64_gt();
        }
        (BinaryOperator::Le, PrimitiveType::F64) => {
            sink.f64_le();
        }
        (BinaryOperator::Ge, PrimitiveType::F64) => {
            sink.f64_ge();
        }

        // ── Logical (Boolean only — i32 representation, eager eval) ─
        (BinaryOperator::And, PrimitiveType::Boolean) => {
            sink.i32_and();
        }
        (BinaryOperator::Or, PrimitiveType::Boolean) => {
            sink.i32_or();
        }

        // ── Disallowed combinations + future variants ───────────────
        (
            BinaryOperator::Add
            | BinaryOperator::Sub
            | BinaryOperator::Mul
            | BinaryOperator::Div
            | BinaryOperator::Mod
            | BinaryOperator::Lt
            | BinaryOperator::Gt
            | BinaryOperator::Le
            | BinaryOperator::Ge
            | BinaryOperator::And
            | BinaryOperator::Or,
            _,
        ) => return Err(unsupported()),
        // Future #[non_exhaustive] BinaryOperator variants ride this arm.
        _ => {
            return Err(LowerError::NotYetImplemented {
                what: format!("BinaryOperator::{op:?} on {operand:?}"),
            });
        }
    }

    Ok(())
}
