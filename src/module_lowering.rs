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
struct PreludeHelpers {
    str_len: u32,
    str_is_empty: u32,
    str_byte_at: u32,
    str_slice: u32,
    str_starts_with: u32,
    str_contains: u32,
    array_len: u32,
    array_is_empty: u32,
    optional_is_some: u32,
    optional_is_none: u32,
    range_len: u32,
    range_is_empty: u32,
    dict_len: u32,
    dict_is_empty: u32,
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

    /// A layout-planner failure surfaced during canonical-ABI
    /// wrapper synthesis (e.g. struct/enum size overflow).
    #[error(transparent)]
    Layout(#[from] crate::layout::LayoutError),
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
    // First pass: declare every extern fn as a wasm import using
    // the *canonical-ABI* shape the host actually provides
    // (`String` / `Path` / `Regex` / `list<T>` lift to `(ptr,
    // len)` rather than a single header pointer). The import
    // declaration is what wit-component validates against, so it
    // must match the WIT signature.
    //
    // We can't update `function_map` to point at the raw import
    // directly: internal call sites (`IrExpr::FunctionCall` →
    // `sink.call(wasm_idx)`) push internal-ABI args (a header
    // pointer per String). A second pass emits a per-import
    // trampoline that bridges the two: load `(ptr, len)` from
    // each header on entry, call the raw import, propagate
    // results.
    let mut extern_import_indices: Vec<(FunctionId, u32, &IrFunction)> = Vec::new();
    for (i, f) in module.functions.iter().enumerate() {
        if !f.is_extern() {
            continue;
        }
        let id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let (params, results) = canonical_abi_import_signature(f, module)?;
        let wasm_idx = builder.declare_function_import(
            IMPORT_MODULE_NAME,
            &kebab_case(&f.name),
            &params,
            &results,
        );
        extern_import_indices.push((FunctionId(id_raw), wasm_idx, f));
    }

    // `extern impl <Struct>` blocks: each method becomes a wasm
    // function import. Names follow the `<struct>-<method>`
    // kebab-case convention so the host-side WIT world can declare
    // each as a free `import` once the WIT emitter learns the same
    // shape. Register the imported index in `method_map` directly
    // — it lives in the same wasm function-index space as user
    // method bodies, but landed earlier (in the import region).
    //
    // Primitive-target extern impls (the prelude's String surface)
    // are still handled by the runtime-helper path further down;
    // this loop only covers `extern impl <UserStruct>` /
    // `<UserEnum>` shapes coming from user `.fv` source.
    let mut extern_method_map: MethodMap = MethodMap::new();
    for (i, imp) in module.impls.iter().enumerate() {
        if !imp.is_extern {
            continue;
        }
        // Skip prelude built-ins (Optional / Array / Dictionary /
        // Range / String). Each is `extern impl` in
        // `src/prelude.fv` because the language-side surface is
        // declared there, but the methods are backend-supplied
        // runtime helpers — not host imports. Registering them
        // here would force every component embedder to wire
        // `array-len`, `optional-is-some` &c. just to instantiate
        // a module that calls them. Calls into prelude methods
        // resolve through `prelude_helper_index` further down.
        let is_prelude_target = match imp.target {
            ImplTarget::Struct(sid) => {
                Some(sid) == module.prelude_array_id()
                    || Some(sid) == module.prelude_dictionary_id()
                    || Some(sid) == module.prelude_range_id()
            }
            ImplTarget::Enum(eid) => Some(eid) == module.prelude_optional_id(),
            ImplTarget::Primitive(_) => true,
        };
        if is_prelude_target {
            continue;
        }
        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let (target_name, drops_self): (Option<String>, bool) = match imp.target {
            ImplTarget::Struct(sid) => match module.structs.get(sid.0 as usize) {
                // Empty receiver structs (`pub struct Canvas {}`)
                // are filtered from the WIT public surface (wit-
                // component rejects empty records), so the WIT
                // signature drops self. To keep the wasm import
                // shape matching the WIT, drop self here too —
                // and let the call-site path materialise the host
                // call by stripping the receiver pointer
                // (handled by the dispatch lowering as a follow-
                // up if needed; today the host can simply ignore
                // the missing receiver since empty structs carry
                // no state).
                Some(s) if s.fields.is_empty() => (Some(s.name.clone()), true),
                Some(s) => (Some(s.name.clone()), false),
                None => (None, false),
            },
            ImplTarget::Enum(eid) => (
                module.enums.get(eid.0 as usize).map(|e| e.name.clone()),
                false,
            ),
            ImplTarget::Primitive(_) => (None, false), // handled separately
        };
        let Some(target_name) = target_name else {
            continue;
        };
        for (j, m) in imp.functions.iter().enumerate() {
            let m_idx_raw = u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            // Inject the impl's self-struct/enum type for the
            // implicit `self` param so signature lowering succeeds
            // even when the frontend leaves `self.ty == None`.
            let self_ty = match imp.target {
                ImplTarget::Struct(sid) => Some(ResolvedType::Struct(sid)),
                ImplTarget::Enum(eid) => Some(ResolvedType::Enum(eid)),
                ImplTarget::Primitive(_) => None,
            };
            let (mut params, _) = lower_params_with_self_ty(m, self_ty.as_ref())?;
            if drops_self && !params.is_empty() {
                // Strip the leading `self` valtype so the wasm
                // import matches the WIT (which omits self for
                // empty receiver structs).
                params.remove(0);
            }
            let results = body_result_types(m.return_type.as_ref())?;
            let import_name = format!("{}-{}", kebab_case(&target_name), kebab_case(&m.name));
            let wasm_idx = builder.declare_function_import(
                IMPORT_MODULE_NAME,
                &import_name,
                &params,
                &results,
            );
            extern_method_map.insert((ImplId(impl_id_raw), MethodIdx(m_idx_raw)), wasm_idx);
        }
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
        array_len: builder.declare_array_len(),
        array_is_empty: builder.declare_array_is_empty(),
        optional_is_some: builder.declare_optional_is_some(),
        optional_is_none: builder.declare_optional_is_none(),
        range_len: builder.declare_range_len(),
        range_is_empty: builder.declare_range_is_empty(),
        dict_len: builder.declare_dict_len(),
        dict_is_empty: builder.declare_dict_is_empty(),
    };
    // `cabi_realloc` is exported so the component runtime can
    // allocate buffers in our linear memory when lowering `string`
    // / `list<T>` arguments. Always declared so any public function
    // that takes one of those types lands an export-ready module.
    let cabi_realloc_idx = builder.declare_cabi_realloc();
    let mut helpers_end = cabi_realloc_idx
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;

    // Emit per-method trampolines for extern impl methods on
    // empty receiver structs: callers push `self`, but the WIT
    // and raw wasm import drop it. The trampoline takes the
    // internal-ABI shape (with self), drops the receiver, and
    // forwards the rest to the raw import.
    let extern_method_keys: Vec<(ImplId, MethodIdx, u32)> = extern_method_map
        .iter()
        .map(|(k, v)| (k.0, k.1, *v))
        .collect();
    let mut method_trampoline_replace: Vec<((ImplId, MethodIdx), u32)> = Vec::new();
    for (impl_id, method_idx, raw_import_idx) in extern_method_keys {
        let Some(imp) = module.impls.get(impl_id.0 as usize) else {
            continue;
        };
        let drops_self = match &imp.target {
            ImplTarget::Struct(sid) => module
                .structs
                .get(sid.0 as usize)
                .is_some_and(|s| s.fields.is_empty()),
            ImplTarget::Enum(_) | ImplTarget::Primitive(_) => false,
        };
        if !drops_self {
            continue;
        }
        let Some(m) = imp.functions.get(method_idx.0 as usize) else {
            continue;
        };
        let self_struct_id = match imp.target {
            ImplTarget::Struct(sid) => Some(sid),
            ImplTarget::Enum(_) | ImplTarget::Primitive(_) => None,
        };
        let (internal_params, _) = lower_params_with_self(m, self_struct_id)?;
        let result_valtypes = body_result_types(m.return_type.as_ref())?;
        let mut body = wasm_encoder::Function::new(Vec::new());
        {
            let mut i = body.instructions();
            // Push every internal param except slot 0 (the
            // dropped receiver).
            for slot in 1..internal_params.len() {
                let slot_idx =
                    u32::try_from(slot).map_err(|_| ModuleLowerError::TooManyFunctions)?;
                i.local_get(slot_idx);
            }
            i.call(raw_import_idx);
            i.end();
        }
        let trampoline_idx =
            builder.declare_function_with_body(&internal_params, &result_valtypes, &body);
        if trampoline_idx != helpers_end {
            return Err(ModuleLowerError::TooManyFunctions);
        }
        helpers_end = helpers_end
            .checked_add(1)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        method_trampoline_replace.push(((impl_id, method_idx), trampoline_idx));
    }
    for ((impl_id, method_idx), trampoline_idx) in method_trampoline_replace {
        extern_method_map.insert((impl_id, method_idx), trampoline_idx);
    }

    // Emit one trampoline per extern fn import whose canonical-ABI
    // shape diverges from the internal pointer ABI. The trampoline
    // takes header pointers, reads `(ptr, len)` from each, calls
    // the raw import, and forwards results. `function_map[fid]`
    // points at the trampoline so internal callers don't need to
    // know about the `ptr/len` split.
    for (fid, raw_import_idx, f) in extern_import_indices {
        let trampoline_idx = if needs_canonical_abi_wrapper(f, module) {
            let idx = emit_canonical_abi_import_trampoline(
                f,
                raw_import_idx,
                bump_idx,
                &mut builder,
                module,
                helpers_end,
            )?;
            helpers_end = helpers_end
                .checked_add(1)
                .ok_or(ModuleLowerError::TooManyFunctions)?;
            idx
        } else {
            raw_import_idx
        };
        function_map.insert(fid, trampoline_idx);
    }

    let user_offset = helpers_end;

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
        // Only reserve a wrapper slot when `emit_function`'s export
        // path will actually emit one — closure-conv-lifted
        // helpers (`__` prefix) skip exports and so skip wrappers.
        // Without this guard we'd reserve a slot we never fill,
        // shifting every later function index by 1.
        if !f.name.starts_with("__") && needs_canonical_abi_wrapper(f, module) {
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
    // Carry forward extern-impl method imports registered at the
    // earlier import-declaration phase. Their wasm indices already
    // live in the import region; the rest of the impl walk
    // resolves user-defined methods to indices past
    // `methods_offset`.
    for (key, idx) in extern_method_map.iter() {
        method_map.insert(*key, *idx);
    }
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
            // `extern impl <prelude compound>` (Optional / Array /
            // Range / Dictionary): each method resolves to a
            // backend-emitted runtime helper rather than a host
            // import. Methods we don't yet wire fall through to
            // `UnknownMethod` at lowering, matching the String
            // path's behaviour above.
            let prelude_compound: Option<&str> = match imp.target {
                ImplTarget::Struct(sid) if Some(sid) == module.prelude_array_id() => Some("array"),
                ImplTarget::Struct(sid) if Some(sid) == module.prelude_range_id() => Some("range"),
                ImplTarget::Struct(sid) if Some(sid) == module.prelude_dictionary_id() => {
                    Some("dict")
                }
                ImplTarget::Enum(eid) if Some(eid) == module.prelude_optional_id() => {
                    Some("optional")
                }
                ImplTarget::Struct(_) | ImplTarget::Enum(_) | ImplTarget::Primitive(_) => None,
            };
            if let Some(target) = prelude_compound {
                for (j, m) in imp.functions.iter().enumerate() {
                    let m_idx_raw =
                        u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
                    let helper_idx = match (target, m.name.as_str()) {
                        ("array", "len") => Some(helpers.array_len),
                        ("array", "is_empty") => Some(helpers.array_is_empty),
                        ("optional", "is_some") => Some(helpers.optional_is_some),
                        ("optional", "is_none") => Some(helpers.optional_is_none),
                        ("range", "len") => Some(helpers.range_len),
                        ("range", "is_empty") => Some(helpers.range_is_empty),
                        ("dict", "len") => Some(helpers.dict_len),
                        ("dict", "is_empty") => Some(helpers.dict_is_empty),
                        _ => None,
                    };
                    if let Some(idx) = helper_idx {
                        method_map.insert((ImplId(impl_id_raw), MethodIdx(m_idx_raw)), idx);
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

    // WIT export names must be unique. Pre-compute the set of
    // top-level FunctionIds that "win" each kebab-cased name (first
    // occurrence wins, mirroring `wit::emit_wit`'s emit-once filter)
    // so duplicates lower to internal-only functions without
    // emitting a colliding wasm export.
    let mut export_winners: std::collections::HashSet<u32> = std::collections::HashSet::new();
    {
        let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (i, f) in module.functions.iter().enumerate() {
            if f.is_extern() || f.name.starts_with("__") {
                continue;
            }
            let i_u32 = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let name = kebab_case(&f.name);
            if taken.insert(name) {
                export_winners.insert(i_u32);
            }
        }
    }
    for (i, f) in module.functions.iter().enumerate() {
        if f.is_extern() {
            continue;
        }
        let i_u32 = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let allow_export = export_winners.contains(&i_u32);
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
            allow_export,
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
            // The explicit `Number | Boolean | Nil` arm and the
            // trailing wildcard share an empty body, but the wildcard
            // is mandatory because `Literal` is `#[non_exhaustive]`.
            // Listing the known no-string-bearing variants keeps the
            // intent visible if the upstream enum gains a new
            // variant.
            #[expect(
                clippy::match_same_arms,
                reason = "non_exhaustive Literal forces a wildcard alongside the named no-op variants"
            )]
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

        let impl_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        write_vtable_cells(
            trait_id,
            ImplId(impl_id_raw),
            imp,
            trait_decl,
            module,
            &method_funcref_indices,
            &mut vtable_data,
        )?;
    }

    let mut call_type_indices: HashMap<(TraitId, MethodIdx), u32> = HashMap::new();
    for (i, _) in module.traits.iter().enumerate() {
        let trait_id_raw = u32::try_from(i).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let trait_id = TraitId(trait_id_raw);
        // Register a call_indirect type for every effective method —
        // direct and inherited — so virtual call sites against this
        // trait can dispatch through the inherited slots too.
        for (j, (_, sig)) in effective_trait_methods(trait_id, module)?
            .iter()
            .enumerate()
        {
            let method_idx_raw =
                u32::try_from(j).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let type_idx = register_trait_method_call_type(sig, builder)?;
            call_type_indices.insert((trait_id, MethodIdx(method_idx_raw)), type_idx);
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

/// Flatten `trait_id`'s methods plus every method inherited from its
/// `composed_traits`, transitively. Each entry is paired with the
/// `TraitId` that originally *declared* the method so callers can
/// look up the corresponding impl block. Methods are de-duplicated
/// by name (the first occurrence wins). Inheritance walks
/// breadth-first starting from `trait_id`'s own methods, then the
/// direct parents in declaration order, then their parents, etc.
/// Append one funcref-slot cell per effective trait method onto
/// `vtable_data`. Walks the trait's effective methods (own +
/// inherited from `composed_traits`) and finds, for each one, the
/// providing impl — either the current `imp` (when the method was
/// declared on the trait being walked) or the matching
/// `impl <ParentTrait> for <Target>` block (when inherited).
fn write_vtable_cells(
    trait_id: TraitId,
    impl_id: ImplId,
    imp: &formalang::ir::IrImpl,
    trait_decl: &formalang::ir::IrTrait,
    module: &IrModule,
    method_funcref_indices: &HashMap<(ImplId, MethodIdx), u32>,
    vtable_data: &mut Vec<u8>,
) -> Result<(), ModuleLowerError> {
    for (owning_trait_id, trait_method) in &effective_trait_methods(trait_id, module)? {
        let funcref_slot = if *owning_trait_id == trait_id {
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
            let key = (impl_id, MethodIdx(m_idx_raw));
            method_funcref_indices
                .get(&key)
                .copied()
                .ok_or(ModuleLowerError::TooManyFunctions)?
        } else {
            // Find `impl owning_trait_id for imp.target`.
            let (parent_impl_idx, parent_method_idx_in_impl) = module
                .impls
                .iter()
                .enumerate()
                .find_map(|(idx, candidate)| {
                    if candidate.is_extern {
                        return None;
                    }
                    let tref = candidate.trait_ref.as_ref()?;
                    if tref.trait_id != *owning_trait_id || candidate.target != imp.target {
                        return None;
                    }
                    let (m_idx, _) = candidate
                        .functions
                        .iter()
                        .enumerate()
                        .find(|(_, f)| f.name == trait_method.name)?;
                    Some((idx, m_idx))
                })
                .ok_or_else(|| ModuleLowerError::MissingTraitMethod {
                    trait_name: trait_decl.name.clone(),
                    method: trait_method.name.clone(),
                    target: imp.target,
                })?;
            let parent_impl_id_raw =
                u32::try_from(parent_impl_idx).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let parent_m_idx_raw = u32::try_from(parent_method_idx_in_impl)
                .map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let key = (ImplId(parent_impl_id_raw), MethodIdx(parent_m_idx_raw));
            method_funcref_indices
                .get(&key)
                .copied()
                .ok_or(ModuleLowerError::TooManyFunctions)?
        };
        vtable_data.extend_from_slice(&funcref_slot.to_le_bytes());
    }
    Ok(())
}

fn effective_trait_methods(
    trait_id: TraitId,
    module: &IrModule,
) -> Result<Vec<(TraitId, formalang::ir::IrFunctionSig)>, ModuleLowerError> {
    let mut out: Vec<(TraitId, formalang::ir::IrFunctionSig)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut stack: Vec<TraitId> = vec![trait_id];
    let mut visited_traits: std::collections::HashSet<u32> = std::collections::HashSet::new();
    while let Some(tid) = stack.first().copied() {
        stack.remove(0);
        if !visited_traits.insert(tid.0) {
            continue;
        }
        let t = module
            .traits
            .get(tid.0 as usize)
            .ok_or(ModuleLowerError::UnknownTrait(tid))?;
        for sig in &t.methods {
            if seen.insert(sig.name.clone()) {
                out.push((tid, sig.clone()));
            }
        }
        for &parent in &t.composed_traits {
            stack.push(parent);
        }
    }
    Ok(out)
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
            // Impl methods are never WIT-level exports — the export
            // gate inside `emit_function` already filters them via
            // `impl_self_struct_id`.
            false,
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
    allow_export: bool,
) -> Result<(), ModuleLowerError> {
    let body_expr = f
        .body
        .as_ref()
        .ok_or_else(|| ModuleLowerError::MissingFunctionBody {
            name: f.name.clone(),
        })?;

    let (param_valtypes, param_bindings) = lower_params_with_self(f, impl_self_struct_id)?;
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
    if impl_self_struct_id.is_none() && !f.name.starts_with("__") && allow_export {
        // Public functions whose signatures carry types whose
        // canonical-ABI lowering differs from our internal pointer
        // convention need a thin trampoline: the trampoline matches
        // the canonical-ABI shape the host sees, while the inner
        // function keeps using the internal header-pointer
        // convention so intra-module callers don't need to change.
        //
        // The `__`-prefix gate mirrors `survey::survey`'s filter:
        // closure-conv-synthesized helpers stay private. The
        // wit-expressibility gate, by contrast, only applies on the
        // WIT side (`wit::emit_wit`); we always emit the wasm
        // export so core-wasm-only tests can call the function. If
        // the WIT side filters and the consumer is component-
        // wrapped, that'll surface as a wit-component validation
        // failure — which is the right diagnostic shape for "you
        // marked a non-pub-style helper as a top-level function but
        // shipped it through the component pipeline".
        let export_idx = if needs_canonical_abi_wrapper(f, module) {
            emit_canonical_abi_wrapper(f, wasm_idx, builder, module)?
        } else {
            wasm_idx
        };
        builder.export_function(&kebab_case(&f.name), export_idx);
    }
    Ok(())
}

/// Whether `f`'s public signature lowers to a different core-wasm
/// shape than its internal one.
///
/// Today this is exactly the case where any `String` / `Path` /
/// `Regex` / `list<T>` reaches the function boundary as a parameter.
/// Returns of those types match the internal i32-pointer return
/// directly because the canonical ABI's "return area pointer"
/// convention coincides with our header layout.
/// Build the canonical-ABI `(params, results)` signature for an
/// extern fn import: each `String` / `Path` / `Regex` / `list<T>`
/// param expands to two i32s `(ptr, len)`, the rest pass through
/// unchanged. Results follow the same shape as `body_result_types`
/// — extern fns never return aggregates today.
fn canonical_abi_import_signature(
    f: &IrFunction,
    module: &IrModule,
) -> Result<(Vec<ValType>, Vec<ValType>), ModuleLowerError> {
    let mut params = Vec::with_capacity(f.params.len());
    for p in &f.params {
        let ty =
            p.ty.as_ref()
                .ok_or_else(|| ModuleLowerError::MissingParamType {
                    function: f.name.clone(),
                    name: p.name.clone(),
                })?;
        if param_needs_split(ty, module) {
            params.push(ValType::I32);
            params.push(ValType::I32);
        } else {
            let vt = body_value_type(ty)?.ok_or_else(|| {
                ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                    kind: "Never-typed parameter on extern function".to_owned(),
                })
            })?;
            params.push(vt);
        }
    }
    let results = body_result_types(f.return_type.as_ref())?;
    Ok((params, results))
}

/// Emit a thin trampoline that bridges the internal pointer ABI
/// (one `i32` header pointer per `String` / `list<T>` param) to the
/// canonical-ABI shape the raw import was declared with (two
/// `i32`s `(ptr, len)` per such param). Returns the trampoline's
/// wasm function index. Internal call sites continue to push
/// header pointers; the trampoline reads `(ptr, len)` from each
/// header and forwards them to the import.
fn emit_canonical_abi_import_trampoline(
    f: &IrFunction,
    raw_import_idx: u32,
    _bump_idx: u32,
    builder: &mut ModuleBuilder,
    module: &IrModule,
    expected_idx: u32,
) -> Result<u32, ModuleLowerError> {
    use crate::layout::{STRING_LEN_OFFSET, STRING_PTR_OFFSET};
    use crate::module::MEMORY_INDEX;
    use wasm_encoder::{Function, MemArg};

    // Internal-ABI param valtypes: every param is one i32 header
    // pointer for split types, otherwise the matching primitive
    // valtype.
    let (internal_params, _) = lower_function_signature(f)?;
    let result_valtypes = body_result_types(f.return_type.as_ref())?;

    let mut body = Function::new(Vec::new());
    let mem_arg = |offset: u64| MemArg {
        offset,
        align: 2,
        memory_index: MEMORY_INDEX,
    };
    {
        let mut i = body.instructions();
        for (idx, p) in f.params.iter().enumerate() {
            let local_idx = u32::try_from(idx).map_err(|_| ModuleLowerError::TooManyFunctions)?;
            let ty =
                p.ty.as_ref()
                    .ok_or_else(|| ModuleLowerError::MissingParamType {
                        function: f.name.clone(),
                        name: p.name.clone(),
                    })?;
            if param_needs_split(ty, module) {
                // Read (ptr, len) from the header at offsets 0 and 4.
                i.local_get(local_idx)
                    .i32_load(mem_arg(u64::from(STRING_PTR_OFFSET)));
                i.local_get(local_idx)
                    .i32_load(mem_arg(u64::from(STRING_LEN_OFFSET)));
            } else {
                i.local_get(local_idx);
            }
        }
        i.call(raw_import_idx);
        i.end();
    }

    let actual_idx = builder.declare_function_with_body(&internal_params, &result_valtypes, &body);
    if actual_idx != expected_idx {
        return Err(ModuleLowerError::TooManyFunctions);
    }
    Ok(actual_idx)
}

fn needs_canonical_abi_wrapper(f: &IrFunction, module: &IrModule) -> bool {
    f.params.iter().any(|p| {
        p.ty.as_ref()
            .is_some_and(|ty| param_needs_split(ty, module))
    })
}

/// Whether a parameter type lowers to multiple core-wasm i32 values
/// at the canonical-ABI boundary. `string` / `Path` / `Regex` / list
/// pass as `(ptr, len)`; record / variant types flatten as a
/// concatenation of their fields' canonical representations.
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
    if matches!(
        crate::compound::Compound::of(ty, module),
        crate::compound::Compound::Array(_)
    ) {
        return true;
    }
    // Record / variant flatten: the canonical-ABI passes the
    // fields directly. Structs always need flattening (every
    // non-empty pub struct surfaces as a record on the WIT
    // side); enums similarly flatten as `(tag, ...payload)`.
    if let Some(_sid) = crate::compound::struct_id_of(ty) {
        return true;
    }
    if let Some(_eid) = crate::compound::enum_id_of(ty) {
        return true;
    }
    if matches!(ty, ResolvedType::Tuple(_)) {
        return true;
    }
    false
}

/// Flatten a [`ResolvedType`] into the sequence of core-wasm value
/// types the canonical ABI represents it with at the component
/// boundary. Mirrors the spec's `flatten_type` lifting rules:
///
/// - Numeric / boolean primitives flatten to one matching
///   `ValType` slot.
/// - `string`, `Path`, `Regex`, `list<T>` flatten to `(ptr, len)`
///   = two `ValType::I32` slots.
/// - Non-empty structs flatten to the concatenation of their
///   fields' canonical reps.
/// - Variants flatten to `(tag, ...flat-of-largest-arm-payload)`.
///
/// Empty structs and `Never` collapse to zero slots — `Never` is
/// also rejected upstream as a parameter type.
fn flatten_canonical_abi(
    ty: &ResolvedType,
    module: &IrModule,
) -> Result<Vec<ValType>, ModuleLowerError> {
    use formalang::ast::PrimitiveType;
    match ty {
        ResolvedType::Primitive(p) => match p {
            PrimitiveType::String | PrimitiveType::Path | PrimitiveType::Regex => {
                Ok(vec![ValType::I32, ValType::I32])
            }
            PrimitiveType::Never => Ok(Vec::new()),
            PrimitiveType::I32
            | PrimitiveType::I64
            | PrimitiveType::F32
            | PrimitiveType::F64
            | PrimitiveType::Boolean => {
                let vt = body_value_type(ty)?.ok_or_else(|| {
                    ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                        kind: format!("primitive {p:?} as canonical-ABI value"),
                    })
                })?;
                Ok(vec![vt])
            }
            // PrimitiveType is `#[non_exhaustive]`; route any future
            // variant through a typed error rather than a silent
            // unknown lowering.
            other => Err(ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                kind: format!(
                    "PrimitiveType variant {other:?} not handled by canonical-ABI flatten"
                ),
            })),
        },
        ResolvedType::Struct(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::Trait(_)
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => {
            if matches!(
                crate::compound::Compound::of(ty, module),
                crate::compound::Compound::Array(_)
            ) {
                return Ok(vec![ValType::I32, ValType::I32]);
            }
            if let Some(sid) = crate::compound::struct_id_of(ty) {
                let s = module.structs.get(sid.0 as usize).ok_or_else(|| {
                    ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                        kind: format!("Struct({}) out of range", sid.0),
                    })
                })?;
                let mut out = Vec::new();
                for field in &s.fields {
                    out.extend(flatten_canonical_abi(&field.ty, module)?);
                }
                return Ok(out);
            }
            if let ResolvedType::Tuple(fields) = ty {
                let mut out = Vec::new();
                for (_, inner) in fields {
                    out.extend(flatten_canonical_abi(inner, module)?);
                }
                return Ok(out);
            }
            if let Some(eid) = crate::compound::enum_id_of(ty) {
                let e_decl = module.enums.get(eid.0 as usize).ok_or_else(|| {
                    ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                        kind: format!("Enum({}) out of range", eid.0),
                    })
                })?;
                // Substitute the generic args so variant fields
                // referencing `TypeParam("T")` lift correctly when
                // the enum is `Optional<String>` etc.
                let type_args = crate::compound::generic_args_for_enum(ty, eid);
                let e_owned =
                    crate::compound::substitute_enum(e_decl, &e_decl.generic_params, type_args);
                let e = &e_owned;
                let mut max_payload: Vec<ValType> = Vec::new();
                for variant in &e.variants {
                    let mut payload = Vec::new();
                    for field in &variant.fields {
                        payload.extend(flatten_canonical_abi(&field.ty, module)?);
                    }
                    if payload.len() > max_payload.len() {
                        max_payload = payload;
                    }
                }
                let mut out = Vec::with_capacity(max_payload.len().saturating_add(1));
                out.push(ValType::I32);
                out.extend(max_payload);
                return Ok(out);
            }
            Ok(vec![body_value_type(ty)?.unwrap_or(ValType::I32)])
        }
    }
}

