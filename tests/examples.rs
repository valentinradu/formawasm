//! End-to-end runner for the `examples/` directory.
//!
//! For each `examples/NN_*.fv` source, this test:
//!
//! 1. Parses + type-checks via `formalang::compile_to_ir_with_resolver`
//!    (filesystem-rooted at `examples/` so `use` statements resolve).
//! 2. Runs the canonical codegen pipeline (`Monomorphise`,
//!    `ResolveRefs`, `ClosureConversion`, `DeadCodeElimination`).
//! 3. Lowers to a Component-Model artifact via `WasmBackend::generate`.
//! 4. Instantiates the component under wasmtime's component runtime
//!    with `assert` wired to a host that aborts on `condition == false`.
//! 5. Calls the example's exported `run-checks` (no args, no return),
//!    relying on the embedded `assert(...)` calls to validate every
//!    expected output.
//!
//! A trap inside `run-checks` means at least one assertion failed —
//! the example produced an unexpected value. A failure earlier in the
//! pipeline surfaces a backend or frontend gap.
//!
//! The expectation is that every numbered `.fv` file in `examples/`
//! eventually passes. Examples that hit a known-but-not-yet-wired
//! feature trap surface as `Err(...)` — explicit failure, not silent
//! skip — so the limitations stay visible.

use std::path::PathBuf;

use formalang::{
    FileSystemResolver, Pipeline, compile_to_ir_with_resolver,
    ir::{ClosureConversionPass, DeadCodeEliminationPass, MonomorphisePass, ResolveReferencesPass},
    report_errors,
};
use formawasm::WasmBackend;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

/// State threaded into the wasmtime store. `assert` panics when its
/// argument is false; the panic is caught by wasmtime's trap
/// mechanism and surfaces as a typed `Trap` to the test caller.
#[derive(Default)]
struct HostState;

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
}

fn compile_example(name: &str) -> Result<Vec<u8>, TestError> {
    let path = examples_dir().join(name);
    let source = std::fs::read_to_string(&path)?;
    let resolver = FileSystemResolver::new(examples_dir());
    let module = compile_to_ir_with_resolver(&source, resolver).map_err(|errors| -> TestError {
        let report = report_errors(&errors, &source, name);
        format!("compile error in {name}:\n{report}").into()
    })?;
    let mut pipeline = Pipeline::new()
        .pass(MonomorphisePass::default())
        .pass(ResolveReferencesPass::new())
        .pass(ClosureConversionPass::new())
        .pass(DeadCodeEliminationPass::new());
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    Ok(bytes)
}

fn run_checks(name: &str) -> TestResult {
    let bytes = compile_example(name)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;

    let mut linker = Linker::<HostState>::new(&engine);
    // The prelude declares `pub extern fn assert(condition: Boolean)`.
    // Wired here as a host function that traps when the condition is
    // false, so failed assertions surface to the test caller as a
    // wasmtime trap.
    linker
        .root()
        .func_wrap("assert", |_store, (condition,): (bool,)| {
            if condition {
                Ok(())
            } else {
                Err(wasmtime::Error::msg(
                    "formalang assert(false) — example produced an unexpected value",
                ))
            }
        })?;
    // Wire example 16's extern fn / extern impl method imports.
    // Examples that don't reference these names ignore them; the
    // linker's `func_wrap` only enforces matches at instantiate
    // time, when wasmtime checks the imports the *component*
    // actually declares.
    linker
        .root()
        .func_wrap("host-double", |_store, (x,): (i32,)| {
            Ok((x.saturating_mul(2),))
        })?;
    linker
        .root()
        .func_wrap("host-log", |_store, (_msg,): (String,)| Ok(()))?;
    linker
        .root()
        .func_wrap("canvas-width", |_store, (): ()| Ok((640i32,)))?;
    linker
        .root()
        .func_wrap("canvas-height", |_store, (): ()| Ok((480i32,)))?;
    linker
        .root()
        .func_wrap("connection-open", |_store, (): ()| Ok((true,)))?;
    linker
        .root()
        .func_wrap("connection-close", |_store, (): ()| Ok(()))?;

    let mut store = Store::new(&engine, HostState);
    let instance = linker.instantiate(&mut store, &component)?;

    let run = instance
        .get_typed_func::<(), ()>(&mut store, "run-checks")
        .map_err(|e| -> TestError {
            format!("{name}: missing or wrong-shape `run-checks` export: {e}").into()
        })?;
    run.call(&mut store, ())
        .map_err(|e| -> TestError { format!("{name}: run-checks trapped: {e}").into() })?;
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// One #[test] per example so failures are reported per-file.
//
// As of this writing, the published formalang 0.0.4-beta has a few
// gaps (`self` parameter typing in inherent impls; `TypeParam` not
// fully eliminated by MonomorphisePass for certain Optional<T> shapes;
// tuple field-access through the type-checker) and the backend has a
// few un-wired surfaces (Closure / Trait / Struct as WIT-boundary
// types; `Literal::Path` lowering). Examples that hit these surface
// as failing tests below — the failure messages identify which gap
// each one trips.
// ────────────────────────────────────────────────────────────────────

#[test]
fn example_01_generics_box() -> TestResult {
    run_checks("01_generics_box.fv")
}
#[test]
fn example_02_generics_pair_result() -> TestResult {
    run_checks("02_generics_pair_result.fv")
}
#[test]
fn example_03_closure_capture() -> TestResult {
    run_checks("03_closure_capture.fv")
}
#[test]
fn example_04_higher_order() -> TestResult {
    run_checks("04_higher_order.fv")
}
#[test]
fn example_05_mut_param() -> TestResult {
    run_checks("05_mut_param.fv")
}
#[test]
fn example_06_sink_param() -> TestResult {
    run_checks("06_sink_param.fv")
}
#[test]
fn example_07_recursion_deep() -> TestResult {
    run_checks("07_recursion_deep.fv")
}
#[test]
fn example_08_trait_dispatch() -> TestResult {
    run_checks("08_trait_dispatch.fv")
}
#[test]
fn example_09_arrays_for_dict() -> TestResult {
    run_checks("09_arrays_for_dict.fv")
}
#[test]
fn example_10_optional_destructure_overload() -> TestResult {
    run_checks("10_optional_destructure_overload.fv")
}
#[test]
fn example_11_strings_path_regex() -> TestResult {
    run_checks("11_strings_path_regex.fv")
}
#[test]
fn example_12_numeric_primitives() -> TestResult {
    run_checks("12_numeric_primitives.fv")
}
#[test]
fn example_13_tuples() -> TestResult {
    run_checks("13_tuples.fv")
}
#[test]
fn example_14_array_destructure() -> TestResult {
    run_checks("14_array_destructure.fv")
}
#[test]
fn example_15_modules_inline() -> TestResult {
    run_checks("15_modules_inline.fv")
}
#[test]
fn example_16_extern_host() -> TestResult {
    run_checks("16_extern_host.fv")
}
#[test]
fn example_17_generic_fn() -> TestResult {
    run_checks("17_generic_fn.fv")
}
#[test]
fn example_18_trait_compose_multi() -> TestResult {
    run_checks("18_trait_compose_multi.fv")
}
#[test]
fn example_19_closure_fields_dispatch() -> TestResult {
    run_checks("19_closure_fields_dispatch.fv")
}
#[test]
fn example_20_match_advanced() -> TestResult {
    run_checks("20_match_advanced.fv")
}
