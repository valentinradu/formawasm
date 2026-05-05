//! Bridge between the formalang [`IrModule`] and the core-Wasm
//! [`ModuleBuilder`].
//!
//! [`lower_module`] walks the IR module, builds a [`FunctionMap`]
//! ahead of time so recursive calls resolve, then lowers each
//! function body and plugs it into a fresh `ModuleBuilder` before
//! returning the encoded module bytes.

use std::collections::HashMap;

use formalang::ast::Literal;
use formalang::ir::{
    BindingId, FunctionId, ImplId, ImplTarget, IrBlockStatement, IrExpr, IrFunction,
    IrFunctionParam, IrImpl, IrModule, MethodIdx, ResolvedType, StructId, TraitId,
};
use wasm_encoder::ValType;

use crate::ident::kebab_case;
use crate::layout::VTABLE_SLOT_ALIGN;
use crate::lower::{
    ClosureCallContext, FunctionMap, LowerError, MethodMap, VTableContext,
    lower_function_body_in_module,
};
use crate::module::{IMPORT_MODULE_NAME, ModuleBuilder};
use crate::string_pool::{StringPool, StringPoolError};
use crate::types::{TypeMapError, body_result_types, body_value_type};

/// `(BindingId, ValType)` pair recording one function parameter's
/// binding identity alongside its wasm value type.
type ParamBinding = (BindingId, ValType);

/// Hashable encoding of an [`ImplTarget`].
///
/// `ImplTarget` itself doesn't implement `Hash` upstream, so vtable
/// keying flattens it into a `(tag, raw_id)` tuple — `tag = 0` for a
/// struct target, `tag = 1` for an enum target, `tag = 2` for a
/// primitive (extern-impl) target with `raw_id` carrying the
/// primitive's discriminant via `primitive_target_id`.
pub(crate) type ImplTargetKey = (u32, u32);

/// Encode an [`ImplTarget`] into the vtable-key form.
pub(crate) const fn impl_target_key(t: ImplTarget) -> ImplTargetKey {
    match t {
        ImplTarget::Struct(id) => (0, id.0),
        ImplTarget::Enum(id) => (1, id.0),
        ImplTarget::Primitive(p) => (2, primitive_target_id(p)),
    }
}

/// Wasm function indices for every runtime helper backing a
/// prelude `extern impl <Primitive>` method. Grouped into a struct
/// so the dispatch table at [`prelude_helper_index`] can look up by
/// (primitive, method-name) without threading half a dozen `u32`s
/// through every caller.
///
/// Every field today starts with `str_` because the prelude only
/// declares String methods. As `Path` / `Regex` / etc. land, their
/// fields will carry the same `<primitive>_<method>` shape and the
/// shared prefix per primitive is intentional disambiguation, not
/// noise.
#[derive(Copy, Clone, Debug)]
#[expect(
    clippy::struct_field_names,
    reason = "prefix is meaningful: separates string helpers from \
              future path/regex/etc. helpers within the same struct"
)]
struct PreludeHelpers {
    str_len: u32,
    str_is_empty: u32,
    str_byte_at: u32,
    str_slice: u32,
    str_starts_with: u32,
    str_contains: u32,
}

/// Look up the wasm function index of the runtime helper that
/// implements `<primitive>::<method_name>` on the prelude's
/// `extern impl <Primitive>` surface.
///
/// The set is hand-written — one arm per (primitive, name) pair the
/// backend has wired. Calls into prelude methods that lack a runtime
/// helper get caught at lowering time as `UnknownMethod` rather than
/// failing module-compile.
fn prelude_helper_index(
    primitive: formalang::ast::PrimitiveType,
    method_name: &str,
    helpers: PreludeHelpers,
) -> Option<u32> {
    use formalang::ast::PrimitiveType;
    match (primitive, method_name) {
        (PrimitiveType::String, "len") => Some(helpers.str_len),
        (PrimitiveType::String, "is_empty") => Some(helpers.str_is_empty),
        (PrimitiveType::String, "byte_at") => Some(helpers.str_byte_at),
        (PrimitiveType::String, "slice") => Some(helpers.str_slice),
        (PrimitiveType::String, "starts_with") => Some(helpers.str_starts_with),
        (PrimitiveType::String, "contains") => Some(helpers.str_contains),
        _ => None,
    }
}

/// Stable id per [`PrimitiveType`] used in the vtable-key tag = 2
/// slot. The id is purely an internal hash key; it has no wasm-
/// level meaning.
const fn primitive_target_id(p: formalang::ast::PrimitiveType) -> u32 {
    use formalang::ast::PrimitiveType;
    match p {
        PrimitiveType::I32 => 0,
        PrimitiveType::I64 => 1,
        PrimitiveType::F32 => 2,
        PrimitiveType::F64 => 3,
        PrimitiveType::Boolean => 4,
        PrimitiveType::Never => 5,
        PrimitiveType::String => 6,
        PrimitiveType::Path => 7,
        PrimitiveType::Regex => 8,
        // PrimitiveType is `#[non_exhaustive]` upstream — future
        // variants ride this arm with a sentinel id; new primitives
        // should land their own branch when they arrive.
        _ => u32::MAX,
    }
}

