//! Lowering of [`IrExpr::Block`] and the function-body assembler
//! that plans wasm locals before instruction emission.

use formalang::ast::PrimitiveType;
use formalang::ir::{BindingId, IrBlockStatement, IrExpr, IrModule, ResolvedType, StructId};
use wasm_encoder::{Function, InstructionSink, ValType};

use super::{
    BindingMap, ClosureCallContext, FunctionMap, LowerContext, LowerError, MethodMap,
    ScratchAllocator, lower_expr,
};
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
            binding_id,
            ty,
            value,
            ..
        } => {
            let idx = ctx
                .bindings
                .get(*binding_id)
                .ok_or(LowerError::UnknownBinding(*binding_id))?;
            // If the binding's annotated type is `Optional<T>` and the
            // value's static type is exactly `T`, the coercion path
            // wraps as Some before storing. Other type combinations
            // (Optional<Never> -> Optional<T>, exact match) flow
            // through as plain pointers via the regular lowering path.
            if let Some(target) = ty.as_ref() {
                super::optional::lower_coerced(value, target, sink, ctx)?;
            } else {
                lower_expr(value, sink, ctx)?;
            }
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
        | IrExpr::CallClosure { .. }
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

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive walk over every IrExpr variant; splitting hides which variants introduce new bindings"
)]
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
        IrExpr::CallClosure { closure, args, .. } => {
            walk_for_locals(closure, out)?;
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
        IrExpr::DictAccess { dict, key, .. } => {
            walk_for_locals(dict, out)?;
            walk_for_locals(key, out)
        }
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
        IrExpr::For {
            var,
            var_ty,
            var_binding_id,
            collection,
            body,
            ..
        } => {
            // The loop variable acts like a let binding: register it
            // in the binding map so body references resolve through
            // `lower_let_ref`.
            let vt = body_value_type(var_ty)?.ok_or_else(|| LowerError::ZeroSizedLetBinding {
                name: var.clone(),
                ty: var_ty.clone(),
            })?;
            out.push((*var_binding_id, vt));
            walk_for_locals(collection, out)?;
            walk_for_locals(body, out)
        }

        // Leaves and not-yet-supported variants — no inner locals.
        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::Array { .. }
        | IrExpr::Closure { .. }
        | IrExpr::DictLiteral { .. } => Ok(()),
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
    finish_function_body(body, None, plan.locals, &ctx)
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
#[expect(
    clippy::too_many_arguments,
    reason = "module-aware body lowering needs every map and table-context input the called expression lowerings can possibly read; bundling into a struct hides the contract"
)]
pub fn lower_function_body_in_module(
    body: &IrExpr,
    return_ty: Option<&ResolvedType>,
    param_bindings: &[(BindingId, ValType)],
    functions: &FunctionMap,
    methods: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    self_struct_id: Option<StructId>,
    closure_ctx: Option<&ClosureCallContext<'_>>,
) -> Result<Function, LowerError> {
    let plan = plan_function_locals(body, param_bindings)?;
    let counts = count_scratch_locals(body, return_ty, Some(module))?;
    let scratch_offset = scratch_locals_offset(param_bindings.len(), plan.locals.len())?;

    let mut locals = plan.locals;
    let mut running = scratch_offset;
    let mut next_region = |count: u32, ty: ValType| -> Result<u32, LowerError> {
        let base = running;
        if count > 0 {
            locals.push((count, ty));
            running = running
                .checked_add(count)
                .ok_or_else(|| LowerError::NotYetImplemented {
                    what: "scratch-local layout overflows u32".to_owned(),
                })?;
        }
        Ok(base)
    };
    let i32_base = next_region(counts.i32, ValType::I32)?;
    let i64_base = next_region(counts.i64, ValType::I64)?;
    let f32_base = next_region(counts.f32, ValType::F32)?;
    let f64_base = next_region(counts.f64, ValType::F64)?;

    let allocator = ScratchAllocator::new(super::ScratchRegions {
        i32: (i32_base, counts.i32),
        i64: (i64_base, counts.i64),
        f32: (f32_base, counts.f32),
        f64: (f64_base, counts.f64),
    });
    let mut ctx = LowerContext::new(&plan.bindings, functions)
        .with_methods(methods)
        .with_module(module)
        .with_bump_allocator(bump_allocator)
        .with_scratch_locals(&allocator);
    if let Some(id) = self_struct_id {
        ctx = ctx.with_self_struct_id(id);
    }
    if let Some(closure) = closure_ctx {
        ctx = ctx
            .with_closure_table(closure.table_idx)
            .with_closure_funcref_indices(closure.funcref_indices)
            .with_closure_type_indices(closure.type_indices);
    }
    finish_function_body(body, return_ty, locals, &ctx)
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

/// Per-wasm-value-type scratch-local counts a function body needs. The
/// pre-walk in [`walk_count`] populates this; [`lower_function_body_in_module`]
/// turns it into reserved local-vector ranges and a [`ScratchAllocator`].
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct ScratchCounts {
    pub i32: u32,
    pub i64: u32,
    pub f32: u32,
    pub f64: u32,
}

fn count_scratch_locals(
    expr: &IrExpr,
    return_ty: Option<&ResolvedType>,
    module: Option<&IrModule>,
) -> Result<ScratchCounts, LowerError> {
    let mut counts = ScratchCounts::default();
    // The function's body value gets coerced to `return_ty` at the
    // closing site, so any Some-wrap that happens there reserves
    // scratch slots up-front just like the per-expression sites.
    if let Some(target) = return_ty {
        super::optional::coercion_scratch_counts(target, expr.ty(), &mut counts)?;
    }
    walk_count(expr, module, &mut counts)?;
    Ok(counts)
}

pub(super) fn bump_count(field: &mut u32) -> Result<(), LowerError> {
    *field = field
        .checked_add(1)
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: "more than u32::MAX scratch slots of one type in a single function".to_owned(),
        })?;
    Ok(())
}

