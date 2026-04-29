//! Lowering of [`IrExpr::Block`] and the function-body assembler
//! that plans wasm locals before instruction emission.

use formalang::ast::PrimitiveType;
use formalang::ir::{BindingId, IrBlockStatement, IrExpr, ResolvedType};
use wasm_encoder::{Function, InstructionSink, ValType};

use super::{BindingMap, FunctionMap, LowerContext, LowerError, lower_expr};
use crate::types::resolved_value_type;

/// Lower an [`IrExpr::Block`] onto `sink`. Statements run in order;
/// the result expression's value becomes the block's value (left on
/// the stack).
pub fn lower_block(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::Block {
        statements, result, ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_block called with non-Block expression".to_owned(),
        });
    };

    for stmt in statements {
        lower_block_statement(stmt, sink, ctx)?;
    }
    lower_expr(result, sink, ctx)
}

fn lower_block_statement(
    stmt: &IrBlockStatement,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    match stmt {
        IrBlockStatement::Let {
            binding_id, value, ..
        } => {
            let idx = ctx
                .bindings
                .get(*binding_id)
                .ok_or(LowerError::UnknownBinding(*binding_id))?;
            lower_expr(value, sink, ctx)?;
            sink.local_set(idx);
            Ok(())
        }
        IrBlockStatement::Expr(e) => {
            // A statement-position expression's value would otherwise
            // unbalance the wasm operand stack. Drop unless the type
            // is `Never` (no value flows past `unreachable`).
            let needs_drop = !matches!(e.ty(), ResolvedType::Primitive(PrimitiveType::Never));
            lower_expr(e, sink, ctx)?;
            if needs_drop {
                sink.drop();
            }
            Ok(())
        }
        IrBlockStatement::Assign { .. } => Err(LowerError::NotYetImplemented {
            what: "IrBlockStatement::Assign (Phase 1b)".to_owned(),
        }),
    }
}

/// Walk an expression tree and collect every `IrBlockStatement::Let`
/// binding it introduces, in declaration order. Used by
/// [`lower_function_body`] to plan wasm locals before instruction
/// emission.
fn collect_local_bindings(expr: &IrExpr) -> Result<Vec<(BindingId, ValType)>, LowerError> {
    let mut out = Vec::new();
    walk_for_locals(expr, &mut out)?;
    Ok(out)
}

fn walk_for_locals(expr: &IrExpr, out: &mut Vec<(BindingId, ValType)>) -> Result<(), LowerError> {
    match expr {
        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                match stmt {
                    IrBlockStatement::Let {
                        binding_id,
                        name,
                        value,
                        ty,
                        ..
                    } => {
                        let resolved = ty.as_ref().unwrap_or_else(|| value.ty());
                        let vt = resolved_value_type(resolved)?.ok_or_else(|| {
                            LowerError::ZeroSizedLetBinding {
                                name: name.clone(),
                                ty: resolved.clone(),
                            }
                        })?;
                        out.push((*binding_id, vt));
                        walk_for_locals(value, out)?;
                    }
                    IrBlockStatement::Assign { target, value } => {
                        walk_for_locals(target, out)?;
                        walk_for_locals(value, out)?;
                    }
                    IrBlockStatement::Expr(e) => walk_for_locals(e, out)?,
                }
            }
            walk_for_locals(result, out)
        }

        IrExpr::BinaryOp { left, right, .. } => {
            walk_for_locals(left, out)?;
            walk_for_locals(right, out)
        }
        IrExpr::UnaryOp { operand, .. } => walk_for_locals(operand, out),
        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            walk_for_locals(condition, out)?;
            walk_for_locals(then_branch, out)?;
            if let Some(else_branch) = else_branch {
                walk_for_locals(else_branch, out)?;
            }
            Ok(())
        }
        IrExpr::FunctionCall { args, .. } => {
            for (_, arg) in args {
                walk_for_locals(arg, out)?;
            }
            Ok(())
        }

        // Leaves and not-yet-supported variants — no inner locals.
        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::FieldAccess { .. }
        | IrExpr::StructInst { .. }
        | IrExpr::EnumInst { .. }
        | IrExpr::Tuple { .. }
        | IrExpr::Array { .. }
        | IrExpr::For { .. }
        | IrExpr::Match { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::Closure { .. }
        | IrExpr::ClosureRef { .. }
        | IrExpr::DictLiteral { .. }
        | IrExpr::DictAccess { .. } => Ok(()),
    }
}

/// Build a wasm [`Function`] body from an `IrExpr` and a list of
/// `(BindingId, ValType)` parameters.
///
/// Plans the locals upfront, sets up the [`BindingMap`] so params
/// live at indices `0..N` and `let` bindings live at `N..M`, then
/// lowers the body and emits the closing `end`. `functions` carries
/// the module-level `FunctionId` -> wasm-index map; pass an empty
/// [`FunctionMap`] when the body makes no calls.
pub fn lower_function_body(
    body: &IrExpr,
    param_bindings: &[(BindingId, ValType)],
    functions: &FunctionMap,
) -> Result<Function, LowerError> {
    let local_bindings = collect_local_bindings(body)?;

    let mut binding_map = BindingMap::new();
    for (i, (id, _)) in param_bindings.iter().enumerate() {
        binding_map.insert(*id, index_of(i)?);
    }
    let local_offset = u32::try_from(param_bindings.len()).unwrap_or(u32::MAX);
    for (i, (id, _)) in local_bindings.iter().enumerate() {
        let idx = local_offset.saturating_add(index_of(i)?);
        binding_map.insert(*id, idx);
    }

    let ctx = LowerContext::new(&binding_map, functions);
    let locals: Vec<(u32, ValType)> = local_bindings.iter().map(|(_, vt)| (1, *vt)).collect();
    let mut func = Function::new(locals);
    {
        let sink = &mut func.instructions();
        lower_expr(body, sink, &ctx)?;
        sink.end();
    }
    Ok(func)
}

fn index_of(i: usize) -> Result<u32, LowerError> {
    u32::try_from(i).map_err(|_| LowerError::NotYetImplemented {
        what: "more than u32::MAX locals in a single function".to_owned(),
    })
}