/// Errors produced by [`lower_module`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ModuleLowerError {
    /// An expression-level lowering failure surfaced from
    /// [`lower_function_body_in_module`] or one of its callees.
    #[error(transparent)]
    Lower(#[from] LowerError),

    /// A non-extern function reached body emission with `body: None`.
    /// `extern_abi`-bearing functions are declared as imports up-
    /// front in [`lower_module`] and never reach this stage; a
    /// `body: None` here means the IR carries a malformed function.
    #[error("function '{name}' has no body and is not declared `extern`")]
    MissingFunctionBody {
        /// Source-level name of the offending function.
        name: String,
    },

    /// A function parameter is missing its type annotation. Only
    /// `self` parameters legitimately omit the type, and Phase 1a
    /// does not yet emit methods.
    #[error("parameter '{name}' on function '{function}' is missing a type annotation")]
    MissingParamType {
        /// Containing function name.
        function: String,
        /// Parameter name.
        name: String,
    },

    /// A type-mapping failure surfaced while lowering a function
    /// signature.
    #[error(transparent)]
    TypeMap(#[from] TypeMapError),

    /// More than `u32::MAX` functions in a module — a wasm-encoding
    /// limit, not a realistic case for hand-written code.
    #[error("module has more than u32::MAX functions")]
    TooManyFunctions,

    /// String-pool population failed during the pre-walk that
    /// collects string literals — typically because the cumulative
    /// data size would exceed `u32::MAX`.
    #[error(transparent)]
    StringPool(#[from] StringPoolError),

    /// An `impl Trait for Type` references a `TraitId` that the
    /// module's `traits` vector does not contain. Indicates corrupt
    /// IR.
    #[error("TraitId {0:?} is not present in IrModule.traits")]
    UnknownTrait(TraitId),

    /// A trait method declared by `IrTrait.methods` has no matching
    /// entry in an `impl Trait for Type` block. Means the impl is
    /// incomplete; semantic analysis upstream should have rejected it.
    #[error("trait '{trait_name}' method '{method}' is missing in impl for {target:?}")]
    MissingTraitMethod {
        /// Containing trait name.
        trait_name: String,
        /// Source-level method name on the trait.
        method: String,
        /// Target the impl applies to.
        target: ImplTarget,
    },

    /// The static-data segment would exceed `u32::MAX` bytes after
    /// appending vtable bytes. Linear-memory offsets are `u32` so
    /// every vtable byte must fit in that range.
    #[error("static data segment exceeds u32::MAX after appending vtable bytes")]
    VtableDataOverflow,
}

/// Walk `module` and return the encoded core-Wasm bytes.
///
/// Top-level non-extern functions are emitted after the bump-
/// allocator runtime helper, so `FunctionId(i)` maps to wasm index
/// `i + 1` (the helper occupies wasm index 0). Methods declared in
/// `module.impls` follow at wasm indices past the top-level
/// functions; the [`MethodMap`] hides the per-impl indexing from
/// call-site lowerings. [`FunctionMap`] hides the user-offset shift.
///
/// Nested `module.modules` are still not walked.
#[expect(
    clippy::too_many_lines,
    reason = "module-level orchestration sequences string interning, function/method index assignment, closure plumbing, and vtable plumbing — splitting hides the dependency order between them"
)]
#[tracing::instrument(skip(module), fields(
    functions = module.functions.len(),
    impls = module.impls.len(),
    traits = module.traits.len(),
))]
pub fn lower_module(module: &IrModule) -> Result<Vec<u8>, ModuleLowerError> {
    let mut builder = ModuleBuilder::new();

    // Pre-walk every function body and impl method to intern the
    // string literals each one references. The pool seeds the data
    // segment that gets emitted at finish() time, and its lookup
    // table feeds `lower_literal` for `Literal::String`.
    let mut string_pool = StringPool::new();
    collect_string_literals(module, &mut string_pool)?;

    let mut function_map = FunctionMap::new();

    // Imports first — every `extern_abi`-bearing top-level function
    // becomes one core-wasm function import under
    // `IMPORT_MODULE_NAME`. Imports occupy the leading region of the
    // wasm function-index space, so they must be declared before the
    // bump allocator and other runtime helpers (which are local
    // function definitions and consume the indices that follow).
    for (i, f) in module.functions.iter().enumerate() {
        if !f.is_extern() {
            continue;
        }
        let id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let (params, results) = lower_function_signature(f)?;
        let wasm_idx = builder.declare_function_import(
            IMPORT_MODULE_NAME,
            &kebab_case(&f.name),
            &params,
            &results,
        );
        function_map.insert(FunctionId(id_raw), wasm_idx);
    }

    let bump_idx = builder.declare_bump_allocator();
    // The string runtime helpers (__str_eq, __str_concat) are
    // unconditionally declared after the bump allocator so their
    // wasm indices are known before any function body is emitted.
    // Bodies that never touch strings pay a small dead-code cost.
    let str_eq_idx = builder.declare_str_eq();
    let str_concat_idx = builder.declare_str_concat();
    // Pre-declare every prelude-method helper so the impl walk
    // below resolves MethodMap entries without lazy plumbing. Each
    // helper is lazy in the builder — declaring it here is cheap
    // when the program never calls into the corresponding method,
    // since `declare_function_with_body` only adds a few bytes per
    // helper to the encoded module.
    let helpers = PreludeHelpers {
        str_len: builder.declare_str_len(),
        str_is_empty: builder.declare_str_is_empty(),
        str_byte_at: builder.declare_str_byte_at(),
        str_slice: builder.declare_str_slice(),
        str_starts_with: builder.declare_str_starts_with(),
        str_contains: builder.declare_str_contains(),
    };
    // `cabi_realloc` is exported so the component runtime can
    // allocate buffers in our linear memory when lowering `string`
    // / `list<T>` arguments. Always declared so any public function
    // that takes one of those types lands an export-ready module.
    let cabi_realloc_idx = builder.declare_cabi_realloc();
    let user_offset = cabi_realloc_idx
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;

    // Locally-defined user functions follow the runtime helpers.
    // Walk `module.functions` again, this time assigning a wasm
    // index only to non-extern entries; their indices ladder up
    // from `user_offset` in source-declaration order.
    //
    // Public functions whose canonical-ABI signature differs from
    // the internal one get a thin wrapper emitted alongside the
    // inner function (see `emit_canonical_abi_wrapper`). The
    // wrapper claims the next available wasm function slot, so this
    // pre-assignment must reserve TWO contiguous slots for those
    // functions: the inner gets the first, the wrapper gets the
    // second. Without that reservation, every subsequent user-
    // function index in `function_map` would point at the wrong
    // function once a wrapper is interleaved.
    let mut local_counter: u32 = 0;
    for (i, f) in module.functions.iter().enumerate() {
        if f.is_extern() {
            continue;
        }
        let id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let wasm_idx = user_offset
            .checked_add(local_counter)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        function_map.insert(FunctionId(id_raw), wasm_idx);
        local_counter = local_counter
            .checked_add(1)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        if needs_canonical_abi_wrapper(f, module) {
            local_counter = local_counter
                .checked_add(1)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
        }
    }

    // Pre-assign wasm function indices for every method in every
    // impl, so MethodCall lowering can resolve targets without
    // re-walking the impls. The methods region begins right after
    // the locally-defined user functions; only non-extern user
    // functions take indices in that region (extern functions live
    // in the import region ahead of the runtime helpers).
    let methods_offset = user_offset
        .checked_add(local_counter)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    let mut method_map = MethodMap::new();
    let mut method_counter: u32 = 0;
    for (i, imp) in module.impls.iter().enumerate() {
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        if imp.is_extern {
            // `extern impl <Primitive>` blocks (the prelude's
            // String surface) get a per-method runtime-helper
            // index, not a fresh method-region slot. Bodies are
            // already in the module via `declare_str_*`; the
            // backend just needs MethodMap entries pointing at
            // them.
            if let ImplTarget::Primitive(p) = imp.target {
                for (j, m) in imp.functions.iter().enumerate() {
                    let m_idx_raw =
                        u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
                    // Silently skip prelude methods we don't have a
                    // runtime helper for yet. Calls into them
                    // surface as `UnknownMethod` at lowering time
                    // (which is a clearer user-facing error than
                    // failing the whole module-compile because the
                    // prelude declares more methods than we
                    // implement). As helpers land, each prelude
                    // method's `prelude_helper_index` arm activates
                    // it.
                    if let Some(helper_idx) = prelude_helper_index(p, &m.name, helpers) {
                        method_map.insert((ImplId(impl_id_raw), MethodIdx(m_idx_raw)), helper_idx);
                    }
                }
            }
            continue;
        }
        for (j, _) in imp.functions.iter().enumerate() {
            let m_idx_raw = u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let wasm_idx = methods_offset
                .checked_add(method_counter)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            method_map.insert((ImplId(impl_id_raw), MethodIdx(m_idx_raw)), wasm_idx);
            method_counter = method_counter
                .checked_add(1)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
        }
    }

    let closure_plumbing = build_closure_plumbing(&mut builder, module, &function_map)?;
    let closure_ctx = closure_plumbing.as_ref().map(|p| ClosureCallContext {
        table_idx: p.table_idx,
        funcref_indices: &p.funcref_indices,
        type_indices: &p.type_indices,
    });

    let string_data_len = u32::try_from(string_pool.data().len())
        .map_err(|_| ModuleLowerError::VtableDataOverflow)?;
    let vtable_plumbing =
        build_vtable_plumbing(&mut builder, module, &method_map, string_data_len)?;
    let vtable_ctx = vtable_plumbing.as_ref().map(|p| VTableContext {
        table_idx: p.table_idx,
        vtable_offsets: &p.vtable_offsets,
        call_type_indices: &p.call_type_indices,
    });

    for f in &module.functions {
        if f.is_extern() {
            continue;
        }
        emit_function(
            f,
            &mut builder,
            &function_map,
            &method_map,
            module,
            bump_idx,
            None,
            closure_ctx.as_ref(),
            vtable_ctx.as_ref(),
            string_pool.lookup_map(),
            str_eq_idx,
            str_concat_idx,
        )?;
    }
    for (i, imp) in module.impls.iter().enumerate() {
        if imp.is_extern {
            continue;
        }
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        emit_impl(
            imp,
            ImplId(impl_id_raw),
            &mut builder,
            &function_map,
            &method_map,
            module,
            bump_idx,
            closure_ctx.as_ref(),
            vtable_ctx.as_ref(),
            string_pool.lookup_map(),
            str_eq_idx,
            str_concat_idx,
        )?;
    }

    let mut static_data = string_pool.data().to_vec();
    if let Some(p) = vtable_plumbing.as_ref() {
        // Pad string-data up to the vtable region's start offset, then
        // append the vtable bytes. The base was computed against the
        // string_data_len snapshot above, so the padding here mirrors
        // the alignment math `build_vtable_plumbing` already did.
        while static_data.len() < p.vtable_data_base as usize {
            static_data.push(0);
        }
        static_data.extend_from_slice(&p.vtable_data);
    }
    builder.set_static_data(static_data);
    Ok(builder.finish())
}

