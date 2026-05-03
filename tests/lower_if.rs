//! Tests for `lower::lower_if`.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType,
};
use formalang::ir::{BindingId, IrExpr, IrSpan, ReferenceTarget, ResolvedType};
use formawasm::lower::{self, FunctionMap};
use formawasm::module::ModuleBuilder;
use wasm_encoder::ValType;
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

fn boolean_literal(b: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(b),
        ty: primitive_ty(PrimitiveType::Boolean),
        span: IrSpan::default(),
    }
}

fn param_ref(id: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::Reference {
        path: vec![format!("p{id}")],
        target: ReferenceTarget::Param(BindingId(id)),
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

fn if_expr(
    condition: IrExpr,
    then_branch: IrExpr,
    else_branch: Option<IrExpr>,
    ty: PrimitiveType,
) -> IrExpr {
    IrExpr::If {
        condition: Box::new(condition),
        then_branch: Box::new(then_branch),
        else_branch: else_branch.map(Box::new),
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

#[test]
fn if_else_with_constant_condition_returns_branch_value() -> TestResult {
    // if true { 1 } else { 0 }
    let expr = if_expr(
        boolean_literal(true),
        integer_literal(1, PrimitiveType::I32),
        Some(integer_literal(0, PrimitiveType::I32)),
        PrimitiveType::I32,
    );
    let body = lower::lower_function_body(&expr, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn if_else_with_param_condition() -> TestResult {
    // fn pick(p: I32) -> I32 { if p < 0 { 0 } else { p } }
    let condition = binary_op(
        BinaryOperator::Lt,
        param_ref(0, PrimitiveType::I32),
        integer_literal(0, PrimitiveType::I32),
        PrimitiveType::Boolean,
    );
    let expr = if_expr(
        condition,
        integer_literal(0, PrimitiveType::I32),
        Some(param_ref(0, PrimitiveType::I32)),
        PrimitiveType::I32,
    );
    let body =
        lower::lower_function_body(&expr, &[(BindingId(0), ValType::I32)], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn nested_if_else_resolves_correctly() -> TestResult {
    // if true { if false { 1 } else { 2 } } else { 3 }
    let inner = if_expr(
        boolean_literal(false),
        integer_literal(1, PrimitiveType::I32),
        Some(integer_literal(2, PrimitiveType::I32)),
        PrimitiveType::I32,
    );
    let outer = if_expr(
        boolean_literal(true),
        inner,
        Some(integer_literal(3, PrimitiveType::I32)),
        PrimitiveType::I32,
    );
    let body = lower::lower_function_body(&outer, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn if_with_i64_branches() -> TestResult {
    let expr = if_expr(
        boolean_literal(true),
        integer_literal(1_000_000_000_000, PrimitiveType::I64),
        Some(integer_literal(2_000_000_000_000, PrimitiveType::I64)),
        PrimitiveType::I64,
    );
    let body = lower::lower_function_body(&expr, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I64], &body);
    validate(&builder.finish())
}

#[test]
fn if_with_boolean_result() -> TestResult {
    // fn is_zero(p: I32) -> Boolean { if p == 0 { true } else { false } }
    let condition = binary_op(
        BinaryOperator::Eq,
        param_ref(0, PrimitiveType::I32),
        integer_literal(0, PrimitiveType::I32),
        PrimitiveType::Boolean,
    );
    let expr = if_expr(
        condition,
        boolean_literal(true),
        Some(boolean_literal(false)),
        PrimitiveType::Boolean,
    );
    let body =
        lower::lower_function_body(&expr, &[(BindingId(0), ValType::I32)], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
    validate(&builder.finish())
}
