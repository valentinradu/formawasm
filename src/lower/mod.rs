//! Per-expression lowering from formalang IR to core-Wasm
//! instructions.
//!
//! Each `lower_*` function appends instructions to the caller's
//! `InstructionSink` and assumes the surrounding stack discipline is
//! maintained by the caller. Helpers do not emit a closing `end` —
//! that's the function-body framer's job.
//!
//! The module is split into submodules per IR variant family, plus
//! shared types ([`LowerError`], [`BindingMap`], [`FunctionMap`],
//! [`LowerContext`]) and the recursive [`lower_expr`] dispatcher.

pub(crate) mod aggregate;
mod binary_op;
mod block;
mod call;
mod control;
mod literal;
mod optional;
mod reference;
mod unary_op;

use std::cell::Cell;
use std::collections::HashMap;

use formalang::ast::PrimitiveType;
use formalang::ir::{
    BindingId, FunctionId, ImplId, IrExpr, IrModule, MethodIdx, ResolvedType, StructId, TraitId,
};
use thiserror::Error;
use wasm_encoder::{InstructionSink, ValType};

use crate::layout::LayoutError;
use crate::types::TypeMapError;

pub use aggregate::{
    lower_array, lower_closure_ref, lower_dict_access, lower_dict_literal, lower_enum_inst,
    lower_field_access, lower_range, lower_self_field_ref, lower_struct_inst, lower_tuple,
};
pub use binary_op::lower_binary_op;
pub use block::{lower_block, lower_function_body, lower_function_body_in_module};
// MethodMap is exported for the production module-lowering pass that
// consumes it as a coordinated input alongside FunctionMap.
pub use call::{lower_call_closure, lower_function_call, lower_method_call};
pub use control::{lower_for, lower_if, lower_match};
pub use literal::lower_literal;
pub use reference::{lower_let_ref, lower_reference};
pub use unary_op::lower_unary_op;

