//! End-to-end tests for `lower::lower_call_closure`.
//!
//! Each test compiles a small formalang program that builds a
//! closure value, calls it, and verifies the result under wasmtime.
//! Closure-conversion lifts the closure to a top-level `__closure<N>`
//! function; `module_lowering` registers it in the funcref table and
//! prepares the matching `call_indirect` type signature so the call
//! site dispatches correctly.

use formalang::compile_to_ir;
use formalang::ir::{ClosureConversionPass, DeadCodeEliminationPass, MonomorphisePass};
use formalang::pipeline::Pipeline;
use formawasm::module_lowering;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::{Engine, Instance, Module, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

fn run_module(source: &str, fn_name: &str) -> Result<i32, TestError> {
    let raw = compile_to_ir(source)
        .map_err(|e| -> TestError { format!("compile_to_ir: {e:?}").into() })?;
    let module = Pipeline::new()
        .pass(MonomorphisePass::default())
        .pass(ClosureConversionPass::new())
        .pass(DeadCodeEliminationPass::new())
        .run(raw)
        .map_err(|errs| -> TestError { format!("pipeline: {errs:?}").into() })?;
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, fn_name)?;
    f.call(&mut store, ()).map_err(Into::into)
}

#[test]
fn no_capture_closure_invocation_returns_arg_plus_one() -> TestResult {
    // pub fn run() -> I32 { let f: I32 -> I32 = |x: I32| x + 1; f(41) }
    let source = r"
        pub fn run() -> I32 {
            let f: I32 -> I32 = |x: I32| x + 1
            f(41)
        }
    ";
    let got = run_module(source, "run")?;
    if got != 42 {
        return Err(format!("got {got}, want 42").into());
    }
    Ok(())
}

#[test]
fn captured_value_flows_through_call_indirect() -> TestResult {
    // pub fn make_adder(n: I32) -> I32 -> I32 { |x: I32| x + n }
    // pub fn run() -> I32 { let add5: I32 -> I32 = make_adder(5); add5(37) }
    let source = r"
        pub fn make_adder(sink n: I32) -> (I32) -> I32 {
            |x: I32| x + n
        }
        pub fn run() -> I32 {
            let add5: I32 -> I32 = make_adder(5)
            add5(37)
        }
    ";
    let got = run_module(source, "run")?;
    if got != 42 {
        return Err(format!("got {got}, want 42").into());
    }
    Ok(())
}
