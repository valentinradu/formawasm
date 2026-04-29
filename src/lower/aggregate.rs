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
use formalang::ir::{IrEnum, IrEnumVariant, IrExpr, IrField, IrModule, IrStruct, ResolvedType};
use wasm_encoder::{InstructionSink, MemArg};

use super::{LowerContext, LowerError, lower_expr};
use crate::layout::{
    ARRAY_HEADER_ALIGN, ArrayLayout, ENUM_TAG_ALIGN, FieldLayout, LayoutError, StructLayout,
    VariantLayout, plan_array, plan_enum, plan_struct,
};
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

/// Pick the right `xN.load` opcode for `p`. `bool` uses
/// `i32.load8_u` to zero-extend the byte into the i32 value type.
pub(super) fn load_primitive(
    p: PrimitiveType,
    layout: FieldLayout,
    sink: &mut InstructionSink<'_>,
) {
    let mem_arg = field_mem_arg(layout);
    match p {
        PrimitiveType::Boolean => {
            sink.i32_load8_u(mem_arg);
        }
        PrimitiveType::I32 => {
            sink.i32_load(mem_arg);
        }
        PrimitiveType::I64 => {
            sink.i64_load(mem_arg);
        }
        PrimitiveType::F32 => {
            sink.f32_load(mem_arg);
        }
        PrimitiveType::F64 => {
            sink.f64_load(mem_arg);
        }
        PrimitiveType::Never
        | PrimitiveType::String
        | PrimitiveType::Path
        | PrimitiveType::Regex
        | _ => {
            sink.unreachable();
        }
    }
}

/// Lower [`IrExpr::ClosureRef`].
///
/// Materializes the closure as an `(i32 funcref, i32 env_ptr)` pair
/// in linear memory:
///
/// 1. Look up the lifted top-level function by name (last segment of
///    `funcref`) in `module.functions` and record its wasm function
///    index.
/// 2. Lower `env_struct` — typically an `IrExpr::StructInst` whose
///    fields hold the captured values; the result is the env's base
///    pointer.
/// 3. Allocate `CLOSURE_VALUE_SIZE` bytes through the bump allocator,
///    store the funcref index at offset 0 and the env pointer at
///    offset 4, and leave the closure value's base pointer on the
///    stack.
///
/// Indirect invocation (calling the closure) is not yet wired — that
/// needs a wasm `Table` of funcrefs plus `call_indirect`, which lands
/// later. Today's job is just to produce a valid closure VALUE.
pub fn lower_closure_ref(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::ClosureRef {
        funcref,
        env_struct,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_closure_ref called with non-ClosureRef expression".to_owned(),
        });
    };

    let module = ctx.module()?;
    let last = funcref
        .last()
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: "ClosureRef carries an empty funcref path".to_owned(),
        })?;
    let func_idx_u32 = module
        .functions
        .iter()
        .enumerate()
        .find(|(_, f)| &f.name == last)
        .map(|(i, _)| i)
        .ok_or_else(|| LowerError::NotYetImplemented {
            what: format!("ClosureRef target function '{last}' is not in module.functions"),
        })?;
    let func_idx_raw = u32::try_from(func_idx_u32).map_err(|_| LowerError::NotYetImplemented {
        what: "ClosureRef target index exceeds u32::MAX".to_owned(),
    })?;
    let funcref_wasm_idx = ctx
        .functions
        .get(formalang::ir::FunctionId(func_idx_raw))
        .ok_or(LowerError::UnknownFunction(formalang::ir::FunctionId(
            func_idx_raw,
        )))?;
    let funcref_signed =
        i32::try_from(funcref_wasm_idx).map_err(|_| LowerError::NotYetImplemented {
            what: "ClosureRef target wasm index exceeds i32::MAX".to_owned(),
        })?;

    let base_local = allocate_aggregate(crate::types::CLOSURE_VALUE_SIZE, sink, ctx)?;

    // Funcref slot at offset 0.
    sink.local_get(base_local);
    sink.i32_const(funcref_signed);
    sink.i32_store(MemArg {
        offset: u64::from(crate::types::CLOSURE_FUNCREF_OFFSET),
        align: 2, // log2(4)
        memory_index: MEMORY_INDEX,
    });

    // Env pointer slot at offset 4.
    sink.local_get(base_local);
    lower_expr(env_struct, sink, ctx)?;
    sink.i32_store(MemArg {
        offset: u64::from(crate::types::CLOSURE_ENV_OFFSET),
        align: 2,
        memory_index: MEMORY_INDEX,
    });

    sink.local_get(base_local);
    Ok(())
}

