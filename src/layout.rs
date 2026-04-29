//! Memory-layout planner for [`IrStruct`] types.
//!
//! Decides where each field lives in linear memory ahead of any
//! lowering. Following the Component-Model canonical ABI:
//!
//! | type            | size | align |
//! | --------------- | ---: | ----: |
//! | `bool`          | 1    | 1     |
//! | `s32` / `f32`   | 4    | 4     |
//! | `s64` / `f64`   | 8    | 8     |
//!
//! Each field starts at the next offset rounded up to the field's
//! alignment; the total struct size is rounded up to the struct's
//! alignment (the maximum of its field alignments, or 1 for an empty
//! struct). Aggregate fields (nested structs, enums, tuples, arrays,
//! …) surface as [`LayoutError::NotYetSupported`] until their own
//! lowerings land in subsequent Phase 1b microcommits.

use formalang::ast::PrimitiveType;
use formalang::ir::{IrModule, IrStruct, ResolvedType};
use thiserror::Error;

/// Errors produced by [`plan_struct`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LayoutError {
    /// A field's type is in scope for the backend but not yet wired
    /// into the layout planner. The variant name is carried as a
    /// string so the diagnostic still reads naturally after
    /// `ResolvedType` evolves.
    #[error("type {kind} is not yet supported by the struct-layout planner")]
    NotYetSupported {
        /// Short tag identifying the unsupported type kind
        /// (`"Struct"`, `"Tuple"`, `"Closure"`, …).
        kind: String,
    },

    /// A field is marked `optional` (i.e. `T?`). `Optional<T>`
    /// lowering lands in Phase 2; rejecting it explicitly here keeps
    /// the layout planner from silently dropping the optionality flag.
    #[error("optional field '{field}' on struct '{struct_name}' is not yet supported")]
    OptionalField {
        /// Source-level struct name.
        struct_name: String,
        /// Source-level field name.
        field: String,
    },

    /// The struct's total size — including alignment padding —
    /// exceeds `u32::MAX`. Linear-memory offsets are `u32` in core
    /// wasm so we cannot represent layouts larger than that.
    #[error("struct '{name}' size exceeds u32::MAX after alignment padding")]
    SizeOverflow {
        /// Source-level struct name.
        name: String,
    },
}

/// Where a single field lives inside its containing struct.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain layout record consumed externally; intentionally constructible"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldLayout {
    /// Byte offset from the start of the struct.
    pub offset: u32,
    /// Field size in bytes (no alignment padding included).
    pub size: u32,
    /// Field alignment in bytes (always a power of two).
    pub align: u32,
}

/// Layout decisions for a whole `IrStruct`.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain layout record consumed externally; intentionally constructible"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLayout {
    /// Total size in bytes, including any trailing alignment padding.
    pub size: u32,
    /// Struct alignment — the maximum of its field alignments, or 1
    /// for an empty struct.
    pub align: u32,
    /// One [`FieldLayout`] per field, in declaration order.
    pub fields: Vec<FieldLayout>,
}

/// Compute the layout of `s`.
///
/// `module` is accepted for API symmetry with the layout planners
/// added in later mcs (which need it to resolve nested struct/enum
/// references); the Phase 1b mc1 implementation only uses primitive
/// fields and therefore ignores it.
pub fn plan_struct(s: &IrStruct, _module: &IrModule) -> Result<StructLayout, LayoutError> {
    let mut offset: u32 = 0;
    let mut max_align: u32 = 1;
    let mut fields = Vec::with_capacity(s.fields.len());

    for f in &s.fields {
        if f.optional {
            return Err(LayoutError::OptionalField {
                struct_name: s.name.clone(),
                field: f.name.clone(),
            });
        }
        let (size, align) = type_size_align(&f.ty)?;
        let aligned = align_up(offset, align).ok_or_else(|| LayoutError::SizeOverflow {
            name: s.name.clone(),
        })?;
        let next_offset = aligned
            .checked_add(size)
            .ok_or_else(|| LayoutError::SizeOverflow {
                name: s.name.clone(),
            })?;

        fields.push(FieldLayout {
            offset: aligned,
            size,
            align,
        });
        offset = next_offset;
        if align > max_align {
            max_align = align;
        }
    }

    let total_size = align_up(offset, max_align).ok_or_else(|| LayoutError::SizeOverflow {
        name: s.name.clone(),
    })?;

    Ok(StructLayout {
        size: total_size,
        align: max_align,
        fields,
    })
}