/// Errors produced by the expression-lowering helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LowerError {
    /// The expression variant or operand shape is in scope for the
    /// backend but not yet implemented in this phase.
    #[error("lowering for {what} is not yet implemented")]
    NotYetImplemented {
        /// Short tag describing the missing variant or shape.
        what: String,
    },

    /// A numeric literal whose payload is incompatible with its
    /// declared type — e.g. an `I32`-typed literal whose value is out
    /// of range or carries a float-syntax payload. Semantic analysis
    /// should catch these upstream; the variant exists so we never
    /// silently emit a wrong constant.
    #[error("numeric literal {payload} cannot be lowered as {target:?}")]
    LiteralOutOfRange {
        /// String form of the offending payload (e.g. `"3.14"` or `"2147483648"`).
        payload: String,
        /// The declared target primitive type.
        target: PrimitiveType,
    },

    /// A literal whose type doesn't match its kind, e.g. a `String`
    /// literal carrying a non-`Primitive(String)` `ty`.
    #[error("literal kind {kind} does not match declared type {ty:?}")]
    LiteralTypeMismatch {
        /// Tag for the literal kind (`"Boolean"`, `"Number(integer)"`, …).
        kind: String,
        /// The declared resolved type.
        ty: ResolvedType,
    },

    /// A reference / `LetRef` carries a `BindingId` that the caller's
    /// [`BindingMap`] does not know about. Indicates a lowering pass
    /// failed to register the binding before walking expressions that
    /// reference it.
    #[error("BindingId {0:?} is not registered in the binding map")]
    UnknownBinding(BindingId),

    /// A reference's `ReferenceTarget` is `Unresolved`. Means
    /// `ResolveReferencesPass` did not run before the backend.
    #[error(
        "Reference target is Unresolved — ResolveReferencesPass must run before WasmBackend::generate"
    )]
    UnresolvedReference,

    /// A binary or unary operator was applied to operand type(s) the
    /// backend does not support — either a fundamentally invalid combo
    /// (e.g. `And` on `I32`) or a deferred case (e.g. arithmetic on
    /// `String`, which lives in Phase 2).
    #[error("operator {op} on {operand:?} is not supported in this phase")]
    UnsupportedOperator {
        /// Operator name (e.g. `"Add"`, `"Range"`, `"Mod"`).
        op: String,
        /// Operand primitive type at the offending site.
        operand: PrimitiveType,
    },

    /// A type appearing on a parameter or `let` binding could not be
    /// mapped to a wasm value type — typically because the lowering
    /// for that type lives in a later phase.
    #[error(transparent)]
    TypeMap(#[from] TypeMapError),

    /// A `let`-binding inside a block has a unit/`Never` type that
    /// can't be assigned to a wasm local. Indicates an upstream
    /// invariant violation.
    #[error("let binding '{name}' has zero-sized type {ty:?} and cannot be stored in a wasm local")]
    ZeroSizedLetBinding {
        /// Source-level name of the offending binding.
        name: String,
        /// The declared resolved type.
        ty: ResolvedType,
    },

    /// A `FunctionCall` carries a `FunctionId` that the caller's
    /// [`FunctionMap`] does not know about. Indicates the module-
    /// level lowering pass failed to register this function before
    /// walking call sites.
    #[error("FunctionId {0:?} is not registered in the function map")]
    UnknownFunction(FunctionId),

    /// A `FunctionCall` carries `function_id = None`. Either the
    /// resolution pass failed, or the call targets an external
    /// (cross-module) function which won't be supported until
    /// Phase 4.
    #[error("FunctionCall path {path:?} is unresolved (function_id = None)")]
    UnresolvedFunctionCall {
        /// Source-level path of the call (e.g. `["math", "sin"]`).
        path: Vec<String>,
    },

    /// A lowering needs an [`LowerContext`] field that is not set —
    /// typically the [`IrModule`] reference (for struct/enum lookups)
    /// or the bump-allocator function index. Surfaced when an
    /// aggregate lowering runs through a context built for the
    /// expression-only test path.
    #[error("LowerContext is missing the {what} field required for this lowering")]
    MissingContext {
        /// Static tag identifying the missing field.
        what: &'static str,
    },

    /// A `StructInst` carries `struct_id = None`, which means it
    /// instantiates an external (cross-module) struct. External
    /// references land in Phase 4.
    #[error("StructInst targets an external struct (struct_id = None) — Phase 4")]
    ExternalStructInst,

    /// A struct's layout could not be planned — see the wrapped
    /// [`LayoutError`] for the underlying cause.
    #[error(transparent)]
    Layout(#[from] LayoutError),

    /// A `FieldAccess` references a field index past the end of its
    /// containing struct's `fields` vector. Indicates an upstream
    /// invariant violation.
    #[error(
        "field index {field_idx} is out of range for struct '{struct_name}' ({field_count} fields)"
    )]
    FieldIndexOutOfRange {
        /// Source-level struct name.
        struct_name: String,
        /// Number of fields actually in the struct.
        field_count: usize,
        /// The offending field index.
        field_idx: u32,
    },

    /// A `FieldAccess`'s `object` expression has a non-aggregate type
    /// (e.g. a primitive). This means the type-checker accepted a
    /// field access on something that has no fields.
    #[error("field access on non-aggregate type {ty:?} — type-checker should have rejected this")]
    FieldAccessOnNonAggregate {
        /// The offending object's resolved type.
        ty: ResolvedType,
    },

    /// A `StructInst` was given a `struct_id` that the module's
    /// `structs` vector does not contain. Indicates corrupt IR.
    #[error("StructId {0:?} is not present in IrModule.structs")]
    UnknownStruct(formalang::ir::StructId),

    /// An `EnumInst` carries `enum_id = None`, which means it
    /// instantiates an external (cross-module) enum. External
    /// references land in Phase 4.
    #[error("EnumInst targets an external enum (enum_id = None) — Phase 4")]
    ExternalEnumInst,

    /// An `EnumInst` was given an `enum_id` that the module's
    /// `enums` vector does not contain. Indicates corrupt IR.
    #[error("EnumId {0:?} is not present in IrModule.enums")]
    UnknownEnum(formalang::ir::EnumId),

    /// An `EnumInst` references a variant that the resolved enum
    /// definition does not contain. Indicates an upstream invariant
    /// violation.
    #[error("variant '{variant}' is not declared on enum '{enum_name}'")]
    UnknownVariant {
        /// Containing enum name.
        enum_name: String,
        /// Source-level variant name.
        variant: String,
    },

    /// A `SelfFieldRef` was lowered without a [`LowerContext::self_struct_id`]
    /// set. Means the function is not a method of any struct, but its
    /// body uses `self.field` — an upstream invariant violation.
    #[error("SelfFieldRef encountered in a function with no `self` struct context")]
    MissingSelfStruct,

    /// A `MethodCall` with `DispatchKind::Static { impl_id }` and
    /// `method_idx` whose pair is not present in the module's
    /// [`MethodMap`]. Indicates the impl was never walked or the
    /// indices are inconsistent.
    #[error("static method (impl {impl_id:?}, method {method_idx:?}) is not registered")]
    UnknownMethod {
        /// The offending impl id.
        impl_id: ImplId,
        /// The offending method index inside that impl.
        method_idx: MethodIdx,
    },

    /// A `MethodCall` with `DispatchKind::Virtual { .. }` was
    /// encountered. Virtual dispatch lands in Phase 3.
    #[error("virtual method dispatch is not yet supported (Phase 3)")]
    VirtualMethodCall,

    /// A virtual `MethodCall` whose `(trait_id, impl_target)` pair
    /// has no vtable registered. Indicates the module-level pre-walk
    /// missed the impl, or the receiver type is one the layout
    /// planner doesn't recognise as a vtable target.
    #[error("no vtable registered for trait {trait_id:?} on target tag={}, id={}", target.0, target.1)]
    UnknownVtable {
        /// The trait whose vtable is missing.
        trait_id: TraitId,
        /// The impl-target key the lookup was keyed on.
        target: ImplTargetKey,
    },

    /// A virtual `MethodCall` whose `(trait_id, method_idx)` pair has
    /// no `call_indirect` type-index registered. Indicates the module-
    /// level pre-walk missed the trait method.
    #[error("no call_indirect type registered for trait {trait_id:?} method {method_idx:?}")]
    UnknownVirtualMethodType {
        /// Trait id.
        trait_id: TraitId,
        /// Method index inside the trait's `methods` vector.
        method_idx: MethodIdx,
    },

    /// A virtual `MethodCall` whose receiver type cannot be resolved
    /// to an `ImplTarget`. Phase 3 supports concrete struct / enum
    /// receivers; trait-object receivers stay rejected (semantic
    /// analysis bans `Trait` as a value).
    #[error("virtual method dispatch on receiver type {ty:?} is not supported")]
    UnsupportedVirtualReceiver {
        /// The offending receiver's resolved type.
        ty: ResolvedType,
    },
}

