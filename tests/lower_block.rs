//! Tests for `lower::lower_block` and `lower::lower_function_body`.
//!
//! Each test builds a small `IrExpr::Block` body, lowers the whole
//! function via `lower_function_body`, and validates the assembled
//! module with `wasmparser`.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType,
};
use formalang::ir::{BindingId, IrBlockStatement, IrExpr, IrSpan, ReferenceTarget, ResolvedType};
use formawasm::lower::{self, FunctionMap, LowerError};
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

fn let_ref(id: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::LetRef {
        name: format!("v{id}"),
        binding_id: BindingId(id),
        ty: primitive_ty(ty),
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

#[test]
fn empty_block_with_literal_result() -> TestResult {
    // { 42 }
    let block = IrExpr::Block {
        statements: Vec::new(),
        result: Box::new(integer_literal(42, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body = lower::lower_function_body(&block, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn let_binding_then_ref() -> TestResult {
    // { let x: I32 = 7; x }
    let block = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id: BindingId(0),
            name: "x".to_owned(),
            mutable: false,
            ty: Some(primitive_ty(PrimitiveType::I32)),
            value: integer_literal(7, PrimitiveType::I32),
            span: IrSpan::default(),
        }],
        result: Box::new(let_ref(0, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body = lower::lower_function_body(&block, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn let_uses_param_then_returns_arithmetic() -> TestResult {
    // fn f(p: I32) -> I32 { let doubled: I32 = p + p; doubled }
    let block = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id: BindingId(1),
            name: "doubled".to_owned(),
            mutable: false,
            ty: Some(primitive_ty(PrimitiveType::I32)),
            value: binary_op(
                BinaryOperator::Add,
                param_ref(0, PrimitiveType::I32),
                param_ref(0, PrimitiveType::I32),
                PrimitiveType::I32,
            ),
            span: IrSpan::default(),
        }],
        result: Box::new(let_ref(1, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body =
        lower::lower_function_body(&block, &[(BindingId(0), ValType::I32)], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn multiple_lets_with_dependencies() -> TestResult {
    // { let a: I32 = 3; let b: I32 = a + 1; let c: I32 = b * a; c }
    let block = IrExpr::Block {
        statements: vec![
            IrBlockStatement::Let {
                binding_id: BindingId(0),
                name: "a".to_owned(),
                mutable: false,
                ty: Some(primitive_ty(PrimitiveType::I32)),
                value: integer_literal(3, PrimitiveType::I32),
                span: IrSpan::default(),
            },
            IrBlockStatement::Let {
                binding_id: BindingId(1),
                name: "b".to_owned(),
                mutable: false,
                ty: Some(primitive_ty(PrimitiveType::I32)),
                value: binary_op(
                    BinaryOperator::Add,
                    let_ref(0, PrimitiveType::I32),
                    integer_literal(1, PrimitiveType::I32),
                    PrimitiveType::I32,
                ),
                span: IrSpan::default(),
            },
            IrBlockStatement::Let {
                binding_id: BindingId(2),
                name: "c".to_owned(),
                mutable: false,
                ty: Some(primitive_ty(PrimitiveType::I32)),
                value: binary_op(
                    BinaryOperator::Mul,
                    let_ref(1, PrimitiveType::I32),
                    let_ref(0, PrimitiveType::I32),
                    PrimitiveType::I32,
                ),
                span: IrSpan::default(),
            },
        ],
        result: Box::new(let_ref(2, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body = lower::lower_function_body(&block, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn expr_statement_is_dropped_before_result() -> TestResult {
    // { 99; 7 } -- the 99 is evaluated then dropped; result is 7.
    let block = IrExpr::Block {
        statements: vec![IrBlockStatement::Expr(integer_literal(
            99,
            PrimitiveType::I32,
        ))],
        result: Box::new(integer_literal(7, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body = lower::lower_function_body(&block, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn assign_statement_is_not_yet_implemented() -> TestResult {
    let block = IrExpr::Block {
        statements: vec![IrBlockStatement::Assign {
            target: param_ref(0, PrimitiveType::I32),
            value: integer_literal(0, PrimitiveType::I32),
            span: IrSpan::default(),
        }],
        result: Box::new(integer_literal(0, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    match lower::lower_function_body(&block, &[(BindingId(0), ValType::I32)], &FunctionMap::new()) {
        Err(LowerError::NotYetImplemented { what }) if what.contains("Assign") => Ok(()),
        other => Err(format!("expected NotYetImplemented(Assign), got {other:?}").into()),
    }
}

#[test]
fn nested_block_inside_block_resolves_correctly() -> TestResult {
    // { let x: I32 = { let y: I32 = 4; y + y }; x }
    let inner_block = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id: BindingId(0),
            name: "y".to_owned(),
            mutable: false,
            ty: Some(primitive_ty(PrimitiveType::I32)),
            value: integer_literal(4, PrimitiveType::I32),
            span: IrSpan::default(),
        }],
        result: Box::new(binary_op(
            BinaryOperator::Add,
            let_ref(0, PrimitiveType::I32),
            let_ref(0, PrimitiveType::I32),
            PrimitiveType::I32,
        )),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let outer_block = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id: BindingId(1),
            name: "x".to_owned(),
            mutable: false,
            ty: Some(primitive_ty(PrimitiveType::I32)),
            value: inner_block,
            span: IrSpan::default(),
        }],
        result: Box::new(let_ref(1, PrimitiveType::I32)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body = lower::lower_function_body(&outer_block, &[], &FunctionMap::new())?;
    let mut builder = ModuleBuilder::new();
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}
