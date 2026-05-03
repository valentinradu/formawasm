//! End-to-end coverage for the prelude's String built-in methods.
//!
//! Upstream ships `extern impl String { fn len(self) -> I32, ... }`
//! in `prelude.fv`; the backend wires each declared method to a
//! runtime helper through `module_lowering::prelude_helper_index`.
//! Wired so far: `len`, `is_empty`, `byte_at`, `slice`,
//! `starts_with`. Remaining (`contains`) lands in a subsequent
//! commit.

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

#[test]
fn string_byte_at_returns_byte_value() -> TestResult {
    // Source: `pub fn h() -> I32 { "hello".byte_at(0I32) }` returns
    // 'h' = 0x68 = 104. The prelude's `byte_at` wires through
    // `__str_byte_at`, which loads the `ptr` slot, adds the index,
    // and reads one zero-extended byte. Out-of-range traps; we test
    // an in-range index here.
    let dir = scratch_dir("byte-at")?;
    let main_path = dir.join("main.fv");
    std::fs::write(
        &main_path,
        "pub fn h() -> I32 { \"hello\".byte_at(0I32) }\n\npub fn o() -> I32 { \"hello\".byte_at(4I32) }\n",
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
    let h = instance.get_typed_func::<(), (i32,)>(&mut store, "h")?;
    let o = instance.get_typed_func::<(), (i32,)>(&mut store, "o")?;

    let (got_h,) = h.call(&mut store, ())?;
    if got_h != i32::from(b'h') {
        return Err(format!("h() = {got_h}, want {}", b'h').into());
    }
    let (got_o,) = o.call(&mut store, ())?;
    if got_o != i32::from(b'o') {
        return Err(format!("o() = {got_o}, want {}", b'o').into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn string_slice_returns_zero_copy_substring() -> TestResult {
    // Source: `pub fn ell_len() -> I32 { "hello".slice(1I32, 4I32).len() }`
    // returns 3 (length of "ell"). Round-tripping a sliced string
    // back through `.len()` exercises the freshly-allocated header
    // — confirming `len = end - start` was stored correctly. Reading
    // a byte at offset 0 of the slice picks up the 'e' (= 'h' + 1
    // offset into the source buffer).
    let dir = scratch_dir("slice")?;
    let main_path = dir.join("main.fv");
    std::fs::write(
        &main_path,
        "pub fn ell_len() -> I32 { \"hello\".slice(1I32, 4I32).len() }\n\npub fn first() -> I32 { \"hello\".slice(1I32, 4I32).byte_at(0I32) }\n",
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
    let ell_len = instance.get_typed_func::<(), (i32,)>(&mut store, "ell-len")?;
    let first = instance.get_typed_func::<(), (i32,)>(&mut store, "first")?;

    let (got_len,) = ell_len.call(&mut store, ())?;
    if got_len != 3 {
        return Err(format!("ell_len() = {got_len}, want 3").into());
    }
    let (got_first,) = first.call(&mut store, ())?;
    if got_first != i32::from(b'e') {
        return Err(format!("first() = {got_first}, want {}", b'e').into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn string_starts_with_distinguishes_prefix_from_non_prefix() -> TestResult {
    // Source: three pubs returning Boolean. `"hello".starts_with("he")`
    // is true, `"hello".starts_with("ho")` is false (length matches
    // but bytes differ at position 1), `"hi".starts_with("hello")` is
    // false (prefix longer than source short-circuits to 0).
    let dir = scratch_dir("starts-with")?;
    let main_path = dir.join("main.fv");
    std::fs::write(
        &main_path,
        "pub fn matches() -> Boolean { \"hello\".starts_with(\"he\") }\n\npub fn diverges() -> Boolean { \"hello\".starts_with(\"ho\") }\n\npub fn too_long() -> Boolean { \"hi\".starts_with(\"hello\") }\n",
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
    let matches = instance.get_typed_func::<(), (bool,)>(&mut store, "matches")?;
    let diverges = instance.get_typed_func::<(), (bool,)>(&mut store, "diverges")?;
    let too_long = instance.get_typed_func::<(), (bool,)>(&mut store, "too-long")?;

    let (got_match,) = matches.call(&mut store, ())?;
    if !got_match {
        return Err(format!("matches() = {got_match}, want true").into());
    }
    let (got_div,) = diverges.call(&mut store, ())?;
    if got_div {
        return Err(format!("diverges() = {got_div}, want false").into());
    }
    let (got_long,) = too_long.call(&mut store, ())?;
    if got_long {
        return Err(format!("too_long() = {got_long}, want false").into());
    }

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
