//! End-to-end coverage for the Some-wrap path on let bindings.
//!
//! Builds a module whose body assigns a primitive value to an
//! `Optional<T>`-typed let binding and returns the resulting pointer.
//! The produced bytes are validated, instantiated under wasmtime, and
//! the tag + payload are read back from linear memory.

use formalang::ast::{Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType};
use formalang::ir::{BindingId, IrBlockStatement, IrExpr, IrFunction, IrModule, ResolvedType};
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
    ResolvedType::Optional(Box::new(inner))
}

fn integer_literal(value: i128, prim: PrimitiveType) -> IrExpr {
    let suffix = match prim {
        PrimitiveType::I64 => NumericSuffix::I64,
        _ => NumericSuffix::I32,
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive(prim),
    }
}

fn boolean_literal(b: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(b),
        ty: primitive(PrimitiveType::Boolean),
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

fn read_memory(
    store: &mut Store<()>,
    instance: &Instance,
    offset: i32,
    len: usize,
) -> Result<Vec<u8>, TestError> {
    let memory = instance
        .exports(&mut *store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;
    let off = usize::try_from(offset).map_err(|_| -> TestError { "ptr negative".into() })?;
    let mut buf = vec![0u8; len];
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
        }],
        result: Box::new(IrExpr::LetRef {
            binding_id,
            name: "x".to_owned(),
            ty: target_ty.clone(),
        }),
        ty: target_ty.clone(),
    };
    let mut module = IrModule::new();
    module.functions.push(function("make", target_ty, body));
    module
}

#[test]
fn some_wrap_i32_stores_tag_and_payload() -> TestResult {
    let module = make_some_wrap_module(PrimitiveType::I32, integer_literal(42, PrimitiveType::I32));
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    let bytes = read_memory(&mut store, &instance, ptr, layout.size as usize)?;

    let tag_end = layout.payload_offset as usize;
    let tag = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }

    let payload = i32::from_le_bytes([
        bytes[tag_end],
        bytes[tag_end + 1],
        bytes[tag_end + 2],
        bytes[tag_end + 3],
    ]);
    if payload != 42 {
        return Err(format!("payload: got {payload}, want 42").into());
    }
    Ok(())
}

#[test]
fn some_wrap_i64_uses_padded_payload_offset() -> TestResult {
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
    let bytes = read_memory(&mut store, &instance, ptr, layout.size as usize)?;

    let tag = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }

    let payload_off = layout.payload_offset as usize;
    let payload = i64::from_le_bytes([
        bytes[payload_off],
        bytes[payload_off + 1],
        bytes[payload_off + 2],
        bytes[payload_off + 3],
        bytes[payload_off + 4],
        bytes[payload_off + 5],
        bytes[payload_off + 6],
        bytes[payload_off + 7],
    ]);
    if payload != i64::MAX {
        return Err(format!("payload: got {payload}, want {}", i64::MAX).into());
    }
    Ok(())
}

#[test]
fn some_wrap_boolean_stores_one_byte_payload() -> TestResult {
    let module = make_some_wrap_module(PrimitiveType::Boolean, boolean_literal(true));
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::Boolean), &module)?;
    let bytes = read_memory(&mut store, &instance, ptr, layout.size as usize)?;

    let tag = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload_off = layout.payload_offset as usize;
    if bytes[payload_off] != 1 {
        return Err(format!("payload: got {}, want 1", bytes[payload_off]).into());
    }
    Ok(())
}
