//! Phase 4 milestone — host-provided extern called from a formalang
//! function, wired through wasmtime's component-runtime Linker.
//!
//! Hand-builds an [`IrModule`] containing one `extern fn host_double(n:
//! I32) -> I32` declaration and one local `fn call_host(n: I32) -> I32
//! { host_double(n) }`. Runs the module through the full pipeline:
//! WIT-side `import` line, core-wasm function import under
//! `cm32p2`, component wrap. Instantiates under wasmtime with a
//! `Linker` that maps `host-double` to `|n| 2 * n` and confirms
//! `call_host(21) == 42`.
//!
//! The cross-module `ResolvedType::External` lowering is not
//! exercised here — the language has no `use`-syntax driver
//! reaching the backend yet, so that path stays a known restriction
//! until a real consumer needs it.

use formalang::ast::{ExternAbi, ParamConvention, PrimitiveType};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget,
    ResolvedType,
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

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

fn host_double_extern() -> IrFunction {
    IrFunction {
        name: "host_double".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "n".to_owned(),
            external_label: None,
            ty: Some(primitive(PrimitiveType::I32)),
            default: None,
            convention: ParamConvention::Let,
            span: IrSpan::default(),
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: None,
        extern_abi: Some(ExternAbi::C),
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn call_host_function() -> IrFunction {
    let n_ref = IrExpr::Reference {
        path: vec!["n".to_owned()],
        target: ReferenceTarget::Param(BindingId(0)),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let body = IrExpr::FunctionCall {
        path: vec!["host_double".to_owned()],
        function_id: Some(FunctionId(0)),
        args: vec![(None, n_ref)],
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    IrFunction {
        name: "call_host".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "n".to_owned(),
            external_label: None,
            ty: Some(primitive(PrimitiveType::I32)),
            default: None,
            convention: ParamConvention::Let,
            span: IrSpan::default(),
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_module() -> IrModule {
    let mut module = IrModule::new();
    // Function 0: extern fn host_double — declared as a wasm import,
    // wired by the host-side Linker before instantiation.
    module.functions.push(host_double_extern());
    // Function 1: local fn call_host — calls FunctionId(0) which the
    // FunctionMap resolves to the import's wasm-function index.
    module.functions.push(call_host_function());
    module
}

#[test]
fn host_provided_extern_resolves_through_component_runtime() -> TestResult {
    let module = build_module();
    let mut pipeline = Pipeline::new();
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;

    let mut linker = Linker::<()>::new(&engine);
    // World-level imports show up at the linker's root namespace
    // under their kebab-case WIT name — `host-double` here.
    linker
        .root()
        .func_wrap("host-double", |_store, (n,): (i32,)| Ok((2_i32 * n,)))?;

    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let call_host = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "call-host")?;

    let (got,) = call_host.call(&mut store, (21,))?;
    if got != 42 {
        return Err(format!("call_host(21) = {got}, want 42").into());
    }
    Ok(())
}
