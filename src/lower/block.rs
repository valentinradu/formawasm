//! Lowering of [`IrExpr::Block`] and the function-body assembler
//! that plans wasm locals before instruction emission.

use std::cell::Cell;

use formalang::ast::PrimitiveType;
use formalang::ir::{BindingId, IrBlockStatement, IrExpr, IrModule, ResolvedType, StructId};
use wasm_encoder::{Function, InstructionSink, ValType};

use super::{BindingMap, FunctionMap, LowerContext, LowerError, MethodMap, lower_expr};
use crate::types::body_value_type;

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
        IrBlockStatement::Assign { target, value } => lower_assign(target, value, sink, ctx),
    }
}

/// Lower an `Assign` statement.
///
/// Phase 1b mc9 supports field-write targets — `self.x = value` and
/// `obj.x = value` — by emitting the value's bytes at the resolved
/// field offset of the object's pointer. Primitive lvalues (mutable
/// `let` bindings) and tuple-element writes ride later phases.
fn lower_assign(
    target: &IrExpr,
    value: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    use super::aggregate::{
        layout_for_aggregate, lookup_field_by_name, lookup_field_by_name_with_meta, primitive_of,
        store_primitive,
    };
    use crate::layout::plan_struct;

    match target {
        IrExpr::SelfFieldRef {
            field, field_idx, ..
        } => {
            let struct_id = ctx.self_struct_id.ok_or(LowerError::MissingSelfStruct)?;
            let module = ctx.module()?;
            let s = module
                .structs
                .get(struct_id.0 as usize)
                .ok_or(LowerError::UnknownStruct(struct_id))?;
            let layout = plan_struct(s, module)?;
            let idx = field_idx.0 as usize;
            let (field_layout, field_def) = if let Some(fl) = layout.fields.get(idx)
                && let Some(fd) = s.fields.get(idx)
            {
                (fl, fd)
            } else {
                lookup_field_by_name(s, &layout.fields, field)?
            };
            let primitive = primitive_of(&field_def.ty)?;
            sink.local_get(0);
            lower_expr(value, sink, ctx)?;
            store_primitive(primitive, *field_layout, sink);
            Ok(())
        }
        IrExpr::FieldAccess {
            object,
            field,
            field_idx,
            ..
        } => {
            let module = ctx.module()?;
            let (layout, fields_meta) = layout_for_aggregate(object.ty(), module)?;
            let idx = field_idx.0 as usize;
            let (field_layout, field_def) = if let Some(fl) = layout.fields.get(idx)
                && let Some(fd) = fields_meta.get(idx)
            {
                (fl, fd)
            } else {
                lookup_field_by_name_with_meta(&fields_meta, &layout.fields, field, "<aggregate>")?
            };
            let primitive = primitive_of(&field_def.ty)?;
            lower_expr(object, sink, ctx)?;
            lower_expr(value, sink, ctx)?;
            store_primitive(primitive, *field_layout, sink);
            Ok(())
        }
        IrExpr::Literal { .. }
        | IrExpr::StructInst { .. }
        | IrExpr::EnumInst { .. }
        | IrExpr::Array { .. }
        | IrExpr::Tuple { .. }
        | IrExpr::Reference { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::BinaryOp { .. }
        | IrExpr::UnaryOp { .. }
        | IrExpr::If { .. }
        | IrExpr::For { .. }
        | IrExpr::Match { .. }
        | IrExpr::FunctionCall { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::Closure { .. }
        | IrExpr::ClosureRef { .. }
        | IrExpr::DictLiteral { .. }
        | IrExpr::DictAccess { .. }
        | IrExpr::Block { .. } => Err(LowerError::NotYetImplemented {
            what: "IrBlockStatement::Assign target shape (only field writes supported in mc9)"
                .to_owned(),
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

fn walk_block_statements(
    statements: &[IrBlockStatement],
    out: &mut Vec<(BindingId, ValType)>,
) -> Result<(), LowerError> {
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
                let vt =
                    body_value_type(resolved)?.ok_or_else(|| LowerError::ZeroSizedLetBinding {
                        name: name.clone(),
                        ty: resolved.clone(),
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
    Ok(())
}

fn walk_for_locals(expr: &IrExpr, out: &mut Vec<(BindingId, ValType)>) -> Result<(), LowerError> {
    match expr {
        IrExpr::Block {
            statements, result, ..
        } => {
            walk_block_statements(statements, out)?;
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
        IrExpr::MethodCall { receiver, args, .. } => {
            walk_for_locals(receiver, out)?;
            for (_, arg) in args {
                walk_for_locals(arg, out)?;
            }
            Ok(())
        }
        IrExpr::FieldAccess { object, .. } => walk_for_locals(object, out),
        IrExpr::ClosureRef { env_struct, .. } => walk_for_locals(env_struct, out),
        IrExpr::StructInst { fields, .. } | IrExpr::EnumInst { fields, .. } => {
            for (_, _, value) in fields {
                walk_for_locals(value, out)?;
            }
            Ok(())
        }
        IrExpr::Tuple { fields, .. } => {
            for (_, value) in fields {
                walk_for_locals(value, out)?;
            }
            Ok(())
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            walk_for_locals(scrutinee, out)?;
            for arm in arms {
                for (name, binding_id, ty) in &arm.bindings {
                    let vt =
                        body_value_type(ty)?.ok_or_else(|| LowerError::ZeroSizedLetBinding {
                            name: name.clone(),
                            ty: ty.clone(),
                        })?;
                    out.push((*binding_id, vt));
                }
                walk_for_locals(&arm.body, out)?;
            }
            Ok(())
        }

        // Leaves and not-yet-supported variants — no inner locals.
        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::Array { .. }
        | IrExpr::For { .. }
        | IrExpr::Closure { .. }
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
///
/// This entry point omits the [`IrModule`] reference and the bump-
/// allocator function index, so any aggregate lowering inside `body`
/// will surface a [`LowerError::MissingContext`]. Use
/// [`lower_function_body_in_module`] when the body can construct
/// structs / tuples / enums.
pub fn lower_function_body(
    body: &IrExpr,
    param_bindings: &[(BindingId, ValType)],
    functions: &FunctionMap,
) -> Result<Function, LowerError> {
    let plan = plan_function_locals(body, param_bindings)?;
    let ctx = LowerContext::new(&plan.bindings, functions);
    finish_function_body(body, plan.locals, &ctx)
}

/// Module-aware variant of [`lower_function_body`].
///
/// Wires the [`IrModule`] reference and the bump-allocator function
/// index into the [`LowerContext`]. Aggregate lowerings invoke the
/// bump allocator and consult the module to look up struct / enum
/// definitions, so they require this entry point. Also pre-walks
/// `body` to count aggregate constructions and reserves one
/// `i32`-typed scratch local per occurrence so the recursive
/// lowering can stash each base pointer without clobbering enclosing
/// constructions.
pub fn lower_function_body_in_module(
    body: &IrExpr,
    param_bindings: &[(BindingId, ValType)],
    functions: &FunctionMap,
    methods: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    self_struct_id: Option<StructId>,
) -> Result<Function, LowerError> {
    let plan = plan_function_locals(body, param_bindings)?;
    let scratch_count = count_aggregates(body)?;
    let scratch_offset = scratch_locals_offset(param_bindings.len(), plan.locals.len())?;

    let mut locals = plan.locals;
    if scratch_count > 0 {
        locals.push((scratch_count, ValType::I32));
    }

    let scratch_counter = Cell::new(scratch_offset);
    let mut ctx = LowerContext::new(&plan.bindings, functions)
        .with_methods(methods)
        .with_module(module)
        .with_bump_allocator(bump_allocator)
        .with_scratch_locals(&scratch_counter);
    if let Some(id) = self_struct_id {
        ctx = ctx.with_self_struct_id(id);
    }
    finish_function_body(body, locals, &ctx)
}

fn scratch_locals_offset(params: usize, lets: usize) -> Result<u32, LowerError> {
    let p = u32::try_from(params).map_err(|_| LowerError::NotYetImplemented {
        what: "more than u32::MAX parameters in a single function".to_owned(),
    })?;
    let l = u32::try_from(lets).map_err(|_| LowerError::NotYetImplemented {
        what: "more than u32::MAX `let` bindings in a single function".to_owned(),
    })?;
    p.checked_add(l)
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: "params + lets overflow u32 in a single function".to_owned(),
        })
}

fn count_aggregates(expr: &IrExpr) -> Result<u32, LowerError> {
    let mut n: u32 = 0;
    walk_count(expr, &mut n)?;
    Ok(n)
}

fn bump_count(n: &mut u32) -> Result<(), LowerError> {
    *n = n
        .checked_add(1)
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: "more than u32::MAX aggregate constructions in a single function".to_owned(),
        })?;
    Ok(())
}

fn walk_count(expr: &IrExpr, out: &mut u32) -> Result<(), LowerError> {
    match expr {
        IrExpr::StructInst { fields, .. } | IrExpr::EnumInst { fields, .. } => {
            bump_count(out)?;
            for (_, _, e) in fields {
                walk_count(e, out)?;
            }
        }
        IrExpr::Tuple { fields, .. } => {
            bump_count(out)?;
            for (_, e) in fields {
                walk_count(e, out)?;
            }
        }
        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                match stmt {
                    IrBlockStatement::Let { value, .. } => walk_count(value, out)?,
                    IrBlockStatement::Assign { target, value } => {
                        walk_count(target, out)?;
                        walk_count(value, out)?;
                    }
                    IrBlockStatement::Expr(e) => walk_count(e, out)?,
                }
            }
            walk_count(result, out)?;
        }
        IrExpr::BinaryOp {
            left, right, op, ..
        } => {
            // `BinaryOperator::Range` allocates a `{ start, end }`
            // aggregate in linear memory and reserves one scratch
            // local for the base pointer.
            if matches!(op, formalang::ast::BinaryOperator::Range) {
                bump_count(out)?;
            }
            walk_count(left, out)?;
            walk_count(right, out)?;
        }
        IrExpr::UnaryOp { operand, .. } => walk_count(operand, out)?,
        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            walk_count(condition, out)?;
            walk_count(then_branch, out)?;
            if let Some(else_branch) = else_branch {
                walk_count(else_branch, out)?;
            }
        }
        IrExpr::FunctionCall { args, .. } => {
            for (_, arg) in args {
                walk_count(arg, out)?;
            }
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            walk_count(receiver, out)?;
            for (_, arg) in args {
                walk_count(arg, out)?;
            }
        }
        IrExpr::FieldAccess { object, .. } => walk_count(object, out)?,
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            // Each `Match` reserves one scratch local for the
            // scrutinee pointer.
            bump_count(out)?;
            walk_count(scrutinee, out)?;
            for arm in arms {
                walk_count(&arm.body, out)?;
            }
        }
        IrExpr::ClosureRef { env_struct, .. } => {
            // Each ClosureRef reserves a scratch local for the
            // (funcref, env_ptr) pair's base pointer.
            bump_count(out)?;
            walk_count(env_struct, out)?;
        }
        IrExpr::Array { elements, .. } => {
            // Each Array literal reserves two scratch locals — one
            // for the element-buffer base pointer, one for the
            // header pointer.
            bump_count(out)?;
            bump_count(out)?;
            for e in elements {
                walk_count(e, out)?;
            }
        }

        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::For { .. }
        | IrExpr::Closure { .. }
        | IrExpr::DictLiteral { .. }
        | IrExpr::DictAccess { .. } => {}
    }
    Ok(())
}

/// Pre-computed bindings + per-let local types for a function body.
struct FunctionPlan {
    bindings: BindingMap,
    locals: Vec<(u32, ValType)>,
}

fn plan_function_locals(
    body: &IrExpr,
    param_bindings: &[(BindingId, ValType)],
) -> Result<FunctionPlan, LowerError> {
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

    let locals: Vec<(u32, ValType)> = local_bindings.iter().map(|(_, vt)| (1, *vt)).collect();
    Ok(FunctionPlan {
        bindings: binding_map,
        locals,
    })
}

fn finish_function_body(
    body: &IrExpr,
    locals: Vec<(u32, ValType)>,
    ctx: &LowerContext<'_>,
) -> Result<Function, LowerError> {
    let mut func = Function::new(locals);
    {
        let sink = &mut func.instructions();
        lower_expr(body, sink, ctx)?;
        sink.end();
    }
    Ok(func)
}

fn index_of(i: usize) -> Result<u32, LowerError> {
    u32::try_from(i).map_err(|_| LowerError::NotYetImplemented {
        what: "more than u32::MAX locals in a single function".to_owned(),
    })
}