// ── canonical-ABI wrapper plumbing ────────────────────────────────
//
// `emit_canonical_abi_wrapper` builds, per public function, a thin
// trampoline that bridges the host-visible canonical-ABI shape (e.g.
// `string` flattens to `(ptr, len)` i32 pairs) to the internal
// header-pointer convention. The types below are the materialisation
// plan — one `Plan` per param, each `Plan::Materialised` carrying a
// `MaterialisationOps` recipe describing how to allocate the header
// or aggregate from the flat slots.
//
// They live at module scope (rather than inside the wrapper fn)
// because the helper functions (`build_field_value`,
// `emit_materialise`) need them too, and embedding type defs after
// `let` statements trips `clippy::items_after_statements`.

/// Span of canonical-ABI flat slots a single source-level parameter
/// occupies. Built in pass 1 of the wrapper before we know whether
/// each param will pass through directly or be materialised.
struct ParamSlots {
    flat_start: u32,
    flat_count: u32,
}

/// Per-param plan for the canonical-ABI wrapper. `Direct` means the
/// param's single canonical slot is forwarded as-is; `Materialised`
/// means the wrapper builds an aggregate / header in linear memory
/// and forwards the pointer to it.
enum CabiPlan {
    Direct {
        slot: u32,
    },
    Materialised {
        scratch: u32,
        ops: CabiMaterialisationOps,
    },
}

