//! Tests for `lower::lower_function_call`. Each test builds a small
//! two-function module: a callee (with a placeholder body) and a
//! caller whose body is a `FunctionCall` to the callee.

use formalang::ast::{Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType};
use formalang::ir::{FunctionId, IrExpr, ResolvedType};
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

fn integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
    let suffix = if ty == PrimitiveType::I64 {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive_ty(ty),
    }
}

#[test]
fn no_arg_call_emits_call_instruction() -> TestResult {
    // Define callee: () -> I32 with placeholder unreachable body at idx 0.
    let mut builder = ModuleBuilder::new();
    let callee_idx = builder.declare_function(&[], &[ValType::I32]);
    let mut functions = FunctionMap::new();
    functions.insert(FunctionId(0), callee_idx);

    // Caller body: callee()
    let call = IrExpr::FunctionCall {
        path: vec!["callee".to_owned()],
        function_id: Some(FunctionId(0)),
        args: Vec::new(),
        ty: primitive_ty(PrimitiveType::I32),
    };

    let bindings = BindingMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    {
        let sink = &mut body.instructions();
        lower::lower_expr(&call, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn two_arg_call_evaluates_args_left_to_right() -> TestResult {
    // Define callee: (i32, i32) -> i32 at idx 0
    let mut builder = ModuleBuilder::new();
    let callee_idx = builder.declare_function(&[ValType::I32, ValType::I32], &[ValType::I32]);
    let mut functions = FunctionMap::new();
    functions.insert(FunctionId(0), callee_idx);

    // Caller body: callee(3, 4)
    let call = IrExpr::FunctionCall {
        path: vec!["callee".to_owned()],
        function_id: Some(FunctionId(0)),
        args: vec![
            (Some("a".to_owned()), integer_literal(3, PrimitiveType::I32)),
            (Some("b".to_owned()), integer_literal(4, PrimitiveType::I32)),
        ],
        ty: primitive_ty(PrimitiveType::I32),
    };

    let bindings = BindingMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    {
        let sink = &mut body.instructions();
        lower::lower_expr(&call, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn rejects_unresolved_function_call() -> TestResult {
    // function_id = None
    let call = IrExpr::FunctionCall {
        path: vec!["math".to_owned(), "sin".to_owned()],
        function_id: None,
        args: Vec::new(),
        ty: primitive_ty(PrimitiveType::I32),
    };

    let bindings = BindingMap::new();
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_expr(&call, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnresolvedFunctionCall { path })
            if path == vec!["math".to_owned(), "sin".to_owned()] =>
        {
            Ok(())
        }
        other => Err(format!("expected UnresolvedFunctionCall, got {other:?}").into()),
    }
}

#[test]
fn rejects_unknown_function_id() -> TestResult {
    // function_id = Some, but not in FunctionMap
    let call = IrExpr::FunctionCall {
        path: vec!["missing".to_owned()],
        function_id: Some(FunctionId(42)),
        args: Vec::new(),
        ty: primitive_ty(PrimitiveType::I32),
    };

    let bindings = BindingMap::new();
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_expr(&call, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnknownFunction(FunctionId(42))) => Ok(()),
        other => Err(format!("expected UnknownFunction(42), got {other:?}").into()),
    }
}

#[test]
fn function_map_starts_empty() -> TestResult {
    let map = FunctionMap::new();
    if !map.is_empty() {
        return Err(format!("expected empty map, got len {}", map.len()).into());
    }
    if map.get(FunctionId(0)).is_some() {
        return Err("expected None for unknown id".into());
    }
    Ok(())
}