/// Walk every function body / impl method body in `module` and intern
/// each `Literal::String` into `pool`.
fn collect_string_literals(
    module: &IrModule,
    pool: &mut StringPool,
) -> Result<(), ModuleLowerError> {
    for f in &module.functions {
        if let Some(body) = &f.body {
            walk_for_strings(body, pool)?;
        }
    }
    for imp in &module.impls {
        if imp.is_extern {
            continue;
        }
        for f in &imp.functions {
            if let Some(body) = &f.body {
                walk_for_strings(body, pool)?;
            }
        }
    }
    Ok(())
}

fn walk_block_statement_for_strings(
    stmt: &IrBlockStatement,
    pool: &mut StringPool,
) -> Result<(), ModuleLowerError> {
    match stmt {
        IrBlockStatement::Let { value, .. } | IrBlockStatement::Expr(value) => {
            walk_for_strings(value, pool)
        }
        IrBlockStatement::Assign { target, value, .. } => {
            walk_for_strings(target, pool)?;
            walk_for_strings(value, pool)
        }
    }
}

fn walk_for_strings(expr: &IrExpr, pool: &mut StringPool) -> Result<(), ModuleLowerError> {
    match expr {
        IrExpr::Literal { value, .. } => {
            // String / Path / Regex all share the {ptr, len} header
            // layout, so they all get interned into the same pool.
            // Regex flags ride in the type identity, not the runtime
            // value, so only the pattern text is interned here.
            match value {
                Literal::String(text) | Literal::Path(text) => {
                    pool.intern(text)?;
                }
                Literal::Regex { pattern, .. } => {
                    pool.intern(pattern)?;
                }
                Literal::Number(_) | Literal::Boolean(_) | Literal::Nil => {}
                _ => {}
            }
        }
        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                walk_block_statement_for_strings(stmt, pool)?;
            }
            walk_for_strings(result, pool)?;
        }
        IrExpr::BinaryOp { left, right, .. } => {
            walk_for_strings(left, pool)?;
            walk_for_strings(right, pool)?;
        }
        IrExpr::UnaryOp { operand, .. } => walk_for_strings(operand, pool)?,
        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            walk_for_strings(condition, pool)?;
            walk_for_strings(then_branch, pool)?;
            if let Some(else_branch) = else_branch {
                walk_for_strings(else_branch, pool)?;
            }
        }
        IrExpr::FunctionCall { args, .. } | IrExpr::CallClosure { args, .. } => {
            for (_, arg) in args {
                walk_for_strings(arg, pool)?;
            }
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            walk_for_strings(receiver, pool)?;
            for (_, arg) in args {
                walk_for_strings(arg, pool)?;
            }
        }
        IrExpr::FieldAccess { object, .. } => walk_for_strings(object, pool)?,
        IrExpr::DictAccess { dict, key, .. } => {
            walk_for_strings(dict, pool)?;
            walk_for_strings(key, pool)?;
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            walk_for_strings(scrutinee, pool)?;
            for arm in arms {
                walk_for_strings(&arm.body, pool)?;
            }
        }
        IrExpr::For {
            collection, body, ..
        } => {
            walk_for_strings(collection, pool)?;
            walk_for_strings(body, pool)?;
        }
        IrExpr::ClosureRef { env_struct, .. } => walk_for_strings(env_struct, pool)?,
        IrExpr::StructInst { fields, .. } | IrExpr::EnumInst { fields, .. } => {
            for (_, _, value) in fields {
                walk_for_strings(value, pool)?;
            }
        }
        IrExpr::Tuple { fields, .. } => {
            for (_, value) in fields {
                walk_for_strings(value, pool)?;
            }
        }
        IrExpr::Array { elements, .. } => {
            for e in elements {
                walk_for_strings(e, pool)?;
            }
        }
        IrExpr::DictLiteral { entries, .. } => {
            for (k, v) in entries {
                walk_for_strings(k, pool)?;
                walk_for_strings(v, pool)?;
            }
        }
        IrExpr::Closure { body, .. } => walk_for_strings(body, pool)?,
        IrExpr::Reference { .. } | IrExpr::LetRef { .. } | IrExpr::SelfFieldRef { .. } => {}
    }
    Ok(())
}

