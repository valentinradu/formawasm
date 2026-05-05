//! End-to-end coverage for the Some-wrap path on let bindings.
//!
//! Builds a module whose body assigns a primitive value to an
//! `Optional<T>`-typed let binding and returns the resulting pointer.
//! The produced bytes are validated, instantiated under wasmtime, and
//! the tag + payload are read back from linear memory.

mod common;
use common::{optional_ty, seed_prelude};

use formalang::ast::{Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType};
use formalang::ir::{
    BindingId, IrBlockStatement, IrExpr, IrFunction, IrModule, IrSpan, ResolvedType,
};
use formawasm::layout::{OPTIONAL_TAG_SOME, plan_optional};
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

fn integer_literal(value: i128, prim: PrimitiveType) -> IrExpr {
    let suffix = if matches!(prim, PrimitiveType::I64) {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive(prim),
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

/// Build a one-function module whose body sets a let binding of type
/// `Optional<payload_prim>` from `value_expr` (whose static type is
/// `payload_prim`) and returns the resulting pointer.
fn make_some_wrap_module(payload_prim: PrimitiveType, value_expr: IrExpr) -> IrModule {
    let binding_id = BindingId(0);
    let target_ty = optional(primitive(payload_prim));
    let body = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id,
            name: "x".to_owned(),
            mutable: false,
            ty: Some(target_ty.clone()),
            value: value_expr,
            span: IrSpan::default(),
        }],
        result: Box::new(IrExpr::LetRef {
            binding_id,
            name: "x".to_owned(),
            ty: target_ty.clone(),
            span: IrSpan::default(),
        }),
        ty: target_ty.clone(),
        span: IrSpan::default(),
    };
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function("make", target_ty, body));
    module
}

#[test]
fn some_wrap_i32_stores_tag_and_payload() -> TestResult {
    // `Optional<I32>` lays out as 8 bytes: tag (4) + payload (4).
    let module = make_some_wrap_module(PrimitiveType::I32, integer_literal(42, PrimitiveType::I32));
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    if layout.size != 8 {
        return Err(format!("Optional<I32> size: got {}, want 8", layout.size).into());
    }
    let buf: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
    let tag = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if payload != 42 {
        return Err(format!("payload: got {payload}, want 42").into());
    }
    Ok(())
}

#[test]
fn some_wrap_i64_uses_padded_payload_offset() -> TestResult {
    // `Optional<I64>` lays out as 16 bytes: tag (4) + 4 padding + payload (8).
    let module = make_some_wrap_module(
        PrimitiveType::I64,
        integer_literal(i128::from(i64::MAX), PrimitiveType::I64),
    );
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::I64), &module)?;
    if layout.size != 16 || layout.payload_offset != 8 {
        return Err(format!(
            "Optional<I64> size/payload_offset: got {}/{}, want 16/8",
            layout.size, layout.payload_offset
        )
        .into());
    }
    let buf: [u8; 16] = read_memory(&mut store, &instance, ptr)?;
    let tag = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    if payload != i64::MAX {
        return Err(format!("payload: got {payload}, want {}", i64::MAX).into());
    }
    Ok(())
}

#[test]
fn some_wrap_boolean_stores_one_byte_payload() -> TestResult {
    // `Optional<Boolean>` lays out as 8 bytes: tag (4) + 1-byte payload + 3 padding.
    let module = make_some_wrap_module(PrimitiveType::Boolean, boolean_literal(true));
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::Boolean), &module)?;
    if layout.size != 8 || layout.payload_offset != 4 {
        return Err(format!(
            "Optional<Boolean> size/payload_offset: got {}/{}, want 8/4",
            layout.size, layout.payload_offset
        )
        .into());
    }
    let buf: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
    let tag = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    if buf[4] != 1 {
        return Err(format!("payload: got {}, want 1", buf[4]).into());
    }
    Ok(())
}