/// Lower [`IrExpr::EnumInst`].
///
/// Allocates `enum_layout.size` bytes through the bump allocator,
/// stores the variant's discriminant tag at `tag_offset`, then writes
/// each provided field at the variant's absolute field offset. The
/// base pointer is left on the stack as the constructor's value.
///
/// The `variant_idx` stored on the IR node is the source of truth
/// when in range; we fall back to a name lookup so older IR shapes
/// emitted with the placeholder `VariantIdx(0)` still resolve.
pub fn lower_enum_inst(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::EnumInst {
        enum_id,
        variant,
        variant_idx,
        fields,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_enum_inst called with non-EnumInst expression".to_owned(),
        });
    };

    let id = enum_id.ok_or(LowerError::ExternalEnumInst)?;
    let module = ctx.module()?;
    let e = module
        .enums
        .get(id.0 as usize)
        .ok_or(LowerError::UnknownEnum(id))?;
    let layout = plan_enum(e, module)?;

    let (variant_layout, variant_def) =
        resolve_variant(e, &layout.variants, variant_idx.0, variant)?;

    let base_local = allocate_aggregate(layout.size, sink, ctx)?;

    // Tag store: i32_const tag, then i32_store at tag_offset.
    sink.local_get(base_local);
    let tag_signed = i32::try_from(variant_layout.tag).unwrap_or(i32::MAX);
    sink.i32_const(tag_signed);
    sink.i32_store(MemArg {
        offset: u64::from(layout.tag_offset),
        align: align_to_log2(ENUM_TAG_ALIGN),
        memory_index: MEMORY_INDEX,
    });

    // Field stores. Match by name so we're robust to a placeholder
    // FieldIdx(0) on the IR node.
    for (field_name, _idx, value_expr) in fields {
        let (field_layout, field_def) =
            lookup_variant_field_by_name(variant_def, &variant_layout.fields, field_name)?;
        let primitive = primitive_of(&field_def.ty)?;
        sink.local_get(base_local);
        lower_expr(value_expr, sink, ctx)?;
        store_primitive(primitive, *field_layout, sink);
    }

    sink.local_get(base_local);
    Ok(())
}

/// Resolve the variant identified by `idx` (with name fallback) on
/// `e`, returning both the layout-side and IR-side metadata.
fn resolve_variant<'a>(
    e: &'a IrEnum,
    variants: &'a [VariantLayout],
    idx: u32,
    name: &str,
) -> Result<(&'a VariantLayout, &'a IrEnumVariant), LowerError> {
    let i = idx as usize;
    if let Some(vl) = variants.get(i)
        && let Some(vd) = e.variants.get(i)
        && vl.name == vd.name
    {
        return Ok((vl, vd));
    }
    for (vl, vd) in variants.iter().zip(e.variants.iter()) {
        if vd.name == name {
            return Ok((vl, vd));
        }
    }
    Err(LowerError::UnknownVariant {
        enum_name: e.name.clone(),
        variant: name.to_owned(),
    })
}

fn lookup_variant_field_by_name<'a>(
    variant_def: &'a IrEnumVariant,
    field_layouts: &'a [FieldLayout],
    name: &str,
) -> Result<(&'a FieldLayout, &'a IrField), LowerError> {
    for (i, f) in variant_def.fields.iter().enumerate() {
        if f.name == name {
            let fl = field_layouts
                .get(i)
                .ok_or_else(|| LowerError::FieldIndexOutOfRange {
                    struct_name: variant_def.name.clone(),
                    field_count: variant_def.fields.len(),
                    field_idx: u32::try_from(i).unwrap_or(u32::MAX),
                })?;
            return Ok((fl, f));
        }
    }
    Err(LowerError::FieldIndexOutOfRange {
        struct_name: variant_def.name.clone(),
        field_count: variant_def.fields.len(),
        field_idx: u32::MAX,
    })
}