/// Owned plumbing for indirect closure invocation.
///
/// `lower_module` builds this once after function-index resolution
/// and before any function bodies emit instructions. The returned
/// owned maps are the storage backing the
/// [`ClosureCallContext`] borrows that flow into per-function
/// lowering — keeping the maps owned here avoids dangling-borrow
/// problems with the `ModuleBuilder` field accesses that come later.
struct ClosurePlumbing {
    table_idx: u32,
    funcref_indices: HashMap<FunctionId, u32>,
    type_indices: HashMap<ResolvedType, u32>,
}

/// Walk `module` and:
///
/// 1. Find every closure-callable function (named `__closure*` per
///    the closure-conversion convention) and register it in a fresh
///    funcref table.
/// 2. Find every `IrExpr::CallClosure` site and pre-register a wasm
///    type signature for each unique closure type seen, with an i32
///    `env_ptr` prepended to the declared parameters.
/// 3. Return the per-module plumbing the per-function lowerings need
///    to emit `call_indirect`.
///
/// Returns `None` when the module has no closure-callable functions
/// — the caller leaves the `ClosureCallContext` unset, and a stray
/// `IrExpr::CallClosure` falls through to a typed
/// [`LowerError::MissingContext`] downstream.
#[tracing::instrument(skip_all)]
fn build_closure_plumbing(
    builder: &mut ModuleBuilder,
    module: &IrModule,
    function_map: &FunctionMap,
) -> Result<Option<ClosurePlumbing>, ModuleLowerError> {
    let closure_funcs: Vec<(FunctionId, u32)> = module
        .functions
        .iter()
        .enumerate()
        .filter(|(_, f)| f.name.starts_with("__closure"))
        .map(|(i, _)| -> Result<(FunctionId, u32), ModuleLowerError> {
            let id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let id = FunctionId(id_raw);
            let wasm_idx = function_map
                .get(id)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            Ok((id, wasm_idx))
        })
        .collect::<Result<_, _>>()?;

    if closure_funcs.is_empty() {
        return Ok(None);
    }

    let num_closures =
        u32::try_from(closure_funcs.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
    let table_idx = builder.declare_closure_table(num_closures);
    let wasm_func_indices: Vec<u32> = closure_funcs.iter().map(|(_, w)| *w).collect();
    builder.populate_closure_table(&wasm_func_indices);

    let mut funcref_indices = HashMap::new();
    for (slot, (id, _)) in closure_funcs.into_iter().enumerate() {
        let slot_u32 = u32::try_from(slot).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        funcref_indices.insert(id, slot_u32);
    }

    let mut type_indices: HashMap<ResolvedType, u32> = HashMap::new();
    for f in &module.functions {
        if let Some(body) = &f.body {
            collect_call_closure_types(body, builder, &mut type_indices)?;
        }
    }
    for imp in &module.impls {
        if imp.is_extern {
            continue;
        }
        for f in &imp.functions {
            if let Some(body) = &f.body {
                collect_call_closure_types(body, builder, &mut type_indices)?;
            }
        }
    }

    Ok(Some(ClosurePlumbing {
        table_idx,
        funcref_indices,
        type_indices,
    }))
}

/// Walk `expr`, register a wasm type for every unique closure type
/// seen at an `IrExpr::CallClosure` site, and update `out` with the
/// `(closure_ty, type_idx)` pair.
fn collect_call_closure_types(
    expr: &IrExpr,
    builder: &mut ModuleBuilder,
    out: &mut HashMap<ResolvedType, u32>,
) -> Result<(), ModuleLowerError> {
    if let IrExpr::CallClosure { closure, .. } = expr {
        let ty = closure.ty().clone();
        if let std::collections::hash_map::Entry::Vacant(slot) = out.entry(ty.clone()) {
            let type_idx = register_closure_call_type(&ty, builder)?;
            slot.insert(type_idx);
        }
    }
    walk_children(expr, &mut |child| {
        collect_call_closure_types(child, builder, out)
    })
}

/// Register a wasm `func` type for `closure_ty`'s `call_indirect`
/// signature. Prepends one i32 (`env_ptr`) to the closure's declared
/// parameters and uses the body-side wasm valtype for each
/// (aggregates lower as i32 pointers).
fn register_closure_call_type(
    closure_ty: &ResolvedType,
    builder: &mut ModuleBuilder,
) -> Result<u32, ModuleLowerError> {
    let ResolvedType::Closure {
        param_tys,
        return_ty,
    } = closure_ty
    else {
        return Err(ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("call_indirect type for non-Closure {closure_ty:?}"),
        }));
    };
    let mut params: Vec<ValType> = Vec::with_capacity(param_tys.len().saturating_add(1));
    params.push(ValType::I32); // env_ptr
    for (_, ty) in param_tys {
        let vt = body_value_type(ty)?.ok_or_else(|| {
            ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                kind: format!("closure parameter of type {ty:?} (Never)"),
            })
        })?;
        params.push(vt);
    }
    let results: Vec<ValType> = body_value_type(return_ty)?
        .map(|vt| vec![vt])
        .unwrap_or_default();
    Ok(builder.declare_type(&params, &results))
}