/// Mapping from a module-scope `FunctionId` to its wasm function
/// index. Built by the module-level lowering pass before walking
/// any function bodies.
#[derive(Debug, Default, Clone)]
pub struct FunctionMap {
    by_id: HashMap<FunctionId, u32>,
}

impl FunctionMap {
    /// Build an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a function's wasm index.
    pub fn insert(&mut self, id: FunctionId, wasm_index: u32) {
        self.by_id.insert(id, wasm_index);
    }

    /// Look up a function's wasm index, or `None` when missing.
    #[must_use]
    pub fn get(&self, id: FunctionId) -> Option<u32> {
        self.by_id.get(&id).copied()
    }

    /// Number of functions recorded so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Mapping from `(ImplId, MethodIdx)` to the wasm function index of the emitted method body.
///
/// Built by the module-level lowering pass alongside [`FunctionMap`]
/// so [`IrExpr::MethodCall`] dispatches resolve without re-walking
/// `IrModule.impls`.
#[derive(Debug, Default, Clone)]
pub struct MethodMap {
    by_id: HashMap<(ImplId, MethodIdx), u32>,
}

impl MethodMap {
    /// Build an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a method's wasm function index.
    pub fn insert(&mut self, key: (ImplId, MethodIdx), wasm_index: u32) {
        self.by_id.insert(key, wasm_index);
    }

    /// Iterate over `(key, wasm_index)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (&(ImplId, MethodIdx), &u32)> {
        self.by_id.iter()
    }

    /// Look up a method's wasm function index.
    #[must_use]
    pub fn get(&self, key: (ImplId, MethodIdx)) -> Option<u32> {
        self.by_id.get(&key).copied()
    }

    /// Number of methods recorded so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Mapping from a function-local `BindingId` (parameters + `let`
/// bindings) to its wasm-local index.
///
/// In wasm, parameters occupy local indices `0..N`; user `let`
/// bindings get indices `N..` in declaration order. The function
/// body's lowering pass owns this map and inserts entries before
/// walking expressions that reference them.
#[derive(Debug, Default, Clone)]
pub struct BindingMap {
    by_id: HashMap<BindingId, u32>,
}

impl BindingMap {
    /// Build an empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a binding's wasm-local index.
    pub fn insert(&mut self, id: BindingId, local_index: u32) {
        self.by_id.insert(id, local_index);
    }

    /// Look up a binding's wasm-local index, or `None` when missing.
    #[must_use]
    pub fn get(&self, id: BindingId) -> Option<u32> {
        self.by_id.get(&id).copied()
    }

