#![cfg(any())] // TODO 0.0.4-beta migration: hand-built IR needs prelude-id seeding

//! Tests for `lower::lower_range` (dispatched from `lower_binary_op`
//! when the operator is `BinaryOperator::Range`).
//!
//! Build a small module whose entry point returns a `Range<T>` value
//! built from two literal bounds, lower it through the production
//! `module_lowering::lower_module` pipeline, validate, instantiate
//! under wasmtime, and read back the `{ start, end }` bytes through
//! the exported memory.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType,
};
use formalang::ir::{IrExpr, IrFunction, IrModule, IrSpan, ResolvedType};
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

fn range_ty(elem: ResolvedType) -> ResolvedType {
    ResolvedType::Range(Box::new(elem))
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

fn range_expr(start: IrExpr, end: IrExpr, elem_ty: ResolvedType) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(start),
        right: Box::new(end),
        op: BinaryOperator::Range,
        ty: range_ty(elem_ty),
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

#[test]
fn range_of_i32_writes_start_and_end_inline() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "make",
        range_ty(primitive(PrimitiveType::I32)),
        range_expr(
            integer_literal(2, PrimitiveType::I32),
            integer_literal(20, PrimitiveType::I32),
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let buf: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
    let start = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let end = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if (start, end) != (2, 20) {
        return Err(format!("got ({start}, {end}), want (2, 20)").into());
    }
    Ok(())
}

#[test]
fn range_of_i64_writes_eight_byte_bounds() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "make",
        range_ty(primitive(PrimitiveType::I64)),
        range_expr(
            integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
            integer_literal(0x1234_5678_9ABC_DEF0, PrimitiveType::I64),
            primitive(PrimitiveType::I64),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let buf: [u8; 16] = read_memory(&mut store, &instance, ptr)?;
    let start = i64::from_le_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]);
    let end = i64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    if (start, end) != (0x0BAD_F00D_DEAD_BEEF, 0x1234_5678_9ABC_DEF0) {
        return Err(format!("got ({start:#x}, {end:#x})").into());
    }
    Ok(())
}

#[test]
fn empty_range_of_i32_still_writes_both_bounds() -> TestResult {
    // start == end represents an empty range; the slot still holds
    // both literal values verbatim.
    let mut module = IrModule::new();
    module.functions.push(function(
        "make",
        range_ty(primitive(PrimitiveType::I32)),
        range_expr(
            integer_literal(7, PrimitiveType::I32),
            integer_literal(7, PrimitiveType::I32),
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let buf: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
    let start = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let end = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if (start, end) != (7, 7) {
        return Err(format!("got ({start}, {end}), want (7, 7)").into());
    }
    Ok(())
}
