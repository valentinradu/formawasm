//! End-to-end coverage for the prelude's String built-in methods.
//!
//! Upstream ships `extern impl String { fn len(self) -> I32, ... }`
//! in `prelude.fv`; the backend wires each declared method to a
//! runtime helper through `module_lowering::prelude_helper_index`.
//! Wired so far: `len`, `is_empty`. Remaining (`slice`,
//! `starts_with`, `contains`, `byte_at`) land in subsequent commits.

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
        "formawasm-string-builtins-{}-{}-{}",
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
fn string_len_returns_byte_count() -> TestResult {
    // Source: `pub fn five() -> I32 { "hello".len() }`.
    // The prelude's `extern impl String` provides the method; the
    // backend wires it to `__str_len` which loads the `len` slot
    // from the string's `{ ptr, len }` header.
    let dir = scratch_dir("len")?;
    let main_path = dir.join("main.fv");
    std::fs::write(
        &main_path,
        "pub fn five() -> I32 {\n    \"hello\".len()\n}\n",
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
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let five = instance.get_typed_func::<(), (i32,)>(&mut store, "five")?;

    let (got,) = five.call(&mut store, ())?;
    if got != 5 {
        return Err(format!("five() = {got}, want 5").into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn string_is_empty_distinguishes_zero_length_from_nonzero() -> TestResult {
    // Source: two pubs returning `Boolean`, one over `""` (truthy) and
    // one over `"hi"` (falsy). Boolean maps to WIT `bool` at the
    // component boundary. The prelude's `is_empty` wires through
    // `__str_is_empty` which returns `len == 0` as i32 0/1.
    let dir = scratch_dir("is-empty")?;
    let main_path = dir.join("main.fv");
    std::fs::write(
        &main_path,
        "pub fn empty() -> Boolean { \"\".is_empty() }\n\npub fn full() -> Boolean { \"hi\".is_empty() }\n",
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
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let empty = instance.get_typed_func::<(), (bool,)>(&mut store, "empty")?;
    let full = instance.get_typed_func::<(), (bool,)>(&mut store, "full")?;

    let (got_empty,) = empty.call(&mut store, ())?;
    if !got_empty {
        return Err(format!("empty() = {got_empty}, want true").into());
    }
    let (got_full,) = full.call(&mut store, ())?;
    if got_full {
        return Err(format!("full() = {got_full}, want false").into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