/// Lower [`IrExpr::Tuple`].
///
/// Treats the tuple as an anonymous struct synthesized from the
/// carried `ResolvedType::Tuple(...)`. Layout planning, allocation,
/// and per-field stores reuse the same path as
/// [`lower_struct_inst`]; the only difference is the field-meta
/// source.
pub fn lower_tuple(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::Tuple { fields, ty, .. } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_tuple called with non-Tuple expression".to_owned(),
        });
    };

    let module = ctx.module()?;
    let synthetic = synthetic_struct_for_tuple(ty)?;
    let layout = plan_struct(&synthetic, module)?;

    let base_local = allocate_aggregate(layout.size, sink, ctx)?;

    for (name, value_expr) in fields {
        let (field_layout, field_def) =
            lookup_field_by_name_with_meta(&synthetic.fields, &layout.fields, name, "__tuple")?;
        let primitive = primitive_of(&field_def.ty)?;
        sink.local_get(base_local);
        lower_expr(value_expr, sink, ctx)?;
        store_primitive(primitive, *field_layout, sink);
    }

    sink.local_get(base_local);
    Ok(())
}

/// Lower [`IrExpr::Array`].
///
/// Materializes an array literal as a 12-byte header
/// (`{ ptr, len, cap }`) plus a separately-allocated element buffer:
///
/// 1. Allocate `len * element_size` bytes for the element buffer
///    through the bump allocator and stash the base pointer in a
///    scratch local.
/// 2. Walk each element expression in source order; for each one,
///    leave the buffer pointer + element value on the stack and emit
///    the right primitive `store` opcode at offset
///    `i * element_size`. Aggregate elements are stored as `i32`
///    pointers since aggregates already live elsewhere in linear
///    memory.
/// 3. Allocate the 12-byte header through the bump allocator and
///    stash that pointer in a second scratch local. Store the buffer
///    base at offset 0 (`ptr`), the length at offset 4 (`len`), and
///    the length again at offset 8 (`cap` — capacity equals length
///    for literals; growable arrays are a later feature).
/// 4. Leave the header pointer on the stack as the array value.
///
/// Empty arrays still take both allocations; the buffer alloc with
/// size 0 returns the current heap pointer without advancing it, so
/// the header's `ptr` slot may coincide with the header itself —
/// benign since nothing reads through `ptr` when `len == 0`.
pub fn lower_array(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::Array { elements, ty } = expr else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_array called with non-Array expression".to_owned(),
        });
    };

    let ResolvedType::Array(elem_box) = ty else {
        return Err(LowerError::NotYetImplemented {
            what: format!("Array literal carrying non-Array type {ty:?}"),
        });
    };
    let elem_ty = elem_box.as_ref();

    let module = ctx.module()?;
    let layout = plan_array(elem_ty, module)?;

    let len_u32 = u32::try_from(elements.len()).map_err(|_| LowerError::NotYetImplemented {
        what: "array literal with more than u32::MAX elements".to_owned(),
    })?;
    let len_signed = i32::try_from(len_u32).map_err(|_| LowerError::NotYetImplemented {
        what: "array literal length exceeds i32::MAX".to_owned(),
    })?;
    let buffer_size =
        layout
            .element_size
            .checked_mul(len_u32)
            .ok_or_else(|| LayoutError::SizeOverflow {
                name: "<array buffer>".to_owned(),
            })?;

    let buf_local = allocate_aggregate(buffer_size, sink, ctx)?;

    for (i, element) in elements.iter().enumerate() {
        let i_u32 = u32::try_from(i).unwrap_or(u32::MAX);
        let offset =
            layout
                .element_size
                .checked_mul(i_u32)
                .ok_or_else(|| LayoutError::SizeOverflow {
                    name: "<array element offset>".to_owned(),
                })?;
        sink.local_get(buf_local);
        lower_expr(element, sink, ctx)?;
        store_array_element(elem_ty, layout, offset, sink)?;
    }

    let header_local = allocate_aggregate(layout.header_size, sink, ctx)?;
    let header_align_log2 = align_to_log2(ARRAY_HEADER_ALIGN);

    // ptr at offset 0
    sink.local_get(header_local);
    sink.local_get(buf_local);
    sink.i32_store(MemArg {
        offset: 0,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });

    // len at offset 4
    sink.local_get(header_local);
    sink.i32_const(len_signed);
    sink.i32_store(MemArg {
        offset: 4,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });

    // cap at offset 8 (= len for literals)
    sink.local_get(header_local);
    sink.i32_const(len_signed);
    sink.i32_store(MemArg {
        offset: 8,
        align: header_align_log2,
        memory_index: MEMORY_INDEX,
    });

    sink.local_get(header_local);
    Ok(())
}

