//! Lowering of control-flow expressions ([`IrExpr::If`] today;
//! [`IrExpr::For`] and [`IrExpr::Match`] land in Phase 1b/1c).

use formalang::ir::IrExpr;
use wasm_encoder::{BlockType, InstructionSink};

use super::{LowerContext, LowerError, lower_expr};
use crate::types::resolved_value_type;

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
