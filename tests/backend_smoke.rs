//! Smoke tests confirming the public type surface compiles and the
//! `Backend` trait is implemented for [`WasmBackend`].

use formawasm::{Backend, IrModule, WasmBackend, WasmBackendError};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[test]
fn wasmbackend_generate_returns_not_yet_implemented() -> TestResult {
    let backend = WasmBackend::new();
    let module = IrModule::new();
    match backend.generate(&module) {
        Err(WasmBackendError::NotYetImplemented) => Ok(()),
        Err(other) => Err(format!("unexpected error variant: {other:?}").into()),
        Ok(_) => Err("stub backend should not return Ok".into()),
    }
}