    /// Number of bindings recorded so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the map is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

/// Context passed through the recursive lowering walkers.
///
/// `bindings` and `functions` are always populated. `module`,
/// `bump_allocator`, and `scratch_locals` are populated by the
/// production module-level lowering pass; expression-only tests can
/// leave them unset and the affected lowerings will surface a
/// [`LowerError::MissingContext`] the moment they're invoked.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct LowerContext<'a> {
    /// Function-local binding indices (params + lets).
    pub bindings: &'a BindingMap,
    /// Module-scope function indices.
    pub functions: &'a FunctionMap,
    /// Method indices keyed on `(ImplId, MethodIdx)`. Empty by
    /// default; the production module-lowering pass populates it
    /// before walking method-bearing function bodies.
    pub methods: Option<&'a MethodMap>,
    /// Reference to the IR module being lowered. Aggregate lowerings
    /// (`StructInst`, `FieldAccess`, `EnumInst`, `Match`, …) need it
    /// to look up struct / enum definitions for layout planning.
    pub module: Option<&'a IrModule>,
    /// Wasm function index of the bump-allocator helper. Aggregate
    /// constructors call this to reserve linear-memory bytes.
    pub bump_allocator: Option<u32>,
    /// Per-type counters that hand out fresh wasm-local indices
    /// reserved as scratch slots. The function-body pre-walk has
    /// already extended `Function::new(locals)` to include these
    /// regions, one per wasm value type; the counters start at each
    /// region's base and increment per scratch slot consumed in
    /// lowering order.
    pub scratch_locals: Option<&'a ScratchAllocator>,
    /// Struct that defines the enclosing impl, if the function being
    /// lowered is a method. `SelfFieldRef` lowering plans this
    /// struct's layout to translate field-name accesses into linear-
    /// memory loads through the implicit `self: i32` parameter at
    /// wasm-local 0.
    pub self_struct_id: Option<StructId>,
    /// Wasm `Table` index of the funcref table declared by the module
    /// builder for indirect closure invocation. `None` when the module
    /// has no closure-callable functions; `lower_call_closure` and
    /// `lower_closure_ref` surface
    /// [`LowerError::MissingContext`] on this field if a closure
    /// value or call site reaches them without the table available.
    pub closure_table: Option<u32>,
    /// Maps a closure-callable function's [`FunctionId`] to its
    /// element index inside [`Self::closure_table`]. The index is
    /// what gets stored in a closure value's funcref slot — the slot
    /// number `call_indirect` looks up at runtime, not the wasm
    /// function index.
    pub closure_funcref_indices: Option<&'a HashMap<FunctionId, u32>>,
    /// Maps a closure's `ResolvedType::Closure { ... }` to the wasm
    /// type-section index for its `call_indirect` signature. The
    /// signature prepends one i32 (`env_ptr`) to the closure's
    /// declared parameters; the lifted top-level function takes the
    /// env struct as its first argument.
    pub closure_type_indices: Option<&'a HashMap<ResolvedType, u32>>,
    /// Compile-time string-literal pool. Maps each interned literal
    /// to the byte offset of its 8-byte `{ ptr, len }` header inside
    /// the wasm data segment. `lower_literal` for `Literal::String`
    /// reads the offset from this map and emits an `i32.const` of it
    /// — the runtime value of a string literal is just a pointer to
    /// its static header.
    pub string_pool: Option<&'a HashMap<String, u32>>,
    /// Wasm function index of the `__str_eq` runtime helper. Routed
    /// through here so `BinaryOp::Eq` / `Ne` lowerings on `String`
    /// operands can `call` it without a separate module-aware lookup.
    pub str_eq: Option<u32>,
    /// Wasm function index of the `__str_concat` runtime helper. Used
    /// by `BinaryOp::Add` lowerings on `String` operands.
    pub str_concat: Option<u32>,
    /// Wasm `Table` index of the method funcref table declared by the
    /// module builder for virtual trait-method dispatch. `None` when
    /// the module has no trait impls; `lower_method_call` surfaces
    /// [`LowerError::MissingContext`] on this field if a virtual call
    /// site reaches it without the table available.
    pub method_table: Option<u32>,
    /// Maps `(trait_id, impl_target_key)` to the absolute byte offset
    /// of the matching vtable inside the static-data segment. The
    /// cell at `vtable_offset + method_idx * VTABLE_SLOT_SIZE` stores
    /// the funcref-table slot index of the method body.
    pub vtable_offsets: Option<&'a HashMap<(TraitId, ImplTargetKey), u32>>,
    /// Maps `(trait_id, method_idx)` to the wasm type-section index
    /// for that method's `call_indirect` signature.
    pub virtual_call_type_indices: Option<&'a HashMap<(TraitId, MethodIdx), u32>>,
}

impl<'a> LowerContext<'a> {
    /// Bundle the maps a walker needs. `module`, `bump_allocator`,
    /// and `scratch_locals` default to `None`; use the matching
    /// `with_*` setters to attach them when the lowering path needs
    /// aggregates.
    #[must_use]
    pub const fn new(bindings: &'a BindingMap, functions: &'a FunctionMap) -> Self {
        Self {
            bindings,
            functions,
            methods: None,
            module: None,
            bump_allocator: None,
            scratch_locals: None,
            self_struct_id: None,
            closure_table: None,
            closure_funcref_indices: None,
            closure_type_indices: None,
            string_pool: None,
            str_eq: None,
            str_concat: None,
            method_table: None,
            vtable_offsets: None,
            virtual_call_type_indices: None,
        }
    }

