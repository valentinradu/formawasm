//! Tests for `lower::lower_literal`. Each test builds a function
//! whose entire body is a literal, lowers it onto an
//! `InstructionSink`, then runs the assembled module through
//! `wasmparser::Validator` to confirm the emitted instruction is
//! well-typed against the function signature.

use formalang::ast::{Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType};
use formalang::ir::{IrExpr, IrSpan, ResolvedType};
use formawasm::lower::{self, BindingMap, FunctionMap, LowerContext, LowerError};
use formawasm::module::ModuleBuilder;
use wasm_encoder::{Function, ValType};
use wasmparser::{Validator, WasmFeatures};

const fn dummy_ctx<'a>(bindings: &'a BindingMap, functions: &'a FunctionMap) -> LowerContext<'a> {
    LowerContext::new(bindings, functions)
}

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

/// Build a one-function module whose body lowers `expr` onto its
/// instruction sink, framed by `end`. The function takes no params
/// and returns one `result`.
fn build_module_with_literal_body(expr: &IrExpr, result: ValType) -> Result<Vec<u8>, TestError> {
    let mut builder = ModuleBuilder::new();
    let mut body = Function::new(core::iter::empty());
    let bindings = BindingMap::new();
    let functions = FunctionMap::new();
    let ctx = dummy_ctx(&bindings, &functions);
    {
        let sink = &mut body.instructions();
        lower::lower_literal(expr, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[], &[result], &body);
    Ok(builder.finish())
}

const fn primitive_ty(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn integer_literal_unsuffixed(value: i128, ty: ResolvedType) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::unsuffixed(value)),
        ty,
        span: IrSpan::default(),
    }
}

fn integer_literal_suffixed(value: i128, suffix: NumericSuffix, ty: ResolvedType) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty,
        span: IrSpan::default(),
    }
}

fn float_literal_unsuffixed(value: f64, ty: ResolvedType) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::unsuffixed_float(value)),
        ty,
        span: IrSpan::default(),
    }
}

fn boolean_literal(b: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(b),
        ty: primitive_ty(PrimitiveType::Boolean),
        span: IrSpan::default(),
    }
}

#[test]
fn lowers_i32_literal() -> TestResult {
    let bytes = build_module_with_literal_body(
        &integer_literal_unsuffixed(42, primitive_ty(PrimitiveType::I32)),
        ValType::I32,
    )?;
    validate(&bytes)
}

#[test]
fn lowers_i64_literal_at_max_value() -> TestResult {
    // The whole point of the upstream NumberValue change: i64::MAX
    // round-trips exactly through the new i128 storage.
    let bytes = build_module_with_literal_body(
        &integer_literal_suffixed(
            i128::from(i64::MAX),
            NumericSuffix::I64,
            primitive_ty(PrimitiveType::I64),
        ),
        ValType::I64,
    )?;
    validate(&bytes)
}

#[test]
fn lowers_f32_literal() -> TestResult {
    let bytes = build_module_with_literal_body(
        &float_literal_unsuffixed(1.5_f64, primitive_ty(PrimitiveType::F32)),
        ValType::F32,
    )?;
    validate(&bytes)
}

#[test]
fn lowers_f64_literal() -> TestResult {
    let bytes = build_module_with_literal_body(
        &float_literal_unsuffixed(0.125_f64, primitive_ty(PrimitiveType::F64)),
        ValType::F64,
    )?;
    validate(&bytes)
}

#[test]
fn lowers_boolean_true_as_i32_one() -> TestResult {
    let bytes = build_module_with_literal_body(&boolean_literal(true), ValType::I32)?;
    validate(&bytes)
}

#[test]
fn lowers_boolean_false_as_i32_zero() -> TestResult {
    let bytes = build_module_with_literal_body(&boolean_literal(false), ValType::I32)?;
    validate(&bytes)
}

#[test]
fn rejects_i32_literal_out_of_range() -> TestResult {
    let expr = integer_literal_unsuffixed(
        i128::from(i32::MAX).saturating_add(1),
        primitive_ty(PrimitiveType::I32),
    );
    let mut body = Function::new(core::iter::empty());
    let bindings = BindingMap::new();
    let functions = FunctionMap::new();
    let ctx = dummy_ctx(&bindings, &functions);
    let result = lower::lower_literal(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::LiteralOutOfRange {
            target: PrimitiveType::I32,
            ..
        }) => Ok(()),
        other => Err(format!("expected LiteralOutOfRange(I32), got {other:?}").into()),
    }
}

#[test]
fn string_literal_without_pool_surfaces_missing_context() -> TestResult {
    // Phase 2 mc9 wires `Literal::String` lowering through the
    // compile-time string pool; lowering against a context that has
    // no pool attached must surface `MissingContext` rather than
    // emitting a stray instruction or panicking.
    let expr = IrExpr::Literal {
        value: Literal::String("hi".to_owned()),
        ty: primitive_ty(PrimitiveType::String),
        span: IrSpan::default(),
    };
    let mut body = Function::new(core::iter::empty());
    let bindings = BindingMap::new();
    let functions = FunctionMap::new();
    let ctx = dummy_ctx(&bindings, &functions);
    let result = lower::lower_literal(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::MissingContext {
            what: "string_pool",
        }) => Ok(()),
        other => Err(format!("expected MissingContext(string_pool), got {other:?}").into()),
    }
}

#[test]
fn rejects_boolean_with_non_boolean_type() -> TestResult {
    let expr = IrExpr::Literal {
        value: Literal::Boolean(true),
        ty: primitive_ty(PrimitiveType::I64),
        span: IrSpan::default(),
    };
    let mut body = Function::new(core::iter::empty());
    let bindings = BindingMap::new();
    let functions = FunctionMap::new();
    let ctx = dummy_ctx(&bindings, &functions);
    let result = lower::lower_literal(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::LiteralTypeMismatch { kind, .. }) if kind == "Boolean" => Ok(()),
        other => Err(format!("expected LiteralTypeMismatch(Boolean), got {other:?}").into()),
    }
}
