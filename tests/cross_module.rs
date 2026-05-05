//! End-to-end coverage for cross-module compilation.
//!
//! Upstream's `MonomorphisePass` inlines imported items into the
//! entry-point `IrModule`, so by the time `WasmBackend::generate`
//! sees the IR, every `ResolvedType::External` has been rewritten
//! to a local `Struct(StructId)` / `Enum(EnumId)` reference. This
//! test confirms the full path: write a helper module on disk,
//! `use` it from a main module, run `compile_to_ir_with_resolver`,
//! generate the component, instantiate under wasmtime.

use std::path::PathBuf;

use formalang::{
    FileSystemResolver, Pipeline, compile_to_ir_with_resolver,
    ir::{ClosureConversionPass, DeadCodeEliminationPass, MonomorphisePass, ResolveReferencesPass},
};
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn scratch_dir(label: &str) -> Result<PathBuf, TestError> {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "formawasm-cross-module-{}-{}-{}",
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
fn imported_struct_runs_under_wasmtime() -> TestResult {
    let dir = scratch_dir("imported-struct")?;
    let helper_path = dir.join("helper.fv");
    let main_path = dir.join("main.fv");
    std::fs::write(&helper_path, "pub struct Point { x: I32, y: I32 }\n")?;
    std::fs::write(
        &main_path,
        "use helper::Point\n\npub fn make() -> I32 {\n    Point(x: 3I32, y: 4I32).x + Point(x: 3I32, y: 4I32).y\n}\n",
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
    let make = instance.get_typed_func::<(), (i32,)>(&mut store, "make")?;

    let (got,) = make.call(&mut store, ())?;
    if got != 7 {
        return Err(format!("make() = {got}, want 7").into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