    /// Attach the method-index map.
    #[must_use]
    pub const fn with_methods(mut self, methods: &'a MethodMap) -> Self {
        self.methods = Some(methods);
        self
    }

    /// Attach the IR-module reference used for struct/enum lookups.
    #[must_use]
    pub const fn with_module(mut self, module: &'a IrModule) -> Self {
        self.module = Some(module);
        self
    }

    /// Attach the bump-allocator helper's wasm function index.
    #[must_use]
    pub const fn with_bump_allocator(mut self, idx: u32) -> Self {
        self.bump_allocator = Some(idx);
        self
    }

    /// Attach the typed scratch-local allocator.
    #[must_use]
    pub const fn with_scratch_locals(mut self, allocator: &'a ScratchAllocator) -> Self {
        self.scratch_locals = Some(allocator);
        self
    }

    /// Attach the enclosing impl's struct id for `SelfFieldRef`
    /// lookups.
    #[must_use]
    pub const fn with_self_struct_id(mut self, id: StructId) -> Self {
        self.self_struct_id = Some(id);
        self
    }

    /// Attach the funcref-table index for indirect closure
    /// invocation. The table is declared and populated by the module
    /// builder before any function bodies are lowered.
    #[must_use]
    pub const fn with_closure_table(mut self, idx: u32) -> Self {
        self.closure_table = Some(idx);
        self
    }

    /// Attach the closure-funcref index map. Maps a closure-callable
    /// function's [`FunctionId`] to its element index inside the
    /// funcref table.
    #[must_use]
    pub const fn with_closure_funcref_indices(
        mut self,
        indices: &'a HashMap<FunctionId, u32>,
    ) -> Self {
        self.closure_funcref_indices = Some(indices);
        self
    }

    /// Attach the closure-type index map. Maps a closure's
    /// `ResolvedType::Closure { ... }` to the wasm type-section index
    /// for its `call_indirect` signature.
    #[must_use]
    pub const fn with_closure_type_indices(
        mut self,
        indices: &'a HashMap<ResolvedType, u32>,
    ) -> Self {
        self.closure_type_indices = Some(indices);
        self
    }

    /// Attach the compile-time string-literal pool. Required for
    /// lowering `Literal::String` — each literal's runtime value is
    /// a `i32.const` of its header offset, which the pool's lookup
    /// table provides.
    #[must_use]
    pub const fn with_string_pool(mut self, pool: &'a HashMap<String, u32>) -> Self {
        self.string_pool = Some(pool);
        self
    }

    /// Attach the wasm function index of the `__str_eq` runtime
    /// helper. Required for lowering `BinaryOp::Eq` / `Ne` on
    /// `String` operands.
    #[must_use]
    pub const fn with_str_eq(mut self, idx: u32) -> Self {
        self.str_eq = Some(idx);
        self
    }

    /// Wasm function index of the `__str_eq` helper or surface
    /// [`LowerError::MissingContext`] if unset.
    pub fn str_eq_index(&self) -> Result<u32, LowerError> {
        self.str_eq
            .ok_or(LowerError::MissingContext { what: "str_eq" })
    }

    /// Attach the wasm function index of the `__str_concat` runtime
    /// helper. Required for lowering `BinaryOp::Add` on `String`
    /// operands.
    #[must_use]
    pub const fn with_str_concat(mut self, idx: u32) -> Self {
        self.str_concat = Some(idx);
        self
    }

    /// Wasm function index of the `__str_concat` helper or surface
    /// [`LowerError::MissingContext`] if unset.
    pub fn str_concat_index(&self) -> Result<u32, LowerError> {
        self.str_concat
            .ok_or(LowerError::MissingContext { what: "str_concat" })
    }

    /// Look up the header offset of an interned string literal, or
    /// surface [`LowerError::MissingContext`] when the pool is unset
    /// and [`LowerError::NotYetImplemented`] when the literal is not
    /// pre-registered.
    pub fn string_header_offset(&self, text: &str) -> Result<u32, LowerError> {
        let pool = self.string_pool.ok_or(LowerError::MissingContext {
            what: "string_pool",
        })?;
        pool.get(text)
            .copied()
            .ok_or_else(|| LowerError::NotYetImplemented {
                what: format!("string literal {text:?} was not pre-interned"),
            })
    }

    /// Wasm-table index of the closure funcref table or surface
    /// [`LowerError::MissingContext`] if unset.
    pub fn closure_table_index(&self) -> Result<u32, LowerError> {
        self.closure_table.ok_or(LowerError::MissingContext {
            what: "closure_table",
        })
    }

