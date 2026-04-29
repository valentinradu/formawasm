//! Lowering of [`IrExpr::FunctionCall`] (direct calls).
//!
//! Indirect calls (closures, vtables) live in Phase 1b/3 alongside
//! `ClosureRef` and virtual dispatch.

use formalang::ir::IrExpr;
use wasm_encoder::InstructionSink;

use super::{LowerContext, LowerError, lower_expr};

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

    for (_, arg) in args {
        lower_expr(arg, sink, ctx)?;
    }
    sink.call(wasm_idx);
    Ok(())
}