/// Recipe for one materialisation step inside the wrapper. Each
/// variant carries enough info to read its inputs from the wrapper's
/// locals and produce the corresponding header / aggregate shape in
/// linear memory.
enum CabiMaterialisationOps {
    StringFromSlots {
        ptr_slot: u32,
        len_slot: u32,
    },
    ListFromSlots {
        ptr_slot: u32,
        len_slot: u32,
    },
    /// Build a struct of `size` bytes by storing each field
    /// produced by a sub-recipe at its layout offset. The sub-
    /// recipe owns its own scratch when the field type itself
    /// requires materialisation.
    Struct {
        size: u32,
        fields: Vec<CabiStructFieldOp>,
    },
    /// Variant: alloc enum cell, store tag from `tag_slot`,
    /// for each variant try to dispatch on tag; today we
    /// unconditionally store ALL flat-payload bytes at
    /// `payload_offset` since each arm's storage shares the
    /// pointer-aligned payload region.
    Variant {
        size: u32,
        tag_slot: u32,
        payload_offset: u32,
        payload_slots: Vec<u32>,
    },
}

struct CabiStructFieldOp {
    offset: u32,
    /// Either a direct i32-value-store (primitive field) or a
    /// sub-materialisation that produces an i32 pointer to
    /// store at the field offset.
    value: CabiFieldValue,
}

