//! Round-trip a `string -> string` export through the full
//! `WasmBackend::generate` pipeline (pre-flight + survey + core
//! lowering + WIT + component wrap) and instantiate it under
//! wasmtime's component runtime.
//!
//! The exported function `greet` takes a `string` parameter and
//! returns the concatenation `"hello, " + name`. Verifying that the
//! component-runtime call returns the right `String` confirms the
//! canonical-ABI shape (`{ ptr, len }` header at offset 0 / 4 of
//! linear memory) really does match what the host expects when
//! lifting our internal string-header layout across the boundary.

use formalang::ast::{BinaryOperator, Literal, ParamConvention, PrimitiveType};
use formalang::ir::{
    BindingId, IrExpr, IrFunction, IrFunctionParam, IrModule, ReferenceTarget, ResolvedType,
};
use formalang::pipeline::Pipeline;
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn string_literal(text: &str) -> IrExpr {
    IrExpr::Literal {
        value: Literal::String(text.to_owned()),
        ty: primitive(PrimitiveType::String),
    }
}

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

#[test]
fn string_to_string_export_round_trips_through_component_runtime() -> TestResult {
    let name_binding = BindingId(0);
    let body = IrExpr::BinaryOp {
        left: Box::new(string_literal("hello, ")),
        right: Box::new(IrExpr::Reference {
            path: vec!["name".to_owned()],
            target: ReferenceTarget::Param(name_binding),
            ty: primitive(PrimitiveType::String),
        }),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::String),
    };
    let greet = IrFunction {
        name: "greet".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: name_binding,
            name: "name".to_owned(),
            external_label: None,
            ty: Some(primitive(PrimitiveType::String)),
            default: None,
            convention: ParamConvention::Let,
        }],
        return_type: Some(primitive(PrimitiveType::String)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };
    let mut module = IrModule::new();
    module.functions.push(greet);

    let mut pipeline = Pipeline::new();
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let greet_fn = instance.get_typed_func::<(String,), (String,)>(&mut store, "greet")?;

    let (result,) = greet_fn.call(&mut store, ("world".to_owned(),))?;
    if result != "hello, world" {
        return Err(format!("got {result:?}, want \"hello, world\"").into());
    }
    Ok(())
}