    /// Wasm type-section index for a closure-call signature. Returns
    /// [`LowerError::MissingContext`] when the surrounding lowering
    /// pass didn't attach the map, or [`LowerError::NotYetImplemented`]
    /// when the call site uses a closure type the pass didn't pre-
    /// register (typically a sign that the pre-walk missed it).
    pub fn closure_type_index(&self, ty: &ResolvedType) -> Result<u32, LowerError> {
        let map = self
            .closure_type_indices
            .ok_or(LowerError::MissingContext {
                what: "closure_type_indices",
            })?;
        map.get(ty)
            .copied()
            .ok_or_else(|| LowerError::NotYetImplemented {
                what: format!("call_indirect type signature for {ty:?} (not pre-registered)"),
            })
    }

    /// Element index inside the closure funcref table for a function
    /// id. Returns [`LowerError::MissingContext`] when the map is
    /// unset, or [`LowerError::UnknownFunction`] when the function
    /// isn't closure-callable.
    pub fn closure_funcref_index(&self, id: FunctionId) -> Result<u32, LowerError> {
        let map = self
            .closure_funcref_indices
            .ok_or(LowerError::MissingContext {
                what: "closure_funcref_indices",
            })?;
        map.get(&id).copied().ok_or(LowerError::UnknownFunction(id))
    }

    /// Attach the method funcref-table index. Required for lowering
    /// any [`formalang::ir::DispatchKind::Virtual`] call site.
    #[must_use]
    pub const fn with_method_table(mut self, idx: u32) -> Self {
        self.method_table = Some(idx);
        self
    }

    /// Attach the per-`(trait_id, impl_target_key)` vtable offset
    /// table.
    #[must_use]
    pub const fn with_vtable_offsets(
        mut self,
        offsets: &'a HashMap<(TraitId, ImplTargetKey), u32>,
    ) -> Self {
        self.vtable_offsets = Some(offsets);
        self
    }

    /// Attach the per-trait-method `call_indirect` type-index map.
    #[must_use]
    pub const fn with_virtual_call_type_indices(
        mut self,
        indices: &'a HashMap<(TraitId, MethodIdx), u32>,
    ) -> Self {
        self.virtual_call_type_indices = Some(indices);
        self
    }

    /// Method funcref table index, or surface
    /// [`LowerError::MissingContext`] if unset.
    pub fn method_table_index(&self) -> Result<u32, LowerError> {
        self.method_table.ok_or(LowerError::MissingContext {
            what: "method_table",
        })
    }

    /// Look up the absolute byte offset of the
    /// `(trait_id, impl_target_key)` vtable, or surface a typed
    /// error when the map is unset / the key is missing.
    pub fn vtable_offset(
        &self,
        trait_id: TraitId,
        target: ImplTargetKey,
    ) -> Result<u32, LowerError> {
        let map = self.vtable_offsets.ok_or(LowerError::MissingContext {
            what: "vtable_offsets",
        })?;
        map.get(&(trait_id, target))
            .copied()
            .ok_or(LowerError::UnknownVtable { trait_id, target })
    }

    /// Look up the `call_indirect` type-section index for a trait
    /// method, or surface a typed error if missing.
    pub fn virtual_call_type_index(
        &self,
        trait_id: TraitId,
        method_idx: MethodIdx,
    ) -> Result<u32, LowerError> {
        let map = self
            .virtual_call_type_indices
            .ok_or(LowerError::MissingContext {
                what: "virtual_call_type_indices",
            })?;
        map.get(&(trait_id, method_idx))
            .copied()
            .ok_or(LowerError::UnknownVirtualMethodType {
                trait_id,
                method_idx,
            })
    }

    /// Borrow the IR module or surface
    /// [`LowerError::MissingContext`] if the field is unset.
    pub fn module(&self) -> Result<&'a IrModule, LowerError> {
        self.module
            .ok_or(LowerError::MissingContext { what: "module" })
    }

    /// Like [`Self::module`] but returns `None` instead of erroring
    /// when the field is unset. Useful for lowerings that can fall
    /// back to a non-module-aware behaviour (e.g. recognising
    /// prelude Optional Some-wraps).
    #[must_use]
    pub const fn module_opt(&self) -> Option<&'a IrModule> {
        self.module
    }

    /// Return the bump-allocator function index or surface
    /// [`LowerError::MissingContext`] if the field is unset.
    pub fn bump_allocator(&self) -> Result<u32, LowerError> {
        self.bump_allocator.ok_or(LowerError::MissingContext {
            what: "bump_allocator",
        })
    }

    /// Hand out the next scratch wasm-local index for a slot of type
    /// `ty`. The pre-walk that reserved these locals must have counted
    /// at least as many scratch slots of each type as the lowering
    /// walker actually visits; otherwise we'd be pointing past the end
    /// of the function's locals table.
    pub fn next_scratch_local(&self, ty: ValType) -> Result<u32, LowerError> {
        let allocator = self.scratch_locals.ok_or(LowerError::MissingContext {
            what: "scratch_locals",
        })?;
        allocator.allocate(ty)
    }
}

