//! Lowering for aggregate constructors and field access.
//!
//! Phase 1b mc3b lands [`lower_struct_inst`]; the matching
//! `FieldAccess` and `Tuple` lowerings ride the next two mcs but
//! reuse the helpers in this file.
//!
//! Aggregates live in linear memory. A constructor:
//!
//! 1. Calls the bump-allocator helper to reserve the struct's size.
//! 2. Stores the returned base pointer in a fresh scratch local.
//! 3. Stores each field at `base + offset` using the right primitive
//!    store opcode for the field's type.
//! 4. Reloads the base pointer as the constructor's value.

use formalang::ast::PrimitiveType;
use formalang::ir::{IrExpr, IrStruct, ResolvedType};
use wasm_encoder::{InstructionSink, MemArg};

use super::{LowerContext, LowerError, lower_expr};
use crate::layout::{FieldLayout, plan_struct};
use crate::module::MEMORY_INDEX;

/// Lower [`IrExpr::StructInst`].
///
/// Pushes the base pointer of the freshly-allocated struct onto the
/// stack. Field initializers are evaluated in source order — each
/// one is stored at `base + field_offset` via the appropriate
/// primitive `store` opcode for its type.
pub fn lower_struct_inst(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::StructInst {
        struct_id, fields, ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_struct_inst called with non-StructInst expression".to_owned(),
        });
    };

    let id = struct_id.ok_or(LowerError::ExternalStructInst)?;
    let module = ctx.module()?;
    let s = module
        .structs
        .get(id.0 as usize)
        .ok_or(LowerError::UnknownStruct(id))?;
    let layout = plan_struct(s, module)?;

    let base_local = allocate_aggregate(layout.size, sink, ctx)?;

    for (name, _idx, value_expr) in fields {
        let (field_layout, field_def) = lookup_field_by_name(s, &layout.fields, name)?;
        let primitive = primitive_of(&field_def.ty)?;
        sink.local_get(base_local);
        lower_expr(value_expr, sink, ctx)?;
        store_primitive(primitive, *field_layout, sink);
    }

    sink.local_get(base_local);
    Ok(())
}

/// Reserve `size` bytes via the bump-allocator helper, store the
/// returned pointer in a fresh scratch local, and return that
/// local's index.
pub(super) fn allocate_aggregate(
    size: u32,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<u32, LowerError> {
    let alloc_idx = ctx.bump_allocator()?;
    let size_i32 = i32::try_from(size).map_err(|_| {
        LowerError::Layout(crate::layout::LayoutError::SizeOverflow {
            name: "<aggregate>".to_owned(),
        })
    })?;
    let base_local = ctx.next_scratch_local()?;
    sink.i32_const(size_i32);
    sink.call(alloc_idx);
    sink.local_set(base_local);
    Ok(base_local)
}

/// Pick the right `xN.store` opcode for `p`. `bool` uses
/// `i32.store8` since the field occupies a single byte; the other
/// primitives store in their full native width.
pub(super) fn store_primitive(
    p: PrimitiveType,
    layout: FieldLayout,
    sink: &mut InstructionSink<'_>,
) {
    let mem_arg = field_mem_arg(layout);
    match p {
        PrimitiveType::Boolean => {
            sink.i32_store8(mem_arg);
        }
        PrimitiveType::I32 => {
            sink.i32_store(mem_arg);
        }
        PrimitiveType::I64 => {
            sink.i64_store(mem_arg);
        }
        PrimitiveType::F32 => {
            sink.f32_store(mem_arg);
        }
        PrimitiveType::F64 => {
            sink.f64_store(mem_arg);
        }
        // primitive_of() filters non-storable primitives long before
        // we get here; the wildcard satisfies wildcard_enum_match_arm
        // and traps in case an unsupported primitive ever slips
        // through (defensive — shouldn't be reachable).
        PrimitiveType::Never
        | PrimitiveType::String
        | PrimitiveType::Path
        | PrimitiveType::Regex
        | _ => {
            sink.unreachable();
        }
    }
}

/// Translate a [`FieldLayout`] into a wasm `MemArg`. The `align`
/// field encodes the *log2* of the alignment hint (0 for 1-byte, 2
/// for 4-byte, 3 for 8-byte). `MemArg::offset` is the byte offset
/// added to the base pointer at runtime.
pub(super) fn field_mem_arg(layout: FieldLayout) -> MemArg {
    MemArg {
        offset: u64::from(layout.offset),
        align: align_to_log2(layout.align),
        memory_index: MEMORY_INDEX,
    }
}

const fn align_to_log2(align: u32) -> u32 {
    match align {
        2 => 1,
        4 => 2,
        8 => 3,
        // 1 (and any non-power-of-two value, which our layout planner
        // never produces) gets the byte-alignment hint.
        _ => 0,
    }
}

/// Extract the primitive type of a field. Aggregate field types
/// surface as `LayoutError::NotYetSupported` through `plan_struct`
/// long before reaching this helper, so anything non-primitive here
/// indicates an upstream invariant break.
pub(super) fn primitive_of(ty: &ResolvedType) -> Result<PrimitiveType, LowerError> {
    match ty {
        ResolvedType::Primitive(p) => Ok(*p),
        ResolvedType::Struct(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => Err(LowerError::FieldAccessOnNonAggregate { ty: ty.clone() }),
    }
}

/// Resolve a field by name in a struct that has already been laid
/// out. `StructInst.fields` carries `(name, FieldIdx, value)`
/// triples; the name lookup is robust to a `FieldIdx(0)` placeholder
/// from older IR-emitting code paths.
pub(super) fn lookup_field_by_name<'a>(
    s: &'a IrStruct,
    field_layouts: &'a [FieldLayout],
    name: &str,
) -> Result<(&'a FieldLayout, &'a formalang::ir::IrField), LowerError> {
    for (i, f) in s.fields.iter().enumerate() {
        if f.name == name {
            let fl = field_layouts
                .get(i)
                .ok_or_else(|| LowerError::FieldIndexOutOfRange {
                    struct_name: s.name.clone(),
                    field_count: s.fields.len(),
                    field_idx: u32::try_from(i).unwrap_or(u32::MAX),
                })?;
            return Ok((fl, f));
        }
    }
    Err(LowerError::FieldIndexOutOfRange {
        struct_name: s.name.clone(),
        field_count: s.fields.len(),
        field_idx: u32::MAX,
    })
}