/// Owned plumbing for virtual trait-method dispatch.
///
/// Built once after the function/method index space has been laid
/// down, before any function body is lowered. The owned maps back
/// the [`VTableContext`] borrows that flow into per-function
/// lowering — keeping them owned at this scope avoids dangling
/// borrows against the `ModuleBuilder` field accesses that come
/// later.
struct VTablePlumbing {
    table_idx: u32,
    vtable_offsets: HashMap<(TraitId, ImplTargetKey), u32>,
    call_type_indices: HashMap<(TraitId, MethodIdx), u32>,
    /// Bytes to append to the static-data segment. Each impl's
    /// vtable is `methods * 4` bytes of i32 funcref-table indices,
    /// laid out back-to-back at the offsets recorded in
    /// [`Self::vtable_offsets`].
    vtable_data: Vec<u8>,
    /// Absolute byte offset of [`Self::vtable_data`] within the
    /// final static-data segment. Equals
    /// `align_up(string_data.len(), VTABLE_SLOT_ALIGN)`.
    vtable_data_base: u32,
}

/// Walk `module` and:
///
/// 1. Assign each method on every `impl Trait for Type` (non-extern)
///    a slot inside a fresh funcref table.
/// 2. Build per-`(trait_id, target)` vtable bytes — for each method
///    declared on the trait, look up the matching impl method by
///    name and append its funcref-table slot index as an i32.
/// 3. Pre-register a wasm `func` type per trait method so virtual
///    call sites can `call_indirect` against it.
///
/// Returns `None` when the module has no trait impls — the caller
/// leaves the [`VTableContext`] unset, and any stray
/// [`formalang::ir::DispatchKind::Virtual`] call site falls through
/// to a typed [`LowerError::MissingContext`] downstream.
#[tracing::instrument(skip_all)]
fn build_vtable_plumbing(
    builder: &mut ModuleBuilder,
    module: &IrModule,
    method_map: &MethodMap,
    string_data_len: u32,
) -> Result<Option<VTablePlumbing>, ModuleLowerError> {
    let mut method_funcref_indices: HashMap<(ImplId, MethodIdx), u32> = HashMap::new();
    let mut method_table_func_indices: Vec<u32> = Vec::new();
    let mut next_slot: u32 = 0;
    for (i, imp) in module.impls.iter().enumerate() {
        if imp.is_extern || imp.trait_ref.is_none() {
            continue;
        }
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        for (j, _) in imp.functions.iter().enumerate() {
            let method_idx_raw =
                u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let key = (ImplId(impl_id_raw), MethodIdx(method_idx_raw));
            let wasm_func_idx = method_map
                .get(key)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            method_funcref_indices.insert(key, next_slot);
            method_table_func_indices.push(wasm_func_idx);
            next_slot = next_slot
                .checked_add(1)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
        }
    }

    if next_slot == 0 {
        return Ok(None);
    }

    let table_idx = builder.declare_method_table(next_slot);
    builder.populate_method_table(&method_table_func_indices);

    // Compute the absolute byte offset where the first vtable lives.
    // Vtables come after the string-pool data inside the same static
    // data segment; each cell is 4-aligned, so we round the string
    // segment's length up to that boundary.
    let vtable_data_base = align_up_u32(string_data_len, VTABLE_SLOT_ALIGN)
        .ok_or(ModuleLowerError::VtableDataOverflow)?;

    let mut vtable_offsets: HashMap<(TraitId, ImplTargetKey), u32> = HashMap::new();
    let mut vtable_data: Vec<u8> = Vec::new();
    for (i, imp) in module.impls.iter().enumerate() {
        if imp.is_extern {
            continue;
        }
        let Some(trait_ref) = imp.trait_ref.as_ref() else {
            continue;
        };
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let trait_id = trait_ref.trait_id;
        let trait_decl = module
            .traits
            .get(trait_id.0 as usize)
            .ok_or(ModuleLowerError::UnknownTrait(trait_id))?;

        let local_offset =
            u32::try_from(vtable_data.len()).map_err(|_| ModuleLowerError::VtableDataOverflow)?;
        let absolute_offset = vtable_data_base
            .checked_add(local_offset)
            .ok_or(ModuleLowerError::VtableDataOverflow)?;
        vtable_offsets.insert((trait_id, impl_target_key(imp.target)), absolute_offset);

        for trait_method in &trait_decl.methods {
            let (method_idx_in_impl, _) = imp
                .functions
                .iter()
                .enumerate()
                .find(|(_, f)| f.name == trait_method.name)
                .ok_or_else(|| ModuleLowerError::MissingTraitMethod {
                    trait_name: trait_decl.name.clone(),
                    method: trait_method.name.clone(),
                    target: imp.target,
                })?;
            let m_idx_raw = u32::try_from(method_idx_in_impl)
                .map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let key = (ImplId(impl_id_raw), MethodIdx(m_idx_raw));
            let funcref_slot = method_funcref_indices
                .get(&key)
                .copied()
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            vtable_data.extend_from_slice(&funcref_slot.to_le_bytes());
        }
    }

    let mut call_type_indices: HashMap<(TraitId, MethodIdx), u32> = HashMap::new();
    for (i, t) in module.traits.iter().enumerate() {
        let trait_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        for (j, sig) in t.methods.iter().enumerate() {
            let method_idx_raw =
                u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let type_idx = register_trait_method_call_type(sig, builder)?;
            call_type_indices.insert((TraitId(trait_id_raw), MethodIdx(method_idx_raw)), type_idx);
        }
    }

    Ok(Some(VTablePlumbing {
        table_idx,
        vtable_offsets,
        call_type_indices,
        vtable_data,
        vtable_data_base,
    }))
}

