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

use formalang::ast::{PrimitiveType, Visibility};
use formalang::ir::{IrEnum, IrModule, IrStruct, ResolvedType};
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

    /// An enum has more than `u32::MAX` variants. The discriminant tag
    /// is encoded as a 4-byte unsigned integer.
    #[error("enum '{name}' has more than u32::MAX variants")]
    TooManyVariants {
        /// Source-level enum name.
        name: String,
    },

    /// An enum has zero variants. Such an enum is uninhabited (no
    /// value can ever exist) and rejecting it here matches Rust's
    /// treatment of `enum Empty {}`.
    #[error("enum '{name}' is uninhabited (zero variants)")]
    UninhabitedEnum {
        /// Source-level enum name.
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

// ── enum layout ──────────────────────────────────────────────────────

/// Discriminant-tag size in bytes. The tag occupies a `u32` slot at
/// offset 0 of every enum value.
pub const ENUM_TAG_SIZE: u32 = 4;

/// Discriminant-tag alignment.
pub const ENUM_TAG_ALIGN: u32 = 4;

/// Layout for one variant inside an [`EnumLayout`].
#[expect(
    clippy::exhaustive_structs,
    reason = "plain layout record consumed externally; intentionally constructible"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantLayout {
    /// Source-level variant name (preserved for diagnostics).
    pub name: String,
    /// Discriminant value stored in the tag slot when this variant is
    /// active. Variants are tagged in declaration order starting at 0.
    pub tag: u32,
    /// Per-payload-field offsets, **absolute** from the enum value's
    /// base pointer (i.e. already past the tag and any post-tag
    /// alignment padding). Empty for unit variants.
    pub fields: Vec<FieldLayout>,
}

/// Layout decisions for a whole [`IrEnum`].
///
/// Uniform-variant policy: every variant occupies exactly the same
/// number of bytes — the maximum across variants — so the constructor
/// site doesn't need to know which variant is being built when
/// computing the allocation size. This trades memory for code-size
/// simplicity, which suits Phase 1b where allocations are infrequent.
#[expect(
    clippy::exhaustive_structs,
    reason = "plain layout record consumed externally; intentionally constructible"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumLayout {
    /// Total bytes one enum value occupies. Includes the tag, post-
    /// tag alignment padding, payload, and trailing alignment padding.
    pub size: u32,
    /// Enum alignment — the maximum of [`ENUM_TAG_ALIGN`] and every
    /// variant payload's alignment.
    pub align: u32,
    /// Offset of the discriminant tag (always 0).
    pub tag_offset: u32,
    /// Offset where every variant's payload starts. `tag_offset +
    /// ENUM_TAG_SIZE`, rounded up to the max payload alignment.
    pub payload_offset: u32,
    /// One [`VariantLayout`] per variant, in declaration order.
    pub variants: Vec<VariantLayout>,
}

/// Compute the layout of `e`.
///
/// Each variant's payload is laid out via the same canonical-ABI
/// rules as a struct (see [`plan_struct`]) — field offsets within a
/// variant payload are then offset by `payload_offset` to become
/// absolute. Unit variants have no fields and therefore no
/// per-variant offsets; their slot is just the tag plus enough
/// padding to match the enum's overall size.
pub fn plan_enum(e: &IrEnum, module: &IrModule) -> Result<EnumLayout, LayoutError> {
    if e.variants.is_empty() {
        return Err(LayoutError::UninhabitedEnum {
            name: e.name.clone(),
        });
    }

    // Step 1: lay out each variant's payload as a sub-struct so we
    // can reuse `plan_struct`. The placeholder enum name is appended
    // for clearer error reporting.
    let mut variant_payloads = Vec::with_capacity(e.variants.len());
    let mut max_payload_align: u32 = 1;
    let mut max_payload_size: u32 = 0;
    for variant in &e.variants {
        let placeholder = IrStruct {
            name: format!("{}::{}", e.name, variant.name),
            visibility: Visibility::Private,
            traits: Vec::new(),
            fields: variant.fields.clone(),
            generic_params: Vec::new(),
            doc: None,
        };
        let payload = plan_struct(&placeholder, module)?;
        if payload.align > max_payload_align {
            max_payload_align = payload.align;
        }
        if payload.size > max_payload_size {
            max_payload_size = payload.size;
        }
        variant_payloads.push(payload);
    }

    let enum_align = if max_payload_align > ENUM_TAG_ALIGN {
        max_payload_align
    } else {
        ENUM_TAG_ALIGN
    };
    let payload_offset =
        align_up(ENUM_TAG_SIZE, max_payload_align).ok_or_else(|| LayoutError::SizeOverflow {
            name: e.name.clone(),
        })?;
    let raw_size = payload_offset
        .checked_add(max_payload_size)
        .ok_or_else(|| LayoutError::SizeOverflow {
            name: e.name.clone(),
        })?;
    let total_size = align_up(raw_size, enum_align).ok_or_else(|| LayoutError::SizeOverflow {
        name: e.name.clone(),
    })?;

    let mut variants = Vec::with_capacity(e.variants.len());
    for (i, (variant, payload)) in e
        .variants
        .iter()
        .zip(variant_payloads.into_iter())
        .enumerate()
    {
        let tag = u32::try_from(i).map_err(|_| LayoutError::TooManyVariants {
            name: e.name.clone(),
        })?;
        let mut absolute_fields = Vec::with_capacity(payload.fields.len());
        for field in &payload.fields {
            let abs_offset = payload_offset.checked_add(field.offset).ok_or_else(|| {
                LayoutError::SizeOverflow {
                    name: e.name.clone(),
                }
            })?;
            absolute_fields.push(FieldLayout {
                offset: abs_offset,
                size: field.size,
                align: field.align,
            });
        }
        variants.push(VariantLayout {
            name: variant.name.clone(),
            tag,
            fields: absolute_fields,
        });
    }

    Ok(EnumLayout {
        size: total_size,
        align: enum_align,
        tag_offset: 0,
        payload_offset,
        variants,
    })
}