/// Emit the `store` opcode that writes one array element to `buf +
/// offset`, given the element's resolved type and the array's layout.
fn store_array_element(
    elem_ty: &ResolvedType,
    layout: ArrayLayout,
    offset: u32,
    sink: &mut InstructionSink<'_>,
) -> Result<(), LowerError> {
    let field_layout = FieldLayout {
        offset,
        size: layout.element_size,
        align: layout.element_align,
    };
    match elem_ty {
        ResolvedType::Primitive(p) => {
            store_primitive(*p, field_layout, sink);
            Ok(())
        }
        ResolvedType::Struct(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Tuple(_)
        | ResolvedType::Array(_) => {
            sink.i32_store(field_mem_arg(field_layout));
            Ok(())
        }
        // `plan_array` rejects every other element type, so this arm
        // is defensive only.
        ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Trait(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Error => Err(LowerError::NotYetImplemented {
            what: format!("array element of type {elem_ty:?}"),
        }),
    }
}

/// Lower [`IrExpr::SelfFieldRef`].
///
/// Reads the field at the resolved offset through `self`, which lives
/// at wasm-local 0 (the implicit first parameter on every method).
/// The enclosing impl's struct id comes from
/// [`LowerContext::self_struct_id`]; the field's primitive type comes
/// from the carried `ty`.
pub fn lower_self_field_ref(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::SelfFieldRef {
        field, field_idx, ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_self_field_ref called with non-SelfFieldRef expression".to_owned(),
        });
    };

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
    // `self` is the first wasm parameter — local index 0.
    sink.local_get(0);
    load_primitive(primitive, *field_layout, sink);
    Ok(())
}

/// Lower [`IrExpr::FieldAccess`].
///
/// Evaluates `object` to leave its base pointer on the stack, then
/// emits the primitive load at the resolved field's offset. Works
/// for both struct objects (`ResolvedType::Struct(_)`) and tuple
/// objects (`ResolvedType::Tuple(_)`).
pub fn lower_field_access(
    expr: &IrExpr,
    sink: &mut InstructionSink<'_>,
    ctx: &LowerContext<'_>,
) -> Result<(), LowerError> {
    let IrExpr::FieldAccess {
        object,
        field,
        field_idx,
        ..
    } = expr
    else {
        return Err(LowerError::NotYetImplemented {
            what: "lower_field_access called with non-FieldAccess expression".to_owned(),
        });
    };

    let module = ctx.module()?;
    let (layout, fields_meta) = layout_for_aggregate(object.ty(), module)?;

    // Resolve the field. `field_idx` carries the resolved position;
    // fall back to a name lookup if it points past the end (older IR
    // emitters sometimes leave it as `FieldIdx(0)` placeholder).
    let idx = field_idx.0 as usize;
    let (field_layout, field_def) = if let Some(fl) = layout.fields.get(idx)
        && let Some(fd) = fields_meta.get(idx)
    {
        (fl, fd)
    } else {
        lookup_field_by_name_with_meta(&fields_meta, &layout.fields, field, &type_tag(object.ty()))?
    };

    let primitive = primitive_of(&field_def.ty)?;
    lower_expr(object, sink, ctx)?;
    load_primitive(primitive, *field_layout, sink);
    Ok(())
}

