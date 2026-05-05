//! End-to-end coverage for default parameter values.
//!
//! Upstream's IR-lowering pass substitutes default expressions at
//! every call site that omits a defaulted parameter, so by the
//! time `WasmBackend::generate` sees the IR, every `FunctionCall`
//! and `MethodCall` carries an args list of exactly the callee's
//! arity. The backend needs no awareness of defaults; this test
//! confirms the upstream substitution flows cleanly through to a
//! valid component.

use formalang::pipeline::Pipeline;
use formalang::{
    FileSystemResolver, compile_to_ir_with_resolver,
    ir::{ClosureConversionPass, DeadCodeEliminationPass, MonomorphisePass, ResolveReferencesPass},
};
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

use std::path::PathBuf;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn scratch_dir(label: &str) -> Result<PathBuf, TestError> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "formawasm-default-params-{}-{}-{}",
        std::process::id(),
        label,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

#[test]
fn default_param_is_filled_at_omitted_call_site() -> TestResult {
    // `add(a: I32, b: I32 = 10I32)` called as `add(a: 5)` should
    // pick up `b = 10`, returning 15. The default expression flows
    // through upstream IR-lowering's default-substitution pass; the
    // backend sees a regular two-arg call.
    let dir = scratch_dir("simple")?;
    let main_path = dir.join("main.fv");
    std::fs::write(
        &main_path,
        "pub fn add(a: I32, b: I32 = 10I32) -> I32 { a + b }\n\npub fn fifteen() -> I32 { add(a: 5I32) }\n",
    )?;
    let source = std::fs::read_to_string(&main_path)?;
    let resolver = FileSystemResolver::new(dir.clone());
    let module = compile_to_ir_with_resolver(&source, resolver)
        .map_err(|errors| format!("compile errors: {errors:?}"))?;

    let mut pipeline = Pipeline::new()
        .pass(MonomorphisePass::default())
        .pass(ResolveReferencesPass::new())
        .pass(ClosureConversionPass::new())
        .pass(DeadCodeEliminationPass::new());
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let mut linker = Linker::<()>::new(&engine);
    // formalang's prelude declares `pub extern fn assert(condition: Boolean)`;
    // every program parsed via `compile_to_ir_*` imports it. Wire to a host
    // function that traps on `false` so failed assertions surface as wasmtime
    // traps to the test caller.
    linker.root().func_wrap("assert", |_store, (cond,): (bool,)| {
        if cond { Ok(()) } else { Err(wasmtime::Error::msg("assert(false)")) }
    })?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let fifteen = instance.get_typed_func::<(), (i32,)>(&mut store, "fifteen")?;

    let (got,) = fifteen.call(&mut store, ())?;
    if got != 15 {
        return Err(format!("fifteen() = {got}, want 15").into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