enum CabiFieldValue {
    DirectSlot {
        slot: u32,
        valtype: ValType,
    },
    MaterialisedScratch {
        scratch: u32,
        ops: Box<CabiMaterialisationOps>,
    },
}

/// Recursively build a [`CabiMaterialisationOps`] for `ty`, pulling
/// consecutive flat slots from `cursor` and reserving scratch
/// locals as it descends. Returns the per-field plan (direct slot
/// pass-through or materialisation recipe).
fn build_cabi_field_value(
    ty: &ResolvedType,
    cursor: &mut u32,
    scratch_count: &mut u32,
    scratch_base: u32,
    module: &IrModule,
) -> Result<CabiFieldValue, ModuleLowerError> {
    use formalang::ast::PrimitiveType;
    // Strings / lists materialise to a header.
    let needs_string_header = matches!(
        ty,
        ResolvedType::Primitive(PrimitiveType::String | PrimitiveType::Path | PrimitiveType::Regex)
    );
    let needs_list_header = matches!(
        crate::compound::Compound::of(ty, module),
        crate::compound::Compound::Array(_)
    );
    if needs_string_header || needs_list_header {
        let ptr_slot = *cursor;
        let len_slot = ptr_slot
            .checked_add(1)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        *cursor = cursor
            .checked_add(2)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        let scratch = scratch_base
            .checked_add(*scratch_count)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        *scratch_count = scratch_count
            .checked_add(1)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        return Ok(CabiFieldValue::MaterialisedScratch {
            scratch,
            ops: Box::new(if needs_string_header {
                CabiMaterialisationOps::StringFromSlots { ptr_slot, len_slot }
            } else {
                CabiMaterialisationOps::ListFromSlots { ptr_slot, len_slot }
            }),
        });
    }
    // Tuples reuse the struct-style materialisation.
    if let ResolvedType::Tuple(fields) = ty {
        let synthetic = crate::lower::aggregate::synthetic_struct_for_tuple(ty).map_err(|e| {
            if matches!(
                e,
                crate::lower::LowerError::FieldAccessOnNonAggregate { .. }
            ) {
                ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                    kind: format!("synthetic tuple for {ty:?}"),
                })
            } else {
                ModuleLowerError::Lower(e)
            }
        })?;
        let layout = crate::layout::plan_struct(&synthetic, module)?;
        let mut field_ops: Vec<CabiStructFieldOp> = Vec::with_capacity(fields.len());
        for ((_, fty), flayout) in fields.iter().zip(layout.fields.iter()) {
            field_ops.push(CabiStructFieldOp {
                offset: flayout.offset,
                value: build_cabi_field_value(fty, cursor, scratch_count, scratch_base, module)?,
            });
        }
        let scratch = scratch_base
            .checked_add(*scratch_count)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        *scratch_count = scratch_count
            .checked_add(1)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        return Ok(CabiFieldValue::MaterialisedScratch {
            scratch,
            ops: Box::new(CabiMaterialisationOps::Struct {
                size: layout.size,
                fields: field_ops,
            }),
        });
    }
    // Nested struct / enum.
    if let Some(sid) = crate::compound::struct_id_of(ty) {
        return build_cabi_struct_value(sid, cursor, scratch_count, scratch_base, module);
    }
    if crate::compound::enum_id_of(ty).is_some() {
        return build_cabi_variant_value(ty, cursor, scratch_count, scratch_base, module);
    }
    // Primitive: direct pass-through of one slot.
    let valtype = body_value_type(ty)?.ok_or_else(|| {
        ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("Never / unsupported parameter type {ty:?}"),
        })
    })?;
    let slot = *cursor;
    *cursor = cursor
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    Ok(CabiFieldValue::DirectSlot { slot, valtype })
}

