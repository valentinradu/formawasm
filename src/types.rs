//! Mapping from formalang [`ResolvedType`] / [`PrimitiveType`] to
//! core-wasm value types.
//!
//! Two surfaces:
//!
//! - [`resolved_value_type`] / [`primitive_value_type`] — strict WIT
//!   boundary mapping; used by `wit::emit_wit` so anything that can't
//!   cross the boundary (cross-module `External`, generic params,
//!   trait objects, error placeholders) surfaces as
//!   [`TypeMapError::NotYetSupported`] before component wrap.
//! - [`body_value_type`] / [`body_result_types`] — the in-body
//!   convention. Aggregates (struct / tuple / enum / array / range /
//!   optional / string / dictionary) lower as `i32` pointers, so the
//!   wasm function signature inside the core module shows a single
//!   value type per aggregate parameter / result. The strict surface
//!   is reserved for the WIT side, where aggregate-as-pointer would
//!   lie about the component-model surface.
//!
//! All four numeric primitives (`I32` / `I64` / `F32` / `F64`),
//! `Boolean` (lowered as `i32`), `Never` (zero-result),
//! `String` / `Path` / `Regex` (i32 header pointer), structs, enums,
//! tuples, arrays, ranges, optionals, and dictionaries are wired up.
//! `External`, `TypeParam`, `Generic`, `Trait`, `Closure`, and `Error`
//! stay rejected — they're either upstream-blocked, eliminated by
//! upstream passes, or invariant-violation sentinels.

use formalang::ast::PrimitiveType;
use formalang::ir::ResolvedType;
use thiserror::Error;
use wasm_encoder::ValType;

/// Errors produced by the type-mapping helpers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum TypeMapError {
    /// The type is in scope for the backend but not yet implemented in
    /// the current phase. The variant name is carried as a string so
    /// the diagnostic still works after `ResolvedType` evolves.
    #[error("type {kind} is not yet supported by the backend")]
    NotYetSupported {
        /// Short tag identifying the unsupported type kind
        /// (`"String"`, `"Array<T>"`, `"Closure"`, …).
        kind: String,
    },
}

/// Map a [`PrimitiveType`] to its core-wasm value-type representation.
///
/// `Never` returns `Ok(None)` because it carries no value at runtime;
/// callers handling result types should treat `None` as an empty
/// result list and rely on `unreachable` in the function body.
///
/// The trailing wildcard arm is required by `#[non_exhaustive]` on
/// `PrimitiveType`; new variants land as `NotYetSupported` until we
/// implement them.
pub fn primitive_value_type(p: PrimitiveType) -> Result<Option<ValType>, TypeMapError> {
    match p {
        // Booleans share the i32 representation; 0 is false, anything
        // else is true. The Wasm spec leaves high bits unconstrained
        // after comparison instructions.
        PrimitiveType::I32 | PrimitiveType::Boolean => Ok(Some(ValType::I32)),
        PrimitiveType::I64 => Ok(Some(ValType::I64)),
        PrimitiveType::F32 => Ok(Some(ValType::F32)),
        PrimitiveType::F64 => Ok(Some(ValType::F64)),
        // Zero-sized; functions returning Never emit `unreachable` and
        // declare an empty result list.
        PrimitiveType::Never => Ok(None),
        // Strings live in linear memory as an `{ ptr, len }` header
        // pointer; the wasm value type is i32. `Path` and `Regex` use
        // the same representation internally — the WIT boundary is
        // what distinguishes them.
        PrimitiveType::String | PrimitiveType::Path | PrimitiveType::Regex => {
            Ok(Some(ValType::I32))
        }
        // Future #[non_exhaustive] variants ride this arm.
        _ => Err(TypeMapError::NotYetSupported {
            kind: format!("{p:?}"),
        }),
    }
}

