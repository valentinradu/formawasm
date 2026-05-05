//! Tests for `lower::lower_binary_op`. Each test builds a function
//! whose body computes a binary expression over its parameters,
//! lowers it, and validates with `wasmparser`.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType,
};
use formalang::ir::{BindingId, IrExpr, IrSpan, ReferenceTarget, ResolvedType};
use formawasm::lower::{self, BindingMap, FunctionMap, LowerContext, LowerError};
use formawasm::module::ModuleBuilder;
use wasm_encoder::{Function, ValType};
use wasmparser::{Validator, WasmFeatures};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

const fn primitive_ty(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn param_ref(id: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::Reference {
        path: vec![format!("p{id}")],
        target: ReferenceTarget::Param(BindingId(id)),
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

fn integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
    let suffix = if ty == PrimitiveType::I64 {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

fn binary_op(op: BinaryOperator, left: IrExpr, right: IrExpr, ty: PrimitiveType) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(left),
        right: Box::new(right),
        op,
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

/// Compile a function with two same-typed params and one result,
/// whose body is `expr`. `result` is the result valtype.
fn build_binary_function(
    expr: &IrExpr,
    operand: ValType,
    result: ValType,
) -> Result<Vec<u8>, TestError> {
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    bindings.insert(BindingId(1), 1);
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);

    let mut builder = ModuleBuilder::new();
    let mut body = Function::new(core::iter::empty());
    {
        let sink = &mut body.instructions();
        lower::lower_expr(expr, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[operand, operand], &[result], &body);
    Ok(builder.finish())
}

// ── Integer arithmetic ─────────────────────────────────────────────

#[test]
fn lowers_i32_add() -> TestResult {
    // p0 + p1
    let expr = binary_op(
        BinaryOperator::Add,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn lowers_i32_div() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Div,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn lowers_i32_mod() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Mod,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn lowers_i64_mul() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Mul,
        param_ref(0, PrimitiveType::I64),
        param_ref(1, PrimitiveType::I64),
        PrimitiveType::I64,
    );
    validate(&build_binary_function(&expr, ValType::I64, ValType::I64)?)
}

// ── Float arithmetic ───────────────────────────────────────────────

#[test]
fn lowers_f32_div() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Div,
        param_ref(0, PrimitiveType::F32),
        param_ref(1, PrimitiveType::F32),
        PrimitiveType::F32,
    );
    validate(&build_binary_function(&expr, ValType::F32, ValType::F32)?)
}

#[test]
fn lowers_f64_sub() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Sub,
        param_ref(0, PrimitiveType::F64),
        param_ref(1, PrimitiveType::F64),
        PrimitiveType::F64,
    );
    validate(&build_binary_function(&expr, ValType::F64, ValType::F64)?)
}

#[test]
fn rejects_f64_mod() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Mod,
        param_ref(0, PrimitiveType::F64),
        param_ref(1, PrimitiveType::F64),
        PrimitiveType::F64,
    );
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    bindings.insert(BindingId(1), 1);
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_expr(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnsupportedOperator {
            op,
            operand: PrimitiveType::F64,
        }) if op == "Mod" => Ok(()),
        other => Err(format!("expected UnsupportedOperator(Mod, F64), got {other:?}").into()),
    }
}

// ── Comparisons (return Boolean → i32) ─────────────────────────────

#[test]
fn lowers_i32_lt_returns_bool() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Lt,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::Boolean,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn lowers_f64_eq_returns_bool() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Eq,
        param_ref(0, PrimitiveType::F64),
        param_ref(1, PrimitiveType::F64),
        PrimitiveType::Boolean,
    );
    validate(&build_binary_function(&expr, ValType::F64, ValType::I32)?)
}

#[test]
fn lowers_boolean_eq() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Eq,
        param_ref(0, PrimitiveType::Boolean),
        param_ref(1, PrimitiveType::Boolean),
        PrimitiveType::Boolean,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

// ── Logical ────────────────────────────────────────────────────────

#[test]
fn lowers_boolean_and() -> TestResult {
    let expr = binary_op(
        BinaryOperator::And,
        param_ref(0, PrimitiveType::Boolean),
        param_ref(1, PrimitiveType::Boolean),
        PrimitiveType::Boolean,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn lowers_boolean_or() -> TestResult {
    let expr = binary_op(
        BinaryOperator::Or,
        param_ref(0, PrimitiveType::Boolean),
        param_ref(1, PrimitiveType::Boolean),
        PrimitiveType::Boolean,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn rejects_and_on_i32() -> TestResult {
    let expr = binary_op(
        BinaryOperator::And,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    bindings.insert(BindingId(1), 1);
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_expr(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnsupportedOperator {
            op,
            operand: PrimitiveType::I32,
        }) if op == "And" => Ok(()),
        other => Err(format!("expected UnsupportedOperator(And, I32), got {other:?}").into()),
    }
}

// ── Mixed: nested expression + literal operand ─────────────────────

#[test]
fn lowers_mixed_param_and_literal() -> TestResult {
    // p0 * 2 (mul i32 with rhs literal)
    let expr = binary_op(
        BinaryOperator::Mul,
        param_ref(0, PrimitiveType::I32),
        integer_literal(2, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn lowers_nested_binary_op() -> TestResult {
    // (p0 + p1) * (p0 - p1)
    let sum = binary_op(
        BinaryOperator::Add,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    let diff = binary_op(
        BinaryOperator::Sub,
        param_ref(0, PrimitiveType::I32),
        param_ref(1, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    let expr = binary_op(BinaryOperator::Mul, sum, diff, PrimitiveType::I32);
    validate(&build_binary_function(&expr, ValType::I32, ValType::I32)?)
}

// Range lowering moved out of "NotYetImplemented" in Phase 1c —
// dedicated coverage lives in `tests/lower_range.rs`.