/// Build a `Struct` materialisation plan for a struct-typed
/// canonical-ABI parameter. Caller has already resolved `sid`
/// from the receiver type.
fn build_cabi_struct_value(
    sid: StructId,
    cursor: &mut u32,
    scratch_count: &mut u32,
    scratch_base: u32,
    module: &IrModule,
) -> Result<CabiFieldValue, ModuleLowerError> {
    let s = module.structs.get(sid.0 as usize).ok_or_else(|| {
        ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("Struct({}) out of range", sid.0),
        })
    })?;
    let layout = crate::layout::plan_struct(s, module)?;
    let mut fields: Vec<CabiStructFieldOp> = Vec::with_capacity(s.fields.len());
    for (fdef, flayout) in s.fields.iter().zip(layout.fields.iter()) {
        fields.push(CabiStructFieldOp {
            offset: flayout.offset,
            value: build_cabi_field_value(&fdef.ty, cursor, scratch_count, scratch_base, module)?,
        });
    }
    let scratch = scratch_base
        .checked_add(*scratch_count)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    *scratch_count = scratch_count
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    Ok(CabiFieldValue::MaterialisedScratch {
        scratch,
        ops: Box::new(CabiMaterialisationOps::Struct {
            size: layout.size,
            fields,
        }),
    })
}

