//! Lowering of [`IrExpr::UnaryOp`] (`Neg`, `Not`).

use formalang::ast::{PrimitiveType, UnaryOperator};
use formalang::ir::{IrExpr, ResolvedType};
use wasm_encoder::InstructionSink;

use super::{LowerContext, LowerError, lower_expr};

/// Lower an [`IrExpr::UnaryOp`] onto `sink`. Operand is lowered
/// recursively via [`super::lower_expr`]; the operator dispatch reads
/// the operand's primitive type to choose the right wasm instruction.
///
/// `Neg` on integers lowers to `0 - operand` (wasm has no `i*.neg`);
/// `Neg` on floats uses native `f*.neg`. `Not` on `Boolean` uses
/// `i32.eqz` — which returns `1` iff the operand is `0`, the
/// canonical boolean NOT under the i32 representation.
pub fn lower_unary_op(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::UnaryOp { op, operand, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_unary_op called with non-UnaryOp expression".to_owned(),
        });
    };

    let operand_prim = match operand.ty() {
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
                what: format!("UnaryOp on non-primitive operand type {:?}", operand.ty()),
            });
        }
    };

    match (op, operand_prim) {
        (UnaryOperator::Neg, PrimitiveType::I32) => {
            // wasm has no i32.neg; emit (0 - operand).
            sink.i32_const(0);
            lower_expr(operand, sink, ctx)?;
            sink.i32_sub();
        }
        (UnaryOperator::Neg, PrimitiveType::I64) => {
            sink.i64_const(0);
            lower_expr(operand, sink, ctx)?;
            sink.i64_sub();
        }
        (UnaryOperator::Neg, PrimitiveType::F32) => {
            lower_expr(operand, sink, ctx)?;
            sink.f32_neg();
        }
        (UnaryOperator::Neg, PrimitiveType::F64) => {
            lower_expr(operand, sink, ctx)?;
            sink.f64_neg();
        }
        (UnaryOperator::Not, PrimitiveType::Boolean) => {
            lower_expr(operand, sink, ctx)?;
            sink.i32_eqz();
        }
        (UnaryOperator::Neg | UnaryOperator::Not, _) => {
            return Err(LowerError::UnsupportedOperator {
                op: format!("{op:?}"),
                operand: operand_prim,
            });
        }
        // Future #[non_exhaustive] UnaryOperator variants ride this arm.
        _ => {
            return Err(LowerError::NotYetImplemented {
                what: format!("UnaryOperator::{op:?} on {operand_prim:?}"),
            });
        }
    }

    Ok(())
}
