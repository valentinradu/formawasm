//! End-to-end coverage for `Literal::String` lowering through the
//! data-section pipeline.
//!
//! Build a module whose entry point returns a string literal, run it
//! through `module_lowering::lower_module`, validate, instantiate
//! under wasmtime, and read back the `{ ptr, len }` header plus the
//! seeded bytes from linear memory.

use formalang::ast::{Literal, PrimitiveType};
use formalang::ir::{IrExpr, IrFunction, IrModule, IrSpan, ResolvedType};
use formawasm::layout::{STRING_LEN_OFFSET, STRING_PTR_OFFSET, plan_string};
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
fn string_literal_returns_pointer_to_seeded_header() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "make",
        primitive(PrimitiveType::String),
        string_literal("hello"),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let _layout = plan_string(&module);
    let header: [u8; 8] = read_memory(&mut store, &instance, header_ptr)?;
    let ptr_off_signed = i32::try_from(STRING_PTR_OFFSET)?;
    let len_off_signed = i32::try_from(STRING_LEN_OFFSET)?;
    let _ = (ptr_off_signed, len_off_signed); // documents the offsets
    let str_ptr = i32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let str_len = i32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if str_len != 5 {
        return Err(format!("len: got {str_len}, want 5").into());
    }

    let chars: [u8; 5] = read_memory(&mut store, &instance, str_ptr)?;
    if &chars != b"hello" {
        return Err(format!("bytes: got {chars:?}, want b\"hello\"").into());
    }
    Ok(())
}

#[test]
fn duplicate_string_literals_dedupe_to_one_header() -> TestResult {
    // `make_a` and `make_b` both return the literal "shared". The
    // string pool interns it once, so both functions return the same
    // header pointer.
    let mut module = IrModule::new();
    module.functions.push(function(
        "make-a",
        primitive(PrimitiveType::String),
        string_literal("shared"),
    ));
    module.functions.push(function(
        "make-b",
        primitive(PrimitiveType::String),
        string_literal("shared"),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let a = instance.get_typed_func::<(), i32>(&mut store, "make-a")?;
    let b = instance.get_typed_func::<(), i32>(&mut store, "make-b")?;
    let pa = a.call(&mut store, ())?;
    let pb = b.call(&mut store, ())?;
    if pa != pb {
        return Err(format!("expected dedup: got {pa} and {pb}").into());
    }
    Ok(())
}

#[test]
fn empty_string_literal_seeds_zero_length_header() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "make",
        primitive(PrimitiveType::String),
        string_literal(""),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;
    let header: [u8; 8] = read_memory(&mut store, &instance, header_ptr)?;
    let str_len = i32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    if str_len != 0 {
        return Err(format!("len: got {str_len}, want 0").into());
    }
    Ok(())
}
