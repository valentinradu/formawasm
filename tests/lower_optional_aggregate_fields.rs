//! Coverage for `Optional<T>` Some-wrap at struct-field and array-
//! element initializer sites.
//!
//! Builds modules where the field/element declared type is
//! `Optional<T>` and the initializer expression has static type `T`.
//! Each scenario runs through the production module-lowering pipeline,
//! validates, instantiates under wasmtime, and reads the wrapped
//! cell's tag + payload through linear memory.

mod common;
use common::{seed_prelude, optional_ty, array_ty, range_ty, dict_ty};

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    FieldIdx, IrExpr, IrField, IrFunction, IrModule, IrSpan, IrStruct, ResolvedType, StructId,
};
use formawasm::layout::{OPTIONAL_TAG_SOME, plan_optional, plan_struct};
use formawasm::module_lowering;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::{Engine, Instance, Module, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn optional(inner: ResolvedType) -> ResolvedType {
    optional_ty(inner)
}

fn integer_literal(value: i128) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(value),
            NumericSuffix::I32,
        )),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    }
}

fn function(name: &str, return_ty: ResolvedType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(return_ty),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn instantiate(bytes: &[u8]) -> Result<(Store<()>, Instance), TestError> {
    let engine = Engine::default();
    let m = Module::from_binary(&engine, bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    Ok((store, instance))
}

fn read_memory<const N: usize>(
    store: &mut Store<()>,
    instance: &Instance,
    offset: i32,
) -> Result<[u8; N], TestError> {
    let memory = instance
        .exports(&mut *store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;
    let off = usize::try_from(offset).map_err(|_| -> TestError { "ptr negative".into() })?;
    let mut buf = [0u8; N];
    memory.read(&*store, off, &mut buf)?;
    Ok(buf)
}

#[test]
fn struct_optional_field_some_wraps_plain_initializer() -> TestResult {
    // ```
    // struct Box { value: Optional<I32> }
    // fn make() -> Box { Box(value: 42) }
    // ```
    //
    // `Box.value` is declared `Optional<I32>` and the initializer is a
    // plain I32 literal. The struct-field-store coercion site wraps
    // the I32 into a tagged-Some cell and writes the cell pointer at
    // the field's offset. The test reads the field pointer back
    // through linear memory and verifies the wrapped cell carries
    // tag = SOME and the original payload.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let struct_def = IrStruct {
        name: "Box".to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields: vec![IrField {
            name: "value".to_owned(),
            ty: optional(primitive(PrimitiveType::I32)),
            mutable: false,
            optional: true,
            default: None,
            doc: None,
            convention: ParamConvention::Let,
            span: IrSpan::default(),
        }],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    };
    module.structs.push(struct_def);

    let body = IrExpr::StructInst {
        struct_id: Some(StructId(3)),
        type_args: Vec::new(),
        fields: vec![("value".to_owned(), FieldIdx(0), integer_literal(42))],
        ty: ResolvedType::Struct(StructId(3)),
        span: IrSpan::default(),
    };
    module
        .functions
        .push(function("make", ResolvedType::Struct(StructId(3)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let box_ptr = f.call(&mut store, ())?;

    // The struct's `value` field lives at offset 0 and stores the
    // optional cell's pointer.
    // After seed_prelude the Box struct is at index 3 (Array=0,
    // Dictionary=1, Range=2 occupy the leading slots).
    let struct_layout = plan_struct(
        module.structs.get(3).ok_or("no Box struct in module")?,
        &module,
    )?;
    if struct_layout.size != 4 {
        return Err(format!("Box size: got {}, want 4", struct_layout.size).into());
    }
    let field_layout = struct_layout
        .fields
        .first()
        .ok_or("no field layout in struct")?;
    if field_layout.offset != 0 || field_layout.size != 4 {
        return Err(format!(
            "Box.value layout: got offset {} size {}, want 0 / 4",
            field_layout.offset, field_layout.size
        )
        .into());
    }

    // Read the i32 pointer the field stores, then read the optional
    // cell at that pointer.
    let field_bytes: [u8; 4] = read_memory(&mut store, &instance, box_ptr)?;
    let opt_ptr = i32::from_le_bytes(field_bytes);
    let opt_layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    if opt_layout.size != 8 {
        return Err(format!("Optional<I32> size: got {}, want 8", opt_layout.size).into());
    }
    let cell: [u8; 8] = read_memory(&mut store, &instance, opt_ptr)?;
    let tag = u32::from_le_bytes([cell[0], cell[1], cell[2], cell[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("optional tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i32::from_le_bytes([cell[4], cell[5], cell[6], cell[7]]);
    if payload != 42 {
        return Err(format!("optional payload: got {payload}, want 42").into());
    }
    Ok(())
}

#[test]
fn array_optional_element_some_wraps_each_initializer() -> TestResult {
    // ```
    // fn make() -> Array<Optional<I32>> { [1, 2, 3] }
    // ```
    //
    // The array's declared element type is `Optional<I32>`. Each
    // literal initializer flows through the array-element coercion
    // site, gets Some-wrapped into its own cell, and the cell pointer
    // gets stored in the buffer. The test reads buffer[1] (the
    // pointer to the second cell) and verifies its tag/payload match
    // the second literal.
    let elem_ty = optional(primitive(PrimitiveType::I32));
    let body = IrExpr::Array {
        elements: vec![integer_literal(1), integer_literal(2), integer_literal(3)],
        ty: array_ty(elem_ty.clone()),
        span: IrSpan::default(),
    };
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "make",
        array_ty(elem_ty),
        body,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    // Read header: ptr at 0, len at 4, cap at 8.
    let header: [u8; 12] = read_memory(&mut store, &instance, header_ptr)?;
    let buf_ptr = i32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let len = i32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if len != 3 {
        return Err(format!("len: got {len}, want 3").into());
    }

    // Each buffer slot is an i32 pointer to an Optional<I32> cell.
    // Read each 4-byte slot back through linear memory (avoids
    // indexing into a stack-allocated buffer at runtime offsets).
    let opt_layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    for (i, expected) in [1, 2, 3].iter().enumerate() {
        let i_i32 = i32::try_from(i).map_err(|_| -> TestError { "index overflow".into() })?;
        let slot_ptr = buf_ptr
            .checked_add(i_i32.checked_mul(4).ok_or("buffer offset overflow")?)
            .ok_or("buffer slot pointer overflow")?;
        let slot: [u8; 4] = read_memory(&mut store, &instance, slot_ptr)?;
        let cell_ptr = i32::from_le_bytes(slot);
        let cell: [u8; 8] = read_memory(&mut store, &instance, cell_ptr)?;
        let tag = u32::from_le_bytes([cell[0], cell[1], cell[2], cell[3]]);
        if tag != OPTIONAL_TAG_SOME {
            return Err(format!("elem[{i}] tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
        }
        let payload = i32::from_le_bytes([cell[4], cell[5], cell[6], cell[7]]);
        if payload != *expected {
            return Err(format!("elem[{i}] payload: got {payload}, want {expected}").into());
        }
    }
    let _ = opt_layout; // documents the expected per-cell layout
    Ok(())
}
