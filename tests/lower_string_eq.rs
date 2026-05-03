//! End-to-end coverage for `BinaryOp::Eq` / `Ne` on `String` operands.
//!
//! Build a one-function module whose body compares two string-literal
//! expressions and returns the boolean result. Run through wasmtime's
//! core runtime and check both the equal and unequal cases.

use formalang::ast::{BinaryOperator, Literal, PrimitiveType};
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

fn string_literal(text: &str) -> IrExpr {
    IrExpr::Literal {
        value: Literal::String(text.to_owned()),
        ty: primitive(PrimitiveType::String),
        span: IrSpan::default(),
    }
}

fn binary_op(op: BinaryOperator, l: IrExpr, r: IrExpr) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(l),
        right: Box::new(r),
        op,
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

#[test]
fn equal_strings_compare_true() -> TestResult {
    let body = binary_op(
        BinaryOperator::Eq,
        string_literal("hello"),
        string_literal("hello"),
    );
    let mut module = IrModule::new();
    module
        .functions
        .push(function("eq", primitive(PrimitiveType::Boolean), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "eq")?;
    let r = f.call(&mut store, ())?;
    if r != 1 {
        return Err(format!("Eq(\"hello\", \"hello\"): got {r}, want 1").into());
    }
    Ok(())
}

#[test]
fn unequal_strings_compare_false() -> TestResult {
    let body = binary_op(
        BinaryOperator::Eq,
        string_literal("hello"),
        string_literal("world"),
    );
    let mut module = IrModule::new();
    module
        .functions
        .push(function("eq", primitive(PrimitiveType::Boolean), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "eq")?;
    let r = f.call(&mut store, ())?;
    if r != 0 {
        return Err(format!("Eq(\"hello\", \"world\"): got {r}, want 0").into());
    }
    Ok(())
}

#[test]
fn different_length_strings_compare_false() -> TestResult {
    // "hi" vs "hello" — fast-path: lengths differ, so __str_eq
    // short-circuits without entering the byte loop.
    let body = binary_op(
        BinaryOperator::Eq,
        string_literal("hi"),
        string_literal("hello"),
    );
    let mut module = IrModule::new();
    module
        .functions
        .push(function("eq", primitive(PrimitiveType::Boolean), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "eq")?;
    let r = f.call(&mut store, ())?;
    if r != 0 {
        return Err(format!("Eq(\"hi\", \"hello\"): got {r}, want 0").into());
    }
    Ok(())
}

#[test]
fn ne_inverts_eq() -> TestResult {
    // `Ne("hello", "hello")` should yield 0; `Ne("hello", "world")`
    // should yield 1.
    let mut module = IrModule::new();
    module.functions.push(function(
        "ne-eq",
        primitive(PrimitiveType::Boolean),
        binary_op(
            BinaryOperator::Ne,
            string_literal("hello"),
            string_literal("hello"),
        ),
    ));
    module.functions.push(function(
        "ne-diff",
        primitive(PrimitiveType::Boolean),
        binary_op(
            BinaryOperator::Ne,
            string_literal("hello"),
            string_literal("world"),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let eq_call = instance.get_typed_func::<(), i32>(&mut store, "ne-eq")?;
    let diff_call = instance.get_typed_func::<(), i32>(&mut store, "ne-diff")?;
    let r1 = eq_call.call(&mut store, ())?;
    let r2 = diff_call.call(&mut store, ())?;
    if r1 != 0 {
        return Err(format!("Ne(\"hello\", \"hello\"): got {r1}, want 0").into());
    }
    if r2 != 1 {
        return Err(format!("Ne(\"hello\", \"world\"): got {r2}, want 1").into());
    }
    Ok(())
}
