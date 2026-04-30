//! End-to-end coverage for `BinaryOp::Add` on `String` operands.
//!
//! The result is a freshly-allocated `{ ptr, len }` header pointing
//! at a freshly-allocated buffer that contains both inputs back-to-
//! back. The test reads the header back through linear memory and
//! verifies the buffer's contents.

use formalang::ast::{BinaryOperator, Literal, PrimitiveType};
use formalang::ir::{IrExpr, IrFunction, IrModule, ResolvedType};
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

fn string_literal(text: &str) -> IrExpr {
    IrExpr::Literal {
        value: Literal::String(text.to_owned()),
        ty: primitive(PrimitiveType::String),
    }
}

fn add(l: IrExpr, r: IrExpr) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(l),
        right: Box::new(r),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::String),
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
    }
}

fn instantiate(bytes: &[u8]) -> Result<(Store<()>, Instance), TestError> {
    let engine = Engine::default();
    let m = Module::from_binary(&engine, bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    Ok((store, instance))
}

fn read_header(
    store: &mut Store<()>,
    instance: &Instance,
    header_ptr: i32,
) -> Result<(i32, i32), TestError> {
    let memory = instance
        .exports(&mut *store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;
    let off = usize::try_from(header_ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    let mut buf = [0u8; 8];
    memory.read(&*store, off, &mut buf)?;
    let ptr = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    Ok((ptr, len))
}

fn read_bytes(
    store: &mut Store<()>,
    instance: &Instance,
    ptr: i32,
    len: i32,
) -> Result<Vec<u8>, TestError> {
    let memory = instance
        .exports(&mut *store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;
    let off = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    let n = usize::try_from(len).map_err(|_| -> TestError { "len negative".into() })?;
    let mut buf = vec![0u8; n];
    memory.read(&*store, off, &mut buf)?;
    Ok(buf)
}

#[test]
fn concat_two_literals_writes_combined_buffer() -> TestResult {
    // `"hello" + " world"` produces a 11-byte buffer containing
    // "hello world".
    let body = add(string_literal("hello"), string_literal(" world"));
    let mut module = IrModule::new();
    module
        .functions
        .push(function("greet", primitive(PrimitiveType::String), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "greet")?;
    let header_ptr = f.call(&mut store, ())?;

    let (ptr, len) = read_header(&mut store, &instance, header_ptr)?;
    if len != 11 {
        return Err(format!("len: got {len}, want 11").into());
    }
    let bytes = read_bytes(&mut store, &instance, ptr, len)?;
    if &bytes != b"hello world" {
        return Err(format!("bytes: got {bytes:?}, want b\"hello world\"").into());
    }
    Ok(())
}

#[test]
fn concat_with_empty_left_yields_right() -> TestResult {
    let body = add(string_literal(""), string_literal("only-right"));
    let mut module = IrModule::new();
    module
        .functions
        .push(function("cat", primitive(PrimitiveType::String), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "cat")?;
    let header_ptr = f.call(&mut store, ())?;
    let (ptr, len) = read_header(&mut store, &instance, header_ptr)?;
    if len != 10 {
        return Err(format!("len: got {len}, want 10").into());
    }
    let bytes = read_bytes(&mut store, &instance, ptr, len)?;
    if &bytes != b"only-right" {
        return Err(format!("bytes: got {bytes:?}, want b\"only-right\"").into());
    }
    Ok(())
}

#[test]
fn concat_three_literals_associates_left() -> TestResult {
    // `("a" + "b") + "cd"` — exercises a nested concatenation where
    // the left operand is itself a runtime-allocated header.
    let body = add(
        add(string_literal("a"), string_literal("b")),
        string_literal("cd"),
    );
    let mut module = IrModule::new();
    module
        .functions
        .push(function("cat3", primitive(PrimitiveType::String), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "cat3")?;
    let header_ptr = f.call(&mut store, ())?;
    let (ptr, len) = read_header(&mut store, &instance, header_ptr)?;
    if len != 4 {
        return Err(format!("len: got {len}, want 4").into());
    }
    let bytes = read_bytes(&mut store, &instance, ptr, len)?;
    if &bytes != b"abcd" {
        return Err(format!("bytes: got {bytes:?}, want b\"abcd\"").into());
    }
    Ok(())
}