/// Register a wasm `func` type for `sig`'s `call_indirect` signature.
///
/// The first parameter is always an i32 receiver pointer (the trait
/// method's implicit `self`). Subsequent parameters use each
/// declared param's body-side wasm value type. The return type
/// follows [`body_value_type`] — `None` (Never / unit) means a
/// zero-result type.
fn register_trait_method_call_type(
    sig: &formalang::ir::IrFunctionSig,
    builder: &mut ModuleBuilder,
) -> Result<u32, ModuleLowerError> {
    let mut params: Vec<ValType> = Vec::with_capacity(sig.params.len());
    let mut iter = sig.params.iter();
    let first = iter.next();
    if let Some(p) = first {
        if p.name == "self" {
            params.push(ValType::I32);
        } else {
            // Trait method without `self` — treat the leading param
            // like a regular one. Phase 3's milestone exclusively
            // exercises self-bearing methods, but the code path stays
            // total over the IR shape.
            let ty =
                p.ty.as_ref()
                    .ok_or_else(|| ModuleLowerError::MissingParamType {
                        function: sig.name.clone(),
                        name: p.name.clone(),
                    })?;
            let vt = body_value_type(ty)?.ok_or_else(|| {
                ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                    kind: format!("trait-method parameter of type {ty:?} (Never)"),
                })
            })?;
            params.push(vt);
        }
    }
    for p in iter {
        let ty =
            p.ty.as_ref()
                .ok_or_else(|| ModuleLowerError::MissingParamType {
                    function: sig.name.clone(),
                    name: p.name.clone(),
                })?;
        let vt = body_value_type(ty)?.ok_or_else(|| {
            ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                kind: format!("trait-method parameter of type {ty:?} (Never)"),
            })
        })?;
        params.push(vt);
    }
    let results = body_result_types(sig.return_type.as_ref())?;
    Ok(builder.declare_type(&params, &results))
}

/// Round `value` up to the next multiple of `align` (must be a
/// power of two). Returns `None` on overflow.
fn align_up_u32(value: u32, align: u32) -> Option<u32> {
    let mask = align.checked_sub(1)?;
    let added = value.checked_add(mask)?;
    Some(added & !mask)
}