/// Build a `Variant` materialisation plan for an enum-typed
/// canonical-ABI parameter. Caller must already have established
/// that `enum_id_of(ty)` is `Some(_)`.
fn build_cabi_variant_value(
    ty: &ResolvedType,
    cursor: &mut u32,
    scratch_count: &mut u32,
    scratch_base: u32,
    module: &IrModule,
) -> Result<CabiFieldValue, ModuleLowerError> {
    let eid = crate::compound::enum_id_of(ty).ok_or_else(|| {
        ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("build_cabi_variant_value called with non-enum type {ty:?}"),
        })
    })?;
    let e_decl = module.enums.get(eid.0 as usize).ok_or_else(|| {
        ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
            kind: format!("Enum({}) out of range", eid.0),
        })
    })?;
    let type_args = crate::compound::generic_args_for_enum(ty, eid);
    let e_owned = crate::compound::substitute_enum(e_decl, &e_decl.generic_params, type_args);
    let layout = crate::layout::plan_enum(&e_owned, module)?;
    let tag_slot = *cursor;
    *cursor = cursor
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    // Determine the longest payload's flat count among arms so the
    // cursor advances correctly.
    let mut max_payload_count: u32 = 0;
    for v in &e_owned.variants {
        let mut sub_count: u32 = 0;
        for fdef in &v.fields {
            let f = flatten_canonical_abi(&fdef.ty, module)?;
            sub_count = sub_count
                .checked_add(
                    u32::try_from(f.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?,
                )
                .ok_or(ModuleLowerError::TooManyFunctions)?;
        }
        if sub_count > max_payload_count {
            max_payload_count = sub_count;
        }
    }
    let mut payload_slots = Vec::with_capacity(max_payload_count as usize);
    for _ in 0..max_payload_count {
        payload_slots.push(*cursor);
        *cursor = cursor
            .checked_add(1)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
    }
    let scratch = scratch_base
        .checked_add(*scratch_count)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    *scratch_count = scratch_count
        .checked_add(1)
        .ok_or(ModuleLowerError::TooManyFunctions)?;
    Ok(CabiFieldValue::MaterialisedScratch {
        scratch,
        ops: Box::new(CabiMaterialisationOps::Variant {
            size: layout.size,
            tag_slot,
            payload_offset: layout.payload_offset,
            payload_slots,
        }),
    })
}

