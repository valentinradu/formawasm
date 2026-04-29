//! Lowering of [`IrExpr::FunctionCall`] (direct calls) and
//! [`IrExpr::MethodCall`] static dispatch.
//!
//! Indirect calls (closures, vtables) live in Phase 1b/3 alongside
//! `ClosureRef` and virtual dispatch.

use formalang::ir::{DispatchKind, IrExpr};
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
    for (_, arg) in args {
        lower_expr(arg, sink, ctx)?;
    }
    sink.call(wasm_idx);
    Ok(())
}