/// Bundle of closure-call plumbing handed from the module-level
/// lowering pass into [`lower_function_body_in_module`].
///
/// Built once per module after the funcref table has been declared
/// and populated; every function body lowered against the same
/// module shares this context.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain bundle consumed by the function-body planner"
)]
#[derive(Debug, Clone, Copy)]
pub struct ClosureCallContext<'a> {
    /// Wasm-table index of the closure funcref table.
    pub table_idx: u32,
    /// Maps each closure-callable function's [`FunctionId`] to its
    /// element index inside the table.
    pub funcref_indices: &'a HashMap<FunctionId, u32>,
    /// Maps a closure's `ResolvedType::Closure { ... }` to the wasm
    /// type-section index for the matching `call_indirect` signature.
    pub type_indices: &'a HashMap<ResolvedType, u32>,
}

/// Hashable encoding of an `ImplTarget` used as part of the vtable
/// lookup key. `tag = 0` for a struct target, `tag = 1` for an enum
/// target; the second field is the raw struct or enum id.
pub type ImplTargetKey = (u32, u32);

/// Bundle of virtual-dispatch plumbing handed from the module-level
/// lowering pass into [`lower_function_body_in_module`].
///
/// Built once per module after the per-impl method funcref table has
/// been declared and the vtable bytes seeded into the static-data
/// segment; every function body lowered against the same module
/// shares this context. Per-call lookups read from these maps to
/// resolve a [`formalang::ir::DispatchKind::Virtual`] site to a
/// concrete `(vtable_base, method_funcref_table_idx, type_idx)`
/// triple.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain bundle consumed by the function-body planner"
)]
#[derive(Debug, Clone, Copy)]
pub struct VTableContext<'a> {
    /// Wasm-table index of the method funcref table populated with
    /// every trait-impl method body.
    pub table_idx: u32,
    /// Absolute byte offset of each `(trait_id, impl_target_key)`
    /// vtable inside the static-data segment. The cell at
    /// `vtable_offset + method_idx * VTABLE_SLOT_SIZE` stores the
    /// funcref-table index of that method's body.
    pub vtable_offsets: &'a HashMap<(TraitId, ImplTargetKey), u32>,
    /// Wasm type-section index for each trait method's
    /// `call_indirect` signature.
    pub call_type_indices: &'a HashMap<(TraitId, MethodIdx), u32>,
}

/// Per-type scratch-local allocator passed by reference into [`LowerContext`].
///
/// The pre-walk reserves four contiguous regions in the wasm-locals
/// table — one per supported value type — and feeds each region's
/// `(base, count)` pair in here. `allocate(ty)` returns the next free
/// index inside the appropriate region, advancing its counter.
#[derive(Debug)]
pub struct ScratchAllocator {
    i32_base: u32,
    i32_next: Cell<u32>,
    i32_count: u32,
    i64_base: u32,
    i64_next: Cell<u32>,
    i64_count: u32,
    f32_base: u32,
    f32_next: Cell<u32>,
    f32_count: u32,
    f64_base: u32,
    f64_next: Cell<u32>,
    f64_count: u32,
}

/// Per-type `(base_index, slot_count)` pairs handed to
/// [`ScratchAllocator::new`].
#[expect(
    clippy::exhaustive_structs,
    reason = "plain layout record consumed by the function-body planner"
)]
#[derive(Debug, Clone, Copy)]
pub struct ScratchRegions {
    pub i32: (u32, u32),
    pub i64: (u32, u32),
    pub f32: (u32, u32),
    pub f64: (u32, u32),
}

impl ScratchAllocator {
    /// Build an allocator from per-type `(base, count)` regions. The
    /// caller is responsible for laying these regions out contiguously
    /// in the function's wasm-locals table.
    #[must_use]
    pub const fn new(regions: ScratchRegions) -> Self {
        let (i32_base, i32_count) = regions.i32;
        let (i64_base, i64_count) = regions.i64;
        let (f32_base, f32_count) = regions.f32;
        let (f64_base, f64_count) = regions.f64;
        Self {
            i32_base,
            i32_next: Cell::new(0),
            i32_count,
            i64_base,
            i64_next: Cell::new(0),
            i64_count,
            f32_base,
            f32_next: Cell::new(0),
            f32_count,
            f64_base,
            f64_next: Cell::new(0),
            f64_count,
        }
    }