/// Emit the wasm instructions for one materialisation step. Reads
/// the inputs from the wrapper's locals (`ptr_slot`, `len_slot`,
/// `tag_slot`, etc.), allocates the corresponding header/aggregate
/// via `alloc_idx`, and leaves the resulting pointer in `scratch`.
fn emit_cabi_materialise(
    ops: &CabiMaterialisationOps,
    scratch: u32,
    i: &mut wasm_encoder::InstructionSink<'_>,
    alloc_idx: u32,
    mem_arg: &impl Fn(u32) -> wasm_encoder::MemArg,
) -> Result<(), ModuleLowerError> {
    use crate::layout::{
        ARRAY_HEADER_LEN_OFFSET, ARRAY_HEADER_PTR_OFFSET, ARRAY_HEADER_SIZE, STRING_HEADER_SIZE,
        STRING_LEN_OFFSET, STRING_PTR_OFFSET,
    };
    match ops {
        CabiMaterialisationOps::StringFromSlots { ptr_slot, len_slot }
        | CabiMaterialisationOps::ListFromSlots { ptr_slot, len_slot } => {
            let header_size = match ops {
                CabiMaterialisationOps::StringFromSlots { .. } => STRING_HEADER_SIZE,
                CabiMaterialisationOps::ListFromSlots { .. }
                | CabiMaterialisationOps::Struct { .. }
                | CabiMaterialisationOps::Variant { .. } => ARRAY_HEADER_SIZE,
            };
            let header_size_signed = i32::try_from(header_size).unwrap_or(8);
            i.i32_const(header_size_signed)
                .call(alloc_idx)
                .local_set(scratch);
            i.local_get(scratch)
                .local_get(*ptr_slot)
                .i32_store(mem_arg(STRING_PTR_OFFSET));
            i.local_get(scratch)
                .local_get(*len_slot)
                .i32_store(mem_arg(STRING_LEN_OFFSET));
            if matches!(ops, CabiMaterialisationOps::ListFromSlots { .. }) {
                // Array header is { ptr, len, cap }; we fill ptr/len
                // from the canonical (ptr, len) and mirror len into
                // cap so consumers reading the cap see a coherent
                // value.
                let cap_offset = ARRAY_HEADER_LEN_OFFSET + 4;
                i.local_get(scratch)
                    .local_get(*len_slot)
                    .i32_store(mem_arg(cap_offset));
                let _ = ARRAY_HEADER_PTR_OFFSET; // referenced for clarity
            }
        }
        CabiMaterialisationOps::Struct { size, fields } => {
            let size_signed = i32::try_from(*size).unwrap_or(0);
            i.i32_const(size_signed).call(alloc_idx).local_set(scratch);
            for f in fields {
                match &f.value {
                    CabiFieldValue::DirectSlot { slot, valtype } => {
                        i.local_get(scratch).local_get(*slot);
                        match valtype {
                            ValType::I32 => i.i32_store(mem_arg(f.offset)),
                            ValType::I64 => i.i64_store(mem_arg(f.offset)),
                            ValType::F32 => i.f32_store(mem_arg(f.offset)),
                            ValType::F64 => i.f64_store(mem_arg(f.offset)),
                            ValType::V128 | ValType::Ref(_) => {
                                return Err(ModuleLowerError::TypeMap(
                                    TypeMapError::NotYetSupported {
                                        kind: format!(
                                            "canonical-ABI store of unsupported valtype {valtype:?}"
                                        ),
                                    },
                                ));
                            }
                        };
                    }
                    CabiFieldValue::MaterialisedScratch {
                        scratch: sub_scratch,
                        ops: sub_ops,
                    } => {
                        emit_cabi_materialise(sub_ops, *sub_scratch, i, alloc_idx, mem_arg)?;
                        i.local_get(scratch)
                            .local_get(*sub_scratch)
                            .i32_store(mem_arg(f.offset));
                    }
                }
            }
        }
        CabiMaterialisationOps::Variant {
            size,
            tag_slot,
            payload_offset,
            payload_slots,
        } => {
            let size_signed = i32::try_from(*size).unwrap_or(0);
            i.i32_const(size_signed).call(alloc_idx).local_set(scratch);
            i.local_get(scratch)
                .local_get(*tag_slot)
                .i32_store(mem_arg(0));
            for (idx, slot) in payload_slots.iter().enumerate() {
                let off = payload_offset
                    .checked_add(u32::try_from(idx).unwrap_or(0).saturating_mul(4))
                    .unwrap_or(*payload_offset);
                i.local_get(scratch)
                    .local_get(*slot)
                    .i32_store(mem_arg(off));
            }
        }
    }
    Ok(())
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
    use wasm_encoder::Function;

    // Pass 1: walk the params once to compute (a) the wrapper's
    // canonical-ABI valtype list and (b) the per-param "input
    // slots" — the indices into the wrapper's locals where each
    // canonical chunk lands. The materialiser walks
    // `f.params` again in pass 2 and consumes those slots.
    let alloc_idx = builder.declare_bump_allocator();
    let mut param_valtypes: Vec<ValType> = Vec::with_capacity(f.params.len());
    let mut param_slots: Vec<ParamSlots> = Vec::with_capacity(f.params.len());
    for p in &f.params {
        let ty =
            p.ty.as_ref()
                .ok_or_else(|| ModuleLowerError::MissingParamType {
                    function: f.name.clone(),
                    name: p.name.clone(),
                })?;
        let flat_start =
            u32::try_from(param_valtypes.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        let flat = flatten_canonical_abi(ty, module)?;
        let flat_count =
            u32::try_from(flat.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
        param_valtypes.extend(flat);
        param_slots.push(ParamSlots {
            flat_start,
            flat_count,
        });
    }
    let result_valtypes = body_result_types(f.return_type.as_ref())?;

    // Track scratch locals: one i32 per "materialisation" we
    // perform inside the wrapper. Each materialised value
    // (string header, struct, enum cell, array header) needs an
    // i32 scratch to hold its allocator-returned pointer between
    // emission and the final `call`.
    let scratch_base =
        u32::try_from(param_valtypes.len()).map_err(|_| ModuleLowerError::TooManyFunctions)?;
    let mut scratch_count: u32 = 0;

    // Walk params to build per-param plans.
    let mut plans: Vec<CabiPlan> = Vec::with_capacity(f.params.len());
    for (i, p) in f.params.iter().enumerate() {
        let ty =
            p.ty.as_ref()
                .ok_or_else(|| ModuleLowerError::MissingParamType {
                    function: f.name.clone(),
                    name: p.name.clone(),
                })?;
        let slots = param_slots
            .get(i)
            .ok_or(ModuleLowerError::TooManyFunctions)?;
        let mut cursor = slots.flat_start;
        // For "single-i32" primitive-only params we still want a
        // direct-slot pass-through. Recursive logic in
        // `build_cabi_field_value` handles the general case.
        let value =
            build_cabi_field_value(ty, &mut cursor, &mut scratch_count, scratch_base, module)?;
        // Sanity: we should have consumed exactly the param's
        // canonical flat slot count.
        let consumed = cursor.saturating_sub(slots.flat_start);
        if consumed != slots.flat_count {
            return Err(ModuleLowerError::TypeMap(TypeMapError::NotYetSupported {
                kind: format!(
                    "canonical-ABI flatten/build mismatch for param `{}` of {ty:?}: consumed {} vs expected {}",
                    p.name, consumed, slots.flat_count
                ),
            }));
        }
        plans.push(match value {
            CabiFieldValue::DirectSlot { slot, .. } => CabiPlan::Direct { slot },
            CabiFieldValue::MaterialisedScratch { scratch, ops } => {
                CabiPlan::Materialised { scratch, ops: *ops }
            }
        });
    }

    let mut body = Function::new(if scratch_count > 0 {
        vec![(scratch_count, ValType::I32)]
    } else {
        Vec::new()
    });

    let mem_arg = |offset: u32| wasm_encoder::MemArg {
        offset: u64::from(offset),
        align: 2,
        memory_index: crate::module::MEMORY_INDEX,
    };

    {
        let mut i = body.instructions();
        // First pass: emit all materialisations so each plan's
        // scratch holds the materialised pointer.
        for plan in &plans {
            if let CabiPlan::Materialised { scratch, ops } = plan {
                emit_cabi_materialise(ops, *scratch, &mut i, alloc_idx, &mem_arg)?;
            }
        }
        // Second pass: push each param's "single value" onto the
        // stack in declaration order, then forward to the inner.
        for plan in &plans {
            match plan {
                CabiPlan::Direct { slot } => {
                    i.local_get(*slot);
                }
                CabiPlan::Materialised { scratch, .. } => {
                    i.local_get(*scratch);
                }
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
    lower_params_with_self(f, None)
}

/// Like [`lower_params`] but injects `Struct(self_struct_id)` as the
/// type of a leading `self` param whose `ty` is `None`. The
/// formalang IR records inherent-impl `self` parameters with the
/// type left implicit — the impl target carries it. The caller
/// (`emit_function`) supplies the impl's self-struct id so the
/// param-type loop here doesn't need to thread it through every
/// generic-resolution path.
fn lower_params_with_self(
    f: &IrFunction,
    self_struct_id: Option<StructId>,
) -> Result<(Vec<ValType>, Vec<ParamBinding>), ModuleLowerError> {
    let self_ty = self_struct_id.map(ResolvedType::Struct);
    lower_params_with_self_ty(f, self_ty.as_ref())
}

/// Generalised version of [`lower_params_with_self`] that accepts
/// any [`ResolvedType`] for the implicit self. Used by the impl
/// walker to handle `extern impl <Enum>` (where the receiver is
/// `Enum(id)`) on top of the struct path.
fn lower_params_with_self_ty(
    f: &IrFunction,
    self_ty: Option<&ResolvedType>,
) -> Result<(Vec<ValType>, Vec<ParamBinding>), ModuleLowerError> {
    let mut valtypes = Vec::with_capacity(f.params.len());
    let mut bindings = Vec::with_capacity(f.params.len());

    for (idx, param) in f.params.iter().enumerate() {
        let injected_self = if idx == 0 && param.name == "self" && param.ty.is_none() {
            self_ty
        } else {
            None
        };
        let ty = param.ty.as_ref().or(injected_self).ok_or_else(|| {
            ModuleLowerError::MissingParamType {
                function: f.name.clone(),
                name: param.name.clone(),
            }
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