/// Return the `(size, align)` pair for a primitive-typed field.
///
/// `Never` and the heap-typed primitives (`String`, `Path`, `Regex`)
/// surface as [`LayoutError::NotYetSupported`] — `Never` because no
/// instance can be stored, the others because their layouts depend on
/// the Phase 2 string runtime.
fn primitive_size_align(p: PrimitiveType) -> Result<(u32, u32), LayoutError> {
    match p {
        PrimitiveType::Boolean => Ok((1, 1)),
        PrimitiveType::I32 | PrimitiveType::F32 => Ok((4, 4)),
        PrimitiveType::I64 | PrimitiveType::F64 => Ok((8, 8)),
        // Never (uninhabited — no instance to lay out), heap-typed
        // primitives (`String` / `Path` / `Regex`) whose layouts
        // depend on the Phase 2 string runtime, and any future
        // #[non_exhaustive] variants all surface here.
        PrimitiveType::Never
        | PrimitiveType::String
        | PrimitiveType::Path
        | PrimitiveType::Regex
        | _ => Err(LayoutError::NotYetSupported {
            kind: format!("{p:?}"),
        }),
    }
}

/// Return the `(size, align)` pair for a [`ResolvedType`] used as a
/// struct field type.
fn type_size_align(ty: &ResolvedType) -> Result<(u32, u32), LayoutError> {
    match ty {
        ResolvedType::Primitive(p) => primitive_size_align(*p),
        ResolvedType::Struct(_) => Err(LayoutError::NotYetSupported {
            kind: "Struct".to_owned(),
        }),
        ResolvedType::Enum(_) => Err(LayoutError::NotYetSupported {
            kind: "Enum".to_owned(),
        }),
        ResolvedType::Tuple(_) => Err(LayoutError::NotYetSupported {
            kind: "Tuple".to_owned(),
        }),
        ResolvedType::Array(_) => Err(LayoutError::NotYetSupported {
            kind: "Array<T>".to_owned(),
        }),
        ResolvedType::Range(_) => Err(LayoutError::NotYetSupported {
            kind: "Range<T>".to_owned(),
        }),
        ResolvedType::Optional(_) => Err(LayoutError::NotYetSupported {
            kind: "Optional<T>".to_owned(),
        }),
        ResolvedType::Dictionary { .. } => Err(LayoutError::NotYetSupported {
            kind: "Dictionary<K, V>".to_owned(),
        }),
        ResolvedType::Closure { .. } => Err(LayoutError::NotYetSupported {
            kind: "Closure".to_owned(),
        }),
        ResolvedType::Trait(_) => Err(LayoutError::NotYetSupported {
            kind: "Trait".to_owned(),
        }),
        ResolvedType::Generic { .. } => Err(LayoutError::NotYetSupported {
            kind: "Generic".to_owned(),
        }),
        ResolvedType::TypeParam(name) => Err(LayoutError::NotYetSupported {
            kind: format!("TypeParam({name})"),
        }),
        ResolvedType::External { name, .. } => Err(LayoutError::NotYetSupported {
            kind: format!("External({name})"),
        }),
        ResolvedType::Error => Err(LayoutError::NotYetSupported {
            kind: "Error".to_owned(),
        }),
    }
}

/// Round `offset` up to the next multiple of `align`.
///
/// Returns `None` on overflow. `align` is required to be a power of
/// two (all our supported alignments — 1, 4, 8 — satisfy this); the
/// helper silently treats non-power-of-two alignments as a no-op
/// once `align <= 1`, which is the only sentinel callers actually
/// pass.
fn align_up(offset: u32, align: u32) -> Option<u32> {
    if align <= 1 {
        return Some(offset);
    }
    let mask = align.checked_sub(1)?;
    let added = offset.checked_add(mask)?;
    Some(added & !mask)
}