/// Map a [`ResolvedType`] to its core-wasm value type.
///
/// `Ok(None)` represents `Never` (no value carried). Any non-primitive
/// `ResolvedType` is rejected as `NotYetSupported` until the
/// corresponding lowering lands in a later phase.
pub fn resolved_value_type(ty: &ResolvedType) -> Result<Option<ValType>, TypeMapError> {
    match ty {
        ResolvedType::Primitive(p) => primitive_value_type(*p),

        ResolvedType::Struct(_) => Err(TypeMapError::NotYetSupported {
            kind: "Struct".to_owned(),
        }),
        ResolvedType::Trait(_) => Err(TypeMapError::NotYetSupported {
            kind: "Trait".to_owned(),
        }),
        ResolvedType::Enum(_) => Err(TypeMapError::NotYetSupported {
            kind: "Enum".to_owned(),
        }),
        ResolvedType::Array(_) => Err(TypeMapError::NotYetSupported {
            kind: "Array<T>".to_owned(),
        }),
        ResolvedType::Range(_) => Err(TypeMapError::NotYetSupported {
            kind: "Range<T>".to_owned(),
        }),
        ResolvedType::Optional(_) => Err(TypeMapError::NotYetSupported {
            kind: "Optional<T>".to_owned(),
        }),
        ResolvedType::Tuple(_) => Err(TypeMapError::NotYetSupported {
            kind: "Tuple".to_owned(),
        }),
        ResolvedType::Generic { .. } => Err(TypeMapError::NotYetSupported {
            kind: "Generic".to_owned(),
        }),
        ResolvedType::TypeParam(name) => Err(TypeMapError::NotYetSupported {
            kind: format!("TypeParam({name})"),
        }),
        ResolvedType::External { name, .. } => Err(TypeMapError::NotYetSupported {
            kind: format!(
                "External({name}) — cross-module types are upstream-blocked; see formalang docs/developer/cross-module-codegen.md"
            ),
        }),
        ResolvedType::Dictionary { .. } => Err(TypeMapError::NotYetSupported {
            kind: "Dictionary<K, V>".to_owned(),
        }),
        ResolvedType::Closure { .. } => Err(TypeMapError::NotYetSupported {
            kind: "Closure".to_owned(),
        }),
        ResolvedType::Error => Err(TypeMapError::NotYetSupported {
            kind: "Error".to_owned(),
        }),
    }
}

/// Hard-coded layout for a closure value in linear memory.
///
/// 8 bytes total, 4-byte aligned: `(i32 funcref_index, i32 env_ptr)`.
/// Phase 1b mc11 lays out the value but does not yet wire indirect
/// invocation through a funcref table — that's a Phase 1c+ concern.
pub const CLOSURE_VALUE_SIZE: u32 = 8;
/// Closure-value alignment in bytes.
pub const CLOSURE_VALUE_ALIGN: u32 = 4;
/// Offset of the funcref index field inside a closure value.
pub const CLOSURE_FUNCREF_OFFSET: u32 = 0;
/// Offset of the env pointer field inside a closure value.
pub const CLOSURE_ENV_OFFSET: u32 = 4;

/// Map a function `return_type` to a `Vec<ValType>` suitable for
/// `wasm_encoder::TypeSection::function`.
///
/// `None` (unit return) and `Some(Primitive(Never))` both produce an
/// empty vector.
pub fn result_types(return_ty: Option<&ResolvedType>) -> Result<Vec<ValType>, TypeMapError> {
    let Some(ty) = return_ty else {
        return Ok(Vec::new());
    };
    Ok(resolved_value_type(ty)?.map_or_else(Vec::new, |vt| vec![vt]))
}

/// `body_value_type` analogue of [`result_types`].
///
/// Used for the internal core-Wasm function signature where
/// aggregates appear as `i32` pointers. The WIT boundary is still
/// validated through the strict [`resolved_value_type`] /
/// `wit::emit_wit` path.
pub fn body_result_types(return_ty: Option<&ResolvedType>) -> Result<Vec<ValType>, TypeMapError> {
    let Some(ty) = return_ty else {
        return Ok(Vec::new());
    };
    Ok(body_value_type(ty)?.map_or_else(Vec::new, |vt| vec![vt]))
}

/// Map a [`ResolvedType`] to its in-body wasm value type.
///
/// Aggregate types (struct, tuple, enum once it lands) live in linear
/// memory and are passed around as `i32` pointers, so they map to
/// [`ValType::I32`]. Primitives map the same way as
/// [`resolved_value_type`]; the strict version is reserved for the
/// WIT boundary, where aggregate-as-pointer would lie about the
/// component-model surface.
///
/// `Ok(None)` represents `Never` (no value carried).
pub fn body_value_type(ty: &ResolvedType) -> Result<Option<ValType>, TypeMapError> {
    // Pre-1b-mc4 the enum layout doesn't exist yet, but as soon as it
    // lands the storage will also be a pointer, so we map it eagerly
    // alongside `Struct` / `Tuple` for forward-compatibility — the
    // aggregate lowerings still gate behind their own
    // `NotYetImplemented` until each lands.
    match ty {
        ResolvedType::Primitive(p) => primitive_value_type(*p),
        ResolvedType::Struct(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. } => Ok(Some(ValType::I32)),
        ResolvedType::Trait(_) => Err(TypeMapError::NotYetSupported {
            kind: "Trait".to_owned(),
        }),
        ResolvedType::Generic { .. } => Err(TypeMapError::NotYetSupported {
            kind: "Generic".to_owned(),
        }),
        ResolvedType::TypeParam(name) => Err(TypeMapError::NotYetSupported {
            kind: format!("TypeParam({name})"),
        }),
        ResolvedType::External { name, .. } => Err(TypeMapError::NotYetSupported {
            kind: format!(
                "External({name}) — cross-module types are upstream-blocked; see formalang docs/developer/cross-module-codegen.md"
            ),
        }),
        ResolvedType::Error => Err(TypeMapError::NotYetSupported {
            kind: "Error".to_owned(),
        }),
    }
}