fn walk_count_block_statement(
    stmt: &IrBlockStatement,
    module: Option<&IrModule>,
    out: &mut ScratchCounts,
) -> Result<(), LowerError> {
    match stmt {
        IrBlockStatement::Let { ty, value, .. } => {
            if let Some(target) = ty.as_ref() {
                super::optional::coercion_scratch_counts(target, value.ty(), out)?;
            }
            walk_count(value, module, out)
        }
        IrBlockStatement::Assign { target, value } => {
            walk_count(target, module, out)?;
            walk_count(value, module, out)
        }
        IrBlockStatement::Expr(e) => walk_count(e, module, out),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "exhaustive walk over every IrExpr variant; splitting hides which variants reserve which scratch slots"
)]
fn walk_count(
    expr: &IrExpr,
    module: Option<&IrModule>,
    out: &mut ScratchCounts,
) -> Result<(), LowerError> {
    match expr {
        IrExpr::StructInst {
            struct_id, fields, ..
        } => {
            bump_count(&mut out.i32)?;
            // Each field initializer flows into the struct field's
            // declared type — Some-wrap widens a plain T into an
            // Optional<T> field.
            if let Some(id) = struct_id
                && let Some(m) = module
                && let Some(s) = m.structs.get(id.0 as usize)
            {
                for (name, _idx, e) in fields {
                    if let Some(decl) = s.fields.iter().find(|f| f.name == *name) {
                        super::optional::coercion_scratch_counts(&decl.ty, e.ty(), out)?;
                    }
                    walk_count(e, module, out)?;
                }
            } else {
                for (_, _, e) in fields {
                    walk_count(e, module, out)?;
                }
            }
        }
        IrExpr::EnumInst {
            enum_id,
            variant_idx,
            fields,
            ..
        } => {
            bump_count(&mut out.i32)?;
            if let Some(id) = enum_id
                && let Some(m) = module
                && let Some(e) = m.enums.get(id.0 as usize)
                && let Some(v) = e.variants.get(variant_idx.0 as usize)
            {
                for (name, _idx, value) in fields {
                    if let Some(decl) = v.fields.iter().find(|f| f.name == *name) {
                        super::optional::coercion_scratch_counts(&decl.ty, value.ty(), out)?;
                    }
                    walk_count(value, module, out)?;
                }
            } else {
                for (_, _, value) in fields {
                    walk_count(value, module, out)?;
                }
            }
        }
        IrExpr::Tuple { fields, .. } => {
            bump_count(&mut out.i32)?;
            for (_, e) in fields {
                walk_count(e, module, out)?;
            }
        }
        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                walk_count_block_statement(stmt, module, out)?;
            }
            walk_count(result, module, out)?;
        }
        IrExpr::BinaryOp {
            left, right, op, ..
        } => {
            // `BinaryOperator::Range` allocates a `{ start, end }`
            // aggregate in linear memory and reserves one i32 scratch
            // local for the base pointer.
            if matches!(op, formalang::ast::BinaryOperator::Range) {
                bump_count(&mut out.i32)?;
            }
            walk_count(left, module, out)?;
            walk_count(right, module, out)?;
        }
        IrExpr::UnaryOp { operand, .. } => walk_count(operand, module, out)?,
        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ty,
        } => {
            walk_count(condition, module, out)?;
            // Each branch's value is coerced to the if's overall type
            // (`Optional` widening only — every other type combination
            // contributes nothing). Count those wraps so the pre-walk's
            // totals match the lowering walker.
            super::optional::coercion_scratch_counts(ty, then_branch.ty(), out)?;
            walk_count(then_branch, module, out)?;
            if let Some(else_branch) = else_branch {
                super::optional::coercion_scratch_counts(ty, else_branch.ty(), out)?;
                walk_count(else_branch, module, out)?;
            }
        }
        IrExpr::FunctionCall {
            function_id, args, ..
        } => {
            // Each call argument flows into the callee's declared
            // parameter type. Look the function up in the module so
            // Some-wrap widening counts at the call site too.
            if let Some(id) = function_id
                && let Some(m) = module
                && let Some(f) = m.functions.get(id.0 as usize)
            {
                for (param_name, arg) in args {
                    let target = param_name.as_ref().and_then(|n| {
                        f.params
                            .iter()
                            .find(|p| p.name == *n)
                            .and_then(|p| p.ty.as_ref())
                    });
                    if let Some(t) = target {
                        super::optional::coercion_scratch_counts(t, arg.ty(), out)?;
                    }
                    walk_count(arg, module, out)?;
                }
            } else {
                for (_, arg) in args {
                    walk_count(arg, module, out)?;
                }
            }
        }
        IrExpr::CallClosure { closure, args, .. } => {
            // One i32 scratch local for the closure value's base
            // pointer — re-read from once for env_ptr and once for the
            // funcref index inside `lower_call_closure`.
            bump_count(&mut out.i32)?;
            walk_count(closure, module, out)?;
            for (_, arg) in args {
                walk_count(arg, module, out)?;
            }
        }
        IrExpr::MethodCall {
            receiver,
            method_idx,
            args,
            dispatch,
            ..
        } => {
            walk_count(receiver, module, out)?;
            // Static-dispatch method args coerce to their declared
            // parameter types; virtual dispatch isn't supported yet.
            let method_sig = match dispatch {
                formalang::ir::DispatchKind::Static { impl_id } => module
                    .and_then(|m| m.impls.get(impl_id.0 as usize))
                    .and_then(|i| i.functions.get(method_idx.0 as usize)),
                formalang::ir::DispatchKind::Virtual { .. } => None,
            };
            for (param_name, arg) in args {
                let target = method_sig.and_then(|sig| {
                    param_name.as_ref().and_then(|n| {
                        sig.params
                            .iter()
                            .find(|p| p.name == *n)
                            .and_then(|p| p.ty.as_ref())
                    })
                });
                if let Some(t) = target {
                    super::optional::coercion_scratch_counts(t, arg.ty(), out)?;
                }
                walk_count(arg, module, out)?;
            }
        }
        IrExpr::FieldAccess { object, .. } => walk_count(object, module, out)?,
        IrExpr::DictAccess { dict, key, .. } => {
            walk_count(dict, module, out)?;
            walk_count(key, module, out)?;
        }
        IrExpr::Match {
            scrutinee,
            arms,
            ty,
        } => {
            // Each `Match` reserves one i32 scratch local for the
            // scrutinee pointer.
            bump_count(&mut out.i32)?;
            walk_count(scrutinee, module, out)?;
            for arm in arms {
                super::optional::coercion_scratch_counts(ty, arm.body.ty(), out)?;
                walk_count(&arm.body, module, out)?;
            }
        }
        IrExpr::ClosureRef { env_struct, .. } => {
            // Each ClosureRef reserves a scratch local for the
            // (funcref, env_ptr) pair's base pointer.
            bump_count(&mut out.i32)?;
            walk_count(env_struct, module, out)?;
        }
        IrExpr::Array { elements, .. } => {
            // Each Array literal reserves two i32 scratch locals — one
            // for the element-buffer base pointer, one for the
            // header pointer.
            bump_count(&mut out.i32)?;
            bump_count(&mut out.i32)?;
            for e in elements {
                walk_count(e, module, out)?;
            }
        }
        IrExpr::For {
            collection, body, ..
        } => {
            // Per-For scratch-local layout is owned by `control` so
            // the reservation here cannot drift from the consumption
            // there. The Range path reserves typed `start` / `end`
            // slots whose width depends on the bound type.
            super::control::for_scratch_counts(collection.ty(), out)?;
            walk_count(collection, module, out)?;
            walk_count(body, module, out)?;
        }

        IrExpr::Literal { value, .. } => {
            // Literal::Nil allocates a tag-only Optional value in
            // linear memory and stashes the base pointer in a fresh
            // i32 scratch slot before storing the tag. Other literal
            // kinds are pure stack pushes and need no scratch.
            if matches!(value, formalang::ast::Literal::Nil) {
                bump_count(&mut out.i32)?;
            }
        }
        IrExpr::Reference { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::Closure { .. }
        | IrExpr::DictLiteral { .. } => {}
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
    return_ty: Option<&ResolvedType>,
    locals: Vec<(u32, ValType)>,
    ctx: &LowerContext<'_>,
) -> Result<Function, LowerError> {
    let mut func = Function::new(locals);
    {
        let sink = &mut func.instructions();
        if let Some(target) = return_ty {
            super::optional::lower_coerced(body, target, sink, ctx)?;
        } else {
            lower_expr(body, sink, ctx)?;
        }
        sink.end();
    }
    Ok(func)
}

fn index_of(i: usize) -> Result<u32, LowerError> {
    u32::try_from(i).map_err(|_| LowerError::NotYetImplemented {
        what: "more than u32::MAX locals in a single function".to_owned(),
    })
}
