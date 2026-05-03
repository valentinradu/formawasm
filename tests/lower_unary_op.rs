//! Tests for `lower::lower_unary_op` (Neg, Not).

use formalang::ast::{PrimitiveType, UnaryOperator};
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

fn unary_op(op: UnaryOperator, operand: IrExpr, ty: PrimitiveType) -> IrExpr {
    IrExpr::UnaryOp {
        op,
        operand: Box::new(operand),
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

fn build_unary_function(
    expr: &IrExpr,
    operand_ty: ValType,
    result_ty: ValType,
) -> Result<Vec<u8>, TestError> {
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);

    let mut builder = ModuleBuilder::new();
    let mut body = Function::new(core::iter::empty());
    {
        let sink = &mut body.instructions();
        lower::lower_expr(expr, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[operand_ty], &[result_ty], &body);
    Ok(builder.finish())
}

#[test]
fn neg_i32_lowers_to_zero_minus_operand() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Neg,
        param_ref(0, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    validate(&build_unary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn neg_i64_lowers_to_zero_minus_operand() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Neg,
        param_ref(0, PrimitiveType::I64),
        PrimitiveType::I64,
    );
    validate(&build_unary_function(&expr, ValType::I64, ValType::I64)?)
}

#[test]
fn neg_f32_uses_native_f32_neg() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Neg,
        param_ref(0, PrimitiveType::F32),
        PrimitiveType::F32,
    );
    validate(&build_unary_function(&expr, ValType::F32, ValType::F32)?)
}

#[test]
fn neg_f64_uses_native_f64_neg() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Neg,
        param_ref(0, PrimitiveType::F64),
        PrimitiveType::F64,
    );
    validate(&build_unary_function(&expr, ValType::F64, ValType::F64)?)
}

#[test]
fn not_boolean_lowers_to_i32_eqz() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Not,
        param_ref(0, PrimitiveType::Boolean),
        PrimitiveType::Boolean,
    );
    validate(&build_unary_function(&expr, ValType::I32, ValType::I32)?)
}

#[test]
fn rejects_neg_on_boolean() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Neg,
        param_ref(0, PrimitiveType::Boolean),
        PrimitiveType::Boolean,
    );
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_expr(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnsupportedOperator {
            op,
            operand: PrimitiveType::Boolean,
        }) if op == "Neg" => Ok(()),
        other => Err(format!("expected UnsupportedOperator(Neg, Boolean), got {other:?}").into()),
    }
}

#[test]
fn rejects_not_on_i32() -> TestResult {
    let expr = unary_op(
        UnaryOperator::Not,
        param_ref(0, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_expr(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnsupportedOperator {
            op,
            operand: PrimitiveType::I32,
        }) if op == "Not" => Ok(()),
        other => Err(format!("expected UnsupportedOperator(Not, I32), got {other:?}").into()),
    }
}

#[test]
fn double_negation_lowers_correctly() -> TestResult {
    // -(-x)
    let inner = unary_op(
        UnaryOperator::Neg,
        param_ref(0, PrimitiveType::I32),
        PrimitiveType::I32,
    );
    let expr = unary_op(UnaryOperator::Neg, inner, PrimitiveType::I32);
    validate(&build_unary_function(&expr, ValType::I32, ValType::I32)?)
}
