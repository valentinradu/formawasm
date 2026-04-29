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

mod aggregate;
mod binary_op;
mod block;
mod call;
mod control;
mod literal;
mod reference;
mod unary_op;

use std::cell::Cell;
use std::collections::HashMap;

use formalang::ast::PrimitiveType;
use formalang::ir::{BindingId, FunctionId, IrExpr, IrModule, ResolvedType};
use thiserror::Error;
use wasm_encoder::InstructionSink;

use crate::layout::LayoutError;
use crate::types::TypeMapError;

pub use aggregate::{lower_field_access, lower_struct_inst};
pub use binary_op::lower_binary_op;
pub use block::{lower_block, lower_function_body, lower_function_body_in_module};
pub use call::lower_function_call;
pub use control::lower_if;
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
    /// Reference to the IR module being lowered. Aggregate lowerings
    /// (`StructInst`, `FieldAccess`, `EnumInst`, `Match`, …) need it
    /// to look up struct / enum definitions for layout planning.
    pub module: Option<&'a IrModule>,
    /// Wasm function index of the bump-allocator helper. Aggregate
    /// constructors call this to reserve linear-memory bytes.
    pub bump_allocator: Option<u32>,
    /// Counter that hands out fresh wasm-local indices reserved as
    /// scratch slots for aggregate base pointers. The function-body
    /// pre-walk has already extended `Function::new(locals)` to
    /// include these; the counter starts past the params + lets and
    /// increments per aggregate construction visited in lowering
    /// order.
    pub scratch_locals: Option<&'a Cell<u32>>,
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
            module: None,
            bump_allocator: None,
            scratch_locals: None,
        }
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

    /// Attach the scratch-local counter.
    #[must_use]
    pub const fn with_scratch_locals(mut self, counter: &'a Cell<u32>) -> Self {
        self.scratch_locals = Some(counter);
        self
    }

    /// Borrow the IR module or surface
    /// [`LowerError::MissingContext`] if the field is unset.
    pub fn module(&self) -> Result<&'a IrModule, LowerError> {
        self.module
            .ok_or(LowerError::MissingContext { what: "module" })
    }

    /// Return the bump-allocator function index or surface
    /// [`LowerError::MissingContext`] if the field is unset.
    pub fn bump_allocator(&self) -> Result<u32, LowerError> {
        self.bump_allocator.ok_or(LowerError::MissingContext {
            what: "bump_allocator",
        })
    }

    /// Hand out the next scratch wasm-local index. The pre-walk that
    /// reserved these locals must have counted at least as many
    /// aggregate constructions as the lowering walker actually visits;
    /// otherwise we'd be pointing past the end of the function's
    /// locals table.
    pub fn next_scratch_local(&self) -> Result<u32, LowerError> {
        let counter = self.scratch_locals.ok_or(LowerError::MissingContext {
            what: "scratch_locals",
        })?;
        let idx = counter.get();
        let next = idx
            .checked_add(1)
            .ok_or_else(|| LowerError::NotYetImplemented {
                what: "more than u32::MAX scratch locals in a single function".to_owned(),
            })?;
        counter.set(next);
        Ok(idx)
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
        IrExpr::Literal { .. } => lower_literal(expr, sink),
        IrExpr::Reference { .. } => lower_reference(expr, sink, ctx),
        IrExpr::LetRef { .. } => lower_let_ref(expr, sink, ctx),
        IrExpr::BinaryOp { .. } => lower_binary_op(expr, sink, ctx),
        IrExpr::UnaryOp { .. } => lower_unary_op(expr, sink, ctx),
        IrExpr::Block { .. } => lower_block(expr, sink, ctx),
        IrExpr::FunctionCall { .. } => lower_function_call(expr, sink, ctx),
        IrExpr::If { .. } => lower_if(expr, sink, ctx),

        IrExpr::StructInst { .. } => lower_struct_inst(expr, sink, ctx),
        IrExpr::FieldAccess { .. } => lower_field_access(expr, sink, ctx),

        IrExpr::SelfFieldRef { .. }
        | IrExpr::EnumInst { .. }
        | IrExpr::Tuple { .. }
        | IrExpr::Array { .. }
        | IrExpr::For { .. }
        | IrExpr::Match { .. }
        | IrExpr::MethodCall { .. }
        | IrExpr::Closure { .. }
        | IrExpr::ClosureRef { .. }
        | IrExpr::DictLiteral { .. }
        | IrExpr::DictAccess { .. } => Err(LowerError::NotYetImplemented {
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
        IrExpr::MethodCall { .. } => "MethodCall",
        IrExpr::Closure { .. } => "Closure",
        IrExpr::ClosureRef { .. } => "ClosureRef",
        IrExpr::DictLiteral { .. } => "DictLiteral",
        IrExpr::DictAccess { .. } => "DictAccess",
        IrExpr::Block { .. } => "Block",
    }
}