/// Walk every child sub-expression of `expr` and apply `visit` to
/// each. Used by [`collect_call_closure_types`] so the per-closure
/// pre-registration walk doesn't need its own exhaustive variant
/// match.
#[expect(
    clippy::too_many_lines,
    reason = "exhaustive walk over every IrExpr variant — extracting arms hides which variants are leaves vs. recursive"
)]
fn walk_children<F>(expr: &IrExpr, visit: &mut F) -> Result<(), ModuleLowerError>
where
    F: FnMut(&IrExpr) -> Result<(), ModuleLowerError>,
{
    match expr {
        IrExpr::Literal { .. }
        | IrExpr::Reference { .. }
        | IrExpr::SelfFieldRef { .. }
        | IrExpr::LetRef { .. }
        | IrExpr::Closure { .. } => Ok(()),
        IrExpr::StructInst { fields, .. } | IrExpr::EnumInst { fields, .. } => {
            for (_, _, e) in fields {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::Tuple { fields, .. } => {
            for (_, e) in fields {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::Array { elements, .. } => {
            for e in elements {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::FieldAccess { object, .. } => visit(object),
        IrExpr::BinaryOp { left, right, .. } => {
            visit(left)?;
            visit(right)
        }
        IrExpr::UnaryOp { operand, .. } => visit(operand),
        IrExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit(condition)?;
            visit(then_branch)?;
            if let Some(e) = else_branch {
                visit(e)?;
            }
            Ok(())
        }
        IrExpr::For {
            collection, body, ..
        } => {
            visit(collection)?;
            visit(body)
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            visit(scrutinee)?;
            for arm in arms {
                visit(&arm.body)?;
            }
            Ok(())
        }
        IrExpr::FunctionCall { args, .. } => {
            for (_, a) in args {
                visit(a)?;
            }
            Ok(())
        }
        IrExpr::CallClosure { closure, args, .. } => {
            visit(closure)?;
            for (_, a) in args {
                visit(a)?;
            }
            Ok(())
        }
        IrExpr::MethodCall { receiver, args, .. } => {
            visit(receiver)?;
            for (_, a) in args {
                visit(a)?;
            }
            Ok(())
        }
        IrExpr::ClosureRef { env_struct, .. } => visit(env_struct),
        IrExpr::DictLiteral { entries, .. } => {
            for (k, v) in entries {
                visit(k)?;
                visit(v)?;
            }
            Ok(())
        }
        IrExpr::DictAccess { dict, key, .. } => {
            visit(dict)?;
            visit(key)
        }
        IrExpr::Block {
            statements, result, ..
        } => {
            for stmt in statements {
                match stmt {
                    IrBlockStatement::Let { value, .. } => visit(value)?,
                    IrBlockStatement::Assign { target, value, .. } => {
                        visit(target)?;
                        visit(value)?;
                    }
                    IrBlockStatement::Expr(e) => visit(e)?,
                }
            }
            visit(result)
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "module-aware impl emission already wires every method/module/closure dependency through; bundling hides which inputs the called helpers actually read"
)]
fn emit_impl(
    imp: &IrImpl,
    _impl_id: ImplId,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
    method_map: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    closure_ctx: Option<&ClosureCallContext<'_>>,
    vtable_ctx: Option<&VTableContext<'_>>,
    string_pool: &HashMap<String, u32>,
    str_eq: u32,
    str_concat: u32,
) -> Result<(), ModuleLowerError> {
    let self_struct_id = match imp.target {
        ImplTarget::Struct(id) => Some(id),
        // Enum methods land later; tests cover struct methods.
        // Primitive impls (extern impl <String> etc.) carry no
        // self-struct context — their bodies are extern stubs the
        // backend resolves to runtime helpers per-method.
        ImplTarget::Enum(_) | ImplTarget::Primitive(_) => None,
    };
    for f in &imp.functions {
        emit_function(
            f,
            builder,
            function_map,
            method_map,
            module,
            bump_allocator,
            self_struct_id,
            closure_ctx,
            vtable_ctx,
            string_pool,
            str_eq,
            str_concat,
        )?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "module-aware function emission needs every map and closure context the body lowering reads"
)]
fn emit_function(
    f: &IrFunction,
    builder: &mut ModuleBuilder,
    function_map: &FunctionMap,
    method_map: &MethodMap,
    module: &IrModule,
    bump_allocator: u32,
    impl_self_struct_id: Option<StructId>,
    closure_ctx: Option<&ClosureCallContext<'_>>,
    vtable_ctx: Option<&VTableContext<'_>>,
    string_pool: &HashMap<String, u32>,
    str_eq: u32,
    str_concat: u32,
) -> Result<(), ModuleLowerError> {
    let body_expr = f
        .body
        .as_ref()
        .ok_or_else(|| ModuleLowerError::MissingFunctionBody {
            name: f.name.clone(),
        })?;

    let (param_valtypes, param_bindings) = lower_params(f)?;
    let result_valtypes = body_result_types(f.return_type.as_ref())?;

    let self_struct_id = impl_self_struct_id.or_else(|| detect_self_struct(f));
    let body = lower_function_body_in_module(
        body_expr,
        f.return_type.as_ref(),
        &param_bindings,
        function_map,
        method_map,
        module,
        bump_allocator,
        self_struct_id,
        closure_ctx,
        vtable_ctx,
        string_pool,
        str_eq,
        str_concat,
    )?;
    let wasm_idx = builder.declare_function_with_body(&param_valtypes, &result_valtypes, &body);
    // Record the source-level identifier in the `name` custom
    // section so disassembly shows it. Methods are qualified with
    // their containing struct (e.g. `counter::apply`) so
    // collisions across impls don't merge into a single name.
    let pretty_name = impl_self_struct_id
        .and_then(|id| module.structs.get(id.0 as usize))
        .map_or_else(
            || kebab_case(&f.name),
            |struct_def| format!("{}::{}", kebab_case(&struct_def.name), kebab_case(&f.name)),
        );
    builder.set_function_name(wasm_idx, &pretty_name);
    // Phase 1a: every non-extern top-level function is exported by
    // its source-level name. Survey-driven export filtering lands
    // alongside WIT generation in the next mc. Impl methods do NOT
    // get exported — name collisions between different impls would
    // otherwise produce a malformed module.
    if impl_self_struct_id.is_none()
        && !f.name.starts_with("__")
        && function_signature_is_wit_expressible(f, module)
    {
        // Public functions whose signatures carry types whose
        // canonical-ABI lowering differs from our internal pointer
        // convention need a thin trampoline: the trampoline matches
        // the canonical-ABI shape the host sees, while the inner
        // function keeps using the internal header-pointer
        // convention so intra-module callers don't need to change.
        //
        // The two `__`-prefix and "WIT-expressible signature" gates
        // mirror `survey::survey` and `wit::emit_wit`'s filtering: a
        // function the WIT side won't surface must not appear in
        // the wasm export section either, or wit-component fails to
        // classify the export against the world.
        let export_idx = if needs_canonical_abi_wrapper(f, module) {
            emit_canonical_abi_wrapper(f, wasm_idx, builder, module)?
        } else {
            wasm_idx
        };
        builder.export_function(&kebab_case(&f.name), export_idx);
    }
    Ok(())
}

/// True iff every type in `f`'s signature can be expressed in WIT
/// (i.e. the function genuinely crosses the public boundary). Used
/// to decide whether to emit a wasm export for `f` — without a
/// `visibility` field on `IrFunction` the only sound reading is
/// "private if its signature can't cross". Mirrors the filter in
/// `wit::emit_wit` so the two sides agree on which functions
/// surface.
fn function_signature_is_wit_expressible(f: &IrFunction, module: &IrModule) -> bool {
    use crate::wit::resolved_wit_type_check;
    for p in &f.params {
        let Some(ty) = p.ty.as_ref() else {
            return false;
        };
        if resolved_wit_type_check(ty, module).is_err() {
            return false;
        }
    }
    if let Some(ret) = f.return_type.as_ref()
        && resolved_wit_type_check(ret, module).is_err()
    {
        return false;
    }
    true
}

/// Whether `f`'s public signature lowers to a different core-wasm
/// shape than its internal one.
///
/// Today this is exactly the case where any `String` / `Path` /
/// `Regex` / `list<T>` reaches the function boundary as a parameter.
/// Returns of those types match the internal i32-pointer return
/// directly because the canonical ABI's "return area pointer"
/// convention coincides with our header layout.
fn needs_canonical_abi_wrapper(f: &IrFunction, module: &IrModule) -> bool {
    f.params
        .iter()
        .any(|p| p.ty.as_ref().is_some_and(|ty| param_needs_split(ty, module)))
}

/// Whether a parameter type lowers to multiple core-wasm i32 values
/// at the canonical-ABI boundary. `string` / `Path` / `Regex` / list
/// pass as `(ptr, len)`; everything else stays as a single value.
fn param_needs_split(ty: &ResolvedType, module: &IrModule) -> bool {
    if matches!(
        ty,
        ResolvedType::Primitive(
            formalang::ast::PrimitiveType::String
                | formalang::ast::PrimitiveType::Path
                | formalang::ast::PrimitiveType::Regex
        )
    ) {
        return true;
    }
    matches!(crate::compound::Compound::of(ty, module), crate::compound::Compound::Array(_))
}

/// Build a thin trampoline matching the canonical-ABI signature of
/// `f` and forwarding to the inner function at `inner_idx`.
///
/// For each parameter that lifts to `(ptr, len)` at the boundary
/// (`String` and friends, `list<T>`), the wrapper accepts two i32s
/// directly, allocates an 8-byte header in linear memory, stores
/// `(ptr, len)` into it, and pushes the header pointer in place of
/// the original single-i32 param. Other parameters pass through
/// unchanged.
///
/// Returns the wasm function index of the wrapper.
fn emit_canonical_abi_wrapper(
    f: &IrFunction,
    inner_idx: u32,
    builder: &mut ModuleBuilder,
    module: &IrModule,
) -> Result<u32, ModuleLowerError> {
    use crate::layout::{STRING_HEADER_SIZE, STRING_LEN_OFFSET, STRING_PTR_OFFSET};
    use crate::module::MEMORY_INDEX;
    use wasm_encoder::{Function, MemArg};

    // Build the canonical-ABI parameter list and remember each split
    // param's (ptr_index, len_index) so the wrapper body can read
    // both back when assembling the header.
    let mut param_valtypes: Vec<ValType> = Vec::with_capacity(f.params.len());
    let mut split_params: Vec<(u32, u32)> = Vec::new();
    let mut single_params: Vec<u32> = Vec::new();

    for p in &f.params {
        let ty =
            p.ty.as_ref()
                .ok_or_else(|| ModuleLowerError::MissingParamType {
                    function: f.name.clone(),
                    name: p.name.clone(),
                })?;
        if param_needs_split(ty, module) {
            let ptr_idx = u32::try_from(param_valtypes.len())
                .map_err(|_| ModuleLowerError::TooManyFunctions)?;
            param_valtypes.push(ValType::I32);
            param_valtypes.push(ValType::I32);
            let len_idx = ptr_idx.saturating_add(1);
            split_params.push((ptr_idx, len_idx));
        } else {
            let single_idx = u32::try_from(param_valtypes.len())
                .map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let vt = body_value_type(ty)?.ok_or_else(|| {
                ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                    kind: "Never-typed parameter on public function".to_owned(),
                })
            })?;
            param_valtypes.push(vt);
            single_params.push(single_idx);
        }
    }

    let result_valtypes = body_result_types(f.return_type.as_ref())?;

    // The wrapper needs one i32 scratch local per split param to
    // hold the freshly-allocated header pointer between the
    // allocator call and the forwarded `call` to the inner.
    let scratch_count =
        u32::try_from(split_params.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
    let scratch_base =
        u32::try_from(param_valtypes.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
    let alloc_idx = builder.declare_bump_allocator();

    let mut body = Function::new(if scratch_count > 0 {
        vec![(scratch_count, ValType::I32)]
    } else {
        Vec::new()
    });
    let header_size_signed = i32::try_from(STRING_HEADER_SIZE).unwrap_or(8);
    let mem_arg = |offset: u64| MemArg {
        offset,
        align: 2, // log2(4)
        memory_index: MEMORY_INDEX,
    };
    {
        let mut i = body.instructions();

        // For each split param: alloc 8 bytes, store ptr/len, stash
        // the header pointer in the wrapper's scratch local.
        for (slot, (ptr_idx, len_idx)) in split_params.iter().enumerate() {
            let scratch_idx = scratch_base.saturating_add(
                u32::try_from(slot).map_err(|_| ModuleLowerError::TooManyFunctions)?,
            );
            i.i32_const(header_size_signed)
                .call(alloc_idx)
                .local_set(scratch_idx);
            i.local_get(scratch_idx)
                .local_get(*ptr_idx)
                .i32_store(mem_arg(u64::from(STRING_PTR_OFFSET)));
            i.local_get(scratch_idx)
                .local_get(*len_idx)
                .i32_store(mem_arg(u64::from(STRING_LEN_OFFSET)));
        }

        // Push every parameter onto the stack in declaration order:
        // each split param contributes its scratch (header pointer);
        // each single param contributes its raw local. Both
        // iterators were sized in the loop above to exactly match
        // each `f.params` entry's classification, so the index
        // arithmetic below cannot overflow.
        let mut split_iter = (0u32..).map(|s| scratch_base.saturating_add(s));
        let mut single_iter = single_params.iter().copied();
        for p in &f.params {
            // `param_needs_split` is total over `Option<&ResolvedType>`
            // when treating `None` as not-split (matches the missing-
            // type path the loop above flagged as MissingParamType).
            let split = p.ty.as_ref().is_some_and(|ty| param_needs_split(ty, module));
            if split {
                let scratch_idx = split_iter.next().unwrap_or(scratch_base);
                i.local_get(scratch_idx);
            } else {
                let single_idx = single_iter.next().unwrap_or(0);
                i.local_get(single_idx);
            }
        }

        i.call(inner_idx).end();
    }

    let wrapper_idx = builder.declare_function_with_body(&param_valtypes, &result_valtypes, &body);
    builder.set_function_name(
        wrapper_idx,
        &format!("{}::cabi-wrapper", kebab_case(&f.name)),
    );
    Ok(wrapper_idx)
}

/// Identify the enclosing impl's struct id when `f` is a method.
///
/// Phase 1b mc7 detects methods purely by shape: the first parameter
/// is named `self` and has a `ResolvedType::Struct(_)` type. Once impl
/// walking lands the right answer will come from the surrounding
/// `IrImpl`, but for now this is enough to lower `SelfFieldRef`
/// inside hand-built `module.functions` test fixtures.
fn detect_self_struct(f: &IrFunction) -> Option<StructId> {
    let first = f.params.first()?;
    if first.name != "self" {
        return None;
    }
    match first.ty.as_ref()? {
        ResolvedType::Struct(id) => Some(*id),
        ResolvedType::Primitive(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => None,
    }
}

/// Build the wasm `(params, results)` signature for `f` without
/// allocating `BindingId` slots. Used by the import-declaration
/// path, where the imported function has no body and so no
/// parameter binding map is required.
fn lower_function_signature(
    f: &IrFunction,
) -> Result<(Vec<ValType>, Vec<ValType>), ModuleLowerError> {
    let (params, _) = lower_params(f)?;
    let results = body_result_types(f.return_type.as_ref())?;
    Ok((params, results))
}

fn lower_params(f: &IrFunction) -> Result<(Vec<ValType>, Vec<ParamBinding>), ModuleLowerError> {
    let mut valtypes = Vec::with_capacity(f.params.len());
    let mut bindings = Vec::with_capacity(f.params.len());

    for param in &f.params {
        let ty = param
            .ty
            .as_ref()
            .ok_or_else(|| ModuleLowerError::MissingParamType {
                function: f.name.clone(),
                name: param.name.clone(),
            })?;
        let vt = param_value_type(param, ty, &f.name)?;
        valtypes.push(vt);
        bindings.push((param.binding_id, vt));
    }

    Ok((valtypes, bindings))
}

fn param_value_type(
    param: &IrFunctionParam,
    ty: &ResolvedType,
    function: &str,
) -> Result<ValType, ModuleLowerError> {
    body_value_type(ty)?.ok_or_else(|| ModuleLowerError::MissingParamType {
        function: function.to_owned(),
        name: param.name.clone(),
    })
}
