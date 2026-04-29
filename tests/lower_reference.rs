//! Tests for `lower::lower_reference` and `lower::lower_let_ref`.
//! Each test builds a small function whose body references a
//! parameter or a local binding, lowers it onto the function's
//! instruction sink, and runs `wasmparser::Validator` over the
//! resulting module.

use formalang::ast::PrimitiveType;
use formalang::ir::{BindingId, IrExpr, ReferenceTarget, ResolvedType};
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

#[test]
fn lowers_param_reference_to_local_get() -> TestResult {
    // fn identity(x: I32) -> I32 { x }
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);

    let expr = IrExpr::Reference {
        path: vec!["x".to_owned()],
        target: ReferenceTarget::Param(BindingId(0)),
        ty: primitive_ty(PrimitiveType::I32),
    };

    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut builder = ModuleBuilder::new();
    let mut body = Function::new(core::iter::empty());
    {
        let sink = &mut body.instructions();
        lower::lower_reference(&expr, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn lowers_let_ref_to_local_get() -> TestResult {
    // Simulate a function with one local at wasm index 1 (the params
    // are at 0..N; this function takes one i32 param so the local
    // sits at index 1). Body references the local via LetRef.
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0); // param
    bindings.insert(BindingId(1), 1); // let

    let expr = IrExpr::LetRef {
        name: "y".to_owned(),
        binding_id: BindingId(1),
        ty: primitive_ty(PrimitiveType::I32),
    };

    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut builder = ModuleBuilder::new();
    // Function takes one i32 param and declares one i32 local.
    let mut body = Function::new([(1_u32, ValType::I32)]);
    {
        let sink = &mut body.instructions();
        lower::lower_let_ref(&expr, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn lowers_local_reference_to_local_get() -> TestResult {
    // Reference with target=Local(BindingId) is the explicit-resolved
    // form for a local binding. Should behave identically to LetRef.
    let mut bindings = BindingMap::new();
    bindings.insert(BindingId(0), 0);
    bindings.insert(BindingId(1), 1);

    let expr = IrExpr::Reference {
        path: vec!["y".to_owned()],
        target: ReferenceTarget::Local(BindingId(1)),
        ty: primitive_ty(PrimitiveType::I32),
    };

    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut builder = ModuleBuilder::new();
    let mut body = Function::new([(1_u32, ValType::I32)]);
    {
        let sink = &mut body.instructions();
        lower::lower_reference(&expr, sink, &ctx)?;
        sink.end();
    }
    builder.declare_function_with_body(&[ValType::I32], &[ValType::I32], &body);
    validate(&builder.finish())
}

#[test]
fn rejects_unknown_binding_id() -> TestResult {
    let bindings = BindingMap::new();
    let expr = IrExpr::LetRef {
        name: "ghost".to_owned(),
        binding_id: BindingId(99),
        ty: primitive_ty(PrimitiveType::I32),
    };
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_let_ref(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnknownBinding(BindingId(99))) => Ok(()),
        other => Err(format!("expected UnknownBinding(99), got {other:?}").into()),
    }
}

#[test]
fn rejects_unresolved_reference_target() -> TestResult {
    let bindings = BindingMap::new();
    let expr = IrExpr::Reference {
        path: vec!["x".to_owned()],
        target: ReferenceTarget::Unresolved,
        ty: primitive_ty(PrimitiveType::I32),
    };
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_reference(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::UnresolvedReference) => Ok(()),
        other => Err(format!("expected UnresolvedReference, got {other:?}").into()),
    }
}

#[test]
fn module_let_reference_is_not_yet_implemented() -> TestResult {
    let bindings = BindingMap::new();
    let expr = IrExpr::Reference {
        path: vec!["MAX".to_owned()],
        target: ReferenceTarget::ModuleLet(formalang::ir::LetId(0)),
        ty: primitive_ty(PrimitiveType::I32),
    };
    let functions = FunctionMap::new();
    let ctx = LowerContext::new(&bindings, &functions);
    let mut body = Function::new(core::iter::empty());
    let result = lower::lower_reference(&expr, &mut body.instructions(), &ctx);
    match result {
        Err(LowerError::NotYetImplemented { what }) if what.contains("ModuleLet") => Ok(()),
        other => Err(format!("expected NotYetImplemented(ModuleLet), got {other:?}").into()),
    }
}

#[test]
fn binding_map_starts_empty() -> TestResult {
    let map = BindingMap::new();
    if !map.is_empty() {
        return Err(format!("expected empty map, got len {}", map.len()).into());
    }
    if map.get(BindingId(0)).is_some() {
        return Err("expected None for unknown id".into());
    }
    Ok(())
}