    /// Hand out the next index for a scratch slot of type `ty`.
    pub fn allocate(&self, ty: ValType) -> Result<u32, LowerError> {
        let (next, base, capacity, tag) = match ty {
            ValType::I32 => (&self.i32_next, self.i32_base, self.i32_count, "i32"),
            ValType::I64 => (&self.i64_next, self.i64_base, self.i64_count, "i64"),
            ValType::F32 => (&self.f32_next, self.f32_base, self.f32_count, "f32"),
            ValType::F64 => (&self.f64_next, self.f64_base, self.f64_count, "f64"),
            ValType::V128 | ValType::Ref(_) => {
                return Err(LowerError::NotYetImplemented {
                    what: format!("scratch local of type {ty:?}"),
                });
            }
        };
        let used = next.get();
        if used >= capacity {
            return Err(LowerError::NotYetImplemented {
                what: format!(
                    "scratch-local pre-walk under-counted {tag} slots (reserved {capacity}, asked for {})",
                    used.saturating_add(1)
                ),
            });
        }
        let local_index = base
            .checked_add(used)
            .ok_or_else(|| LowerError::NotYetImplemented {
                what: format!("scratch local index overflow for {tag}"),
            })?;
        next.set(used.saturating_add(1));
        Ok(local_index)
    }
}

/// Top-level expression dispatcher.
///
/// Each variant either funnels into its dedicated `lower_*` helper or
/// surfaces a typed `NotYetImplemented` error tagged with the variant
/// name. The caller is responsible for the surrounding stack
/// discipline (block types, end markers, function frames).
pub fn lower_expr(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    match expr {
        IrExpr::Literal { .. } => lower_literal(expr, sink, ctx),
        IrExpr::Reference { .. } => lower_reference(expr, sink, ctx),
        IrExpr::LetRef { .. } => lower_let_ref(expr, sink, ctx),
        IrExpr::BinaryOp { .. } => lower_binary_op(expr, sink, ctx),
        IrExpr::UnaryOp { .. } => lower_unary_op(expr, sink, ctx),
        IrExpr::Block { .. } => lower_block(expr, sink, ctx),
        IrExpr::FunctionCall { .. } => lower_function_call(expr, sink, ctx),
        IrExpr::CallClosure { .. } => lower_call_closure(expr, sink, ctx),
        IrExpr::If { .. } => lower_if(expr, sink, ctx),

        IrExpr::StructInst { .. } => lower_struct_inst(expr, sink, ctx),
        IrExpr::FieldAccess { .. } => lower_field_access(expr, sink, ctx),
        IrExpr::Tuple { .. } => lower_tuple(expr, sink, ctx),
        IrExpr::EnumInst { .. } => lower_enum_inst(expr, sink, ctx),
        IrExpr::Match { .. } => lower_match(expr, sink, ctx),
        IrExpr::SelfFieldRef { .. } => lower_self_field_ref(expr, sink, ctx),
        IrExpr::MethodCall { .. } => lower_method_call(expr, sink, ctx),
        IrExpr::ClosureRef { .. } => lower_closure_ref(expr, sink, ctx),
        IrExpr::Array { .. } => lower_array(expr, sink, ctx),
        IrExpr::For { .. } => lower_for(expr, sink, ctx),

        IrExpr::DictAccess { .. } => lower_dict_access(expr, sink, ctx),
        IrExpr::DictLiteral { .. } => lower_dict_literal(expr, sink, ctx),

        IrExpr::Closure { .. } => Err(LowerError::NotYetImplemented {
            what: format!("IrExpr::{}", expr_variant_name(expr)),
        }),
    }
}

/// String tag for an `IrExpr` variant, used in `NotYetImplemented`
/// diagnostics so the surfaced error names the unsupported variant.
pub(crate) const fn expr_variant_name(expr: &IrExpr) -> &'static str {
    match expr {
        IrExpr::Literal { .. } => "Literal",
        IrExpr::Reference { .. } => "Reference",
        IrExpr::LetRef { .. } => "LetRef",
        IrExpr::SelfFieldRef { .. } => "SelfFieldRef",
        IrExpr::FieldAccess { .. } => "FieldAccess",
        IrExpr::StructInst { .. } => "StructInst",
        IrExpr::EnumInst { .. } => "EnumInst",
        IrExpr::Tuple { .. } => "Tuple",
        IrExpr::Array { .. } => "Array",
        IrExpr::BinaryOp { .. } => "BinaryOp",
        IrExpr::UnaryOp { .. } => "UnaryOp",
        IrExpr::If { .. } => "If",
        IrExpr::For { .. } => "For",
        IrExpr::Match { .. } => "Match",
        IrExpr::FunctionCall { .. } => "FunctionCall",
        IrExpr::CallClosure { .. } => "CallClosure",
        IrExpr::MethodCall { .. } => "MethodCall",
        IrExpr::Closure { .. } => "Closure",
        IrExpr::ClosureRef { .. } => "ClosureRef",
        IrExpr::DictLiteral { .. } => "DictLiteral",
        IrExpr::DictAccess { .. } => "DictAccess",
        IrExpr::Block { .. } => "Block",
    }
}