/// Plan the layout for an aggregate object expression and return its
/// field metadata. Tuple objects are mapped to a synthetic struct so
/// `plan_struct` can reused.
pub(super) fn layout_for_aggregate(
    ty: &ResolvedType,
    module: &IrModule,
) -> Result<(StructLayout, Vec<IrField>), LowerError> {
    match ty {
        ResolvedType::Struct(id) => {
            let s = module
                .structs
                .get(id.0 as usize)
                .ok_or(LowerError::UnknownStruct(*id))?;
            let layout = plan_struct(s, module)?;
            Ok((layout, s.fields.clone()))
        }
        ResolvedType::Tuple(_) => {
            let synthetic = synthetic_struct_for_tuple(ty)?;
            let layout = plan_struct(&synthetic, module)?;
            Ok((layout, synthetic.fields))
        }
        ResolvedType::Primitive(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => Err(LowerError::FieldAccessOnNonAggregate { ty: ty.clone() }),
    }
}

/// Build a synthetic [`IrStruct`] from a `ResolvedType::Tuple(...)`.
/// Lets the layout planner be reused for tuples without duplicating
/// its alignment logic.
pub(super) fn synthetic_struct_for_tuple(ty: &ResolvedType) -> Result<IrStruct, LowerError> {
    let ResolvedType::Tuple(fields) = ty else {
        return Err(LowerError::FieldAccessOnNonAggregate { ty: ty.clone() });
    };
    Ok(IrStruct {
        name: "__tuple".to_owned(),
        visibility: formalang::ast::Visibility::Private,
        traits: Vec::new(),
        fields: fields
            .iter()
            .map(|(field_name, field_ty)| IrField {
                name: field_name.clone(),
                ty: field_ty.clone(),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: formalang::ast::ParamConvention::Let,
            })
            .collect(),
        generic_params: Vec::new(),
        doc: None,
    })
}

/// Resolve a field by name once we already have the field-meta and
/// layout vectors. Used as a fallback when `FieldIdx` is out of
/// range — kept robust to placeholder IDs that older IR emitters
/// produce.
pub(super) fn lookup_field_by_name_with_meta<'a>(
    fields_meta: &'a [IrField],
    field_layouts: &'a [FieldLayout],
    name: &str,
    aggregate_tag: &str,
) -> Result<(&'a FieldLayout, &'a IrField), LowerError> {
    for (i, f) in fields_meta.iter().enumerate() {
        if f.name == name {
            let fl = field_layouts
                .get(i)
                .ok_or_else(|| LowerError::FieldIndexOutOfRange {
                    struct_name: aggregate_tag.to_owned(),
                    field_count: fields_meta.len(),
                    field_idx: u32::try_from(i).unwrap_or(u32::MAX),
                })?;
            return Ok((fl, f));
        }
    }
    Err(LowerError::FieldIndexOutOfRange {
        struct_name: aggregate_tag.to_owned(),
        field_count: fields_meta.len(),
        field_idx: u32::MAX,
    })
}

fn type_tag(ty: &ResolvedType) -> String {
    match ty {
        ResolvedType::Struct(_) => "<struct>".to_owned(),
        ResolvedType::Tuple(_) => "__tuple".to_owned(),
        ResolvedType::Primitive(_)
        | ResolvedType::Trait(_)
        | ResolvedType::Enum(_)
        | ResolvedType::Array(_)
        | ResolvedType::Range(_)
        | ResolvedType::Optional(_)
        | ResolvedType::Generic { .. }
        | ResolvedType::TypeParam(_)
        | ResolvedType::External { .. }
        | ResolvedType::Dictionary { .. }
        | ResolvedType::Closure { .. }
        | ResolvedType::Error => "<non-aggregate>".to_owned(),
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
