//! Tests for `lower::lower_array`.
//!
//! Build a tiny module whose entry point returns an `Array<T>` literal,
//! lower it through the production `module_lowering::lower_module`
//! pipeline, validate, instantiate under wasmtime, and read back the
//! header + element-buffer bytes through the exported memory to check
//! that `{ ptr, len, cap }` and the in-buffer values are correct for
//! every supported element type.

mod common;
use common::{array_ty, seed_prelude};

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    FieldIdx, IrExpr, IrField, IrFunction, IrModule, IrSpan, IrStruct, ResolvedType, StructId,
};
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

fn integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
    let suffix = if ty == PrimitiveType::I64 {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive(ty),
        span: IrSpan::default(),
    }
}

fn boolean_literal(b: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(b),
        ty: primitive(PrimitiveType::Boolean),
        span: IrSpan::default(),
    }
}

fn array_literal(elements: Vec<IrExpr>, elem_ty: ResolvedType) -> IrExpr {
    IrExpr::Array {
        elements,
        ty: array_ty(elem_ty),
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
    let mut buf = [0u8; N];
    let off = usize::try_from(offset).map_err(|_| -> TestError { "offset negative".into() })?;
    memory.read(&*store, off, &mut buf)?;
    Ok(buf)
}

fn read_header(
    store: &mut Store<()>,
    instance: &Instance,
    header_ptr: i32,
) -> Result<(i32, i32, i32), TestError> {
    let buf: [u8; 12] = read_memory(store, instance, header_ptr)?;
    let ptr = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let cap = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    Ok((ptr, len, cap))
}

#[test]
fn empty_array_of_i32_validates_and_has_zero_len() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "empty",
        array_ty(primitive(PrimitiveType::I32)),
        array_literal(Vec::new(), primitive(PrimitiveType::I32)),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "empty")?;
    let header_ptr = f.call(&mut store, ())?;
    if header_ptr < 0 {
        return Err(format!("expected non-negative header ptr, got {header_ptr}").into());
    }

    let (_buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (0, 0) {
        return Err(format!("len/cap: got ({len}, {cap}), want (0, 0)").into());
    }
    Ok(())
}

#[test]
fn array_of_three_i32_writes_buffer_correctly() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "make",
        array_ty(primitive(PrimitiveType::I32)),
        array_literal(
            vec![
                integer_literal(7, PrimitiveType::I32),
                integer_literal(11, PrimitiveType::I32),
                integer_literal(13, PrimitiveType::I32),
            ],
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }

    let buf: [u8; 12] = read_memory(&mut store, &instance, buf_ptr)?;
    let a = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let b = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let c = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if (a, b, c) != (7, 11, 13) {
        return Err(format!("elements: got ({a}, {b}, {c}), want (7, 11, 13)").into());
    }
    Ok(())
}

#[test]
fn array_of_two_i64_uses_eight_byte_stride() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "make",
        array_ty(primitive(PrimitiveType::I64)),
        array_literal(
            vec![
                integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
                integer_literal(0x1234_5678_9ABC_DEF0, PrimitiveType::I64),
            ],
            primitive(PrimitiveType::I64),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (2, 2) {
        return Err(format!("len/cap: got ({len}, {cap}), want (2, 2)").into());
    }

    let buf: [u8; 16] = read_memory(&mut store, &instance, buf_ptr)?;
    let a = i64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let b = i64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    if (a, b) != (0x0BAD_F00D_DEAD_BEEF, 0x1234_5678_9ABC_DEF0) {
        return Err(format!("elements: got ({a:#x}, {b:#x})").into());
    }
    Ok(())
}

#[test]
fn array_of_booleans_uses_one_byte_stride() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "make",
        array_ty(primitive(PrimitiveType::Boolean)),
        array_literal(
            vec![
                boolean_literal(true),
                boolean_literal(false),
                boolean_literal(true),
                boolean_literal(true),
            ],
            primitive(PrimitiveType::Boolean),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (4, 4) {
        return Err(format!("len/cap: got ({len}, {cap}), want (4, 4)").into());
    }

    let buf: [u8; 4] = read_memory(&mut store, &instance, buf_ptr)?;
    if buf != [1, 0, 1, 1] {
        return Err(format!("elements: got {buf:?}, want [1, 0, 1, 1]").into());
    }
    Ok(())
}

#[test]
fn array_of_struct_pointers_stores_aggregate_pointers() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    // The prelude seeded Array=0, Dictionary=1, Range=2; user
    // struct lands at the next free index.
    let pair_struct_id = StructId(3);
    let pair = IrStruct {
        name: "Pair".to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: vec![
            IrField {
                name: "a".to_owned(),
                ty: primitive(PrimitiveType::I32),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            },
            IrField {
                name: "b".to_owned(),
                ty: primitive(PrimitiveType::I32),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            },
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    };
    module.structs.push(pair);

    let pair_inst = |a: i128, b: i128| IrExpr::StructInst {
        struct_id: Some(pair_struct_id),
        type_args: Vec::new(),
        fields: vec![
            (
                "a".to_owned(),
                FieldIdx(0),
                integer_literal(a, PrimitiveType::I32),
            ),
            (
                "b".to_owned(),
                FieldIdx(1),
                integer_literal(b, PrimitiveType::I32),
            ),
        ],
        ty: ResolvedType::Struct(pair_struct_id),
        span: IrSpan::default(),
    };

    module.functions.push(function(
        "make",
        array_ty(ResolvedType::Struct(pair_struct_id)),
        array_literal(
            vec![pair_inst(11, 22), pair_inst(33, 44)],
            ResolvedType::Struct(pair_struct_id),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (2, 2) {
        return Err(format!("len/cap: got ({len}, {cap}), want (2, 2)").into());
    }

    // Each element is an i32 pointer — 8 bytes total for two pointers.
    let pointers: [u8; 8] = read_memory(&mut store, &instance, buf_ptr)?;
    let p0 = i32::from_le_bytes([pointers[0], pointers[1], pointers[2], pointers[3]]);
    let p1 = i32::from_le_bytes([pointers[4], pointers[5], pointers[6], pointers[7]]);
    if p0 == 0 || p1 == 0 || p0 == p1 {
        return Err(format!("expected two distinct non-zero pair ptrs, got {p0}, {p1}").into());
    }

    // Follow each pointer and verify the (a, b) pair behind it.
    let mut read_pair = |ptr: i32| -> Result<(i32, i32), TestError> {
        let raw: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
        Ok((
            i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            i32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]),
        ))
    };
    if read_pair(p0)? != (11, 22) {
        return Err("pair[0] != (11, 22)".into());
    }
    if read_pair(p1)? != (33, 44) {
        return Err("pair[1] != (33, 44)".into());
    }
    Ok(())
}
