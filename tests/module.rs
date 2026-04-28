//! Tests that the empty module skeleton produces bytes that
//! `wasmparser::Validator` accepts.

use formawasm::module::{HEAP_PTR_GLOBAL_INDEX, MEMORY_INDEX, ModuleBuilder};
use wasmparser::{Validator, WasmFeatures};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<wasmparser::types::Types, TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    Ok(validator.validate_all(bytes)?)
}

#[test]
fn skeleton_validates() -> TestResult {
    let bytes = ModuleBuilder::new().finish();
    validate(&bytes).map(|_| ())
}

#[test]
fn skeleton_declares_one_memory_and_one_global() -> TestResult {
    let bytes = ModuleBuilder::new().finish();
    let types = validate(&bytes)?;
    let types = types.as_ref();
    if types.memory_count() != 1 {
        return Err(format!("expected 1 memory, got {}", types.memory_count()).into());
    }
    if types.global_count() != 1 {
        return Err(format!("expected 1 global, got {}", types.global_count()).into());
    }
    Ok(())
}

#[test]
fn skeleton_indexes_are_zero() -> TestResult {
    if MEMORY_INDEX != 0 || HEAP_PTR_GLOBAL_INDEX != 0 {
        return Err(format!(
            "expected MEMORY_INDEX=0 and HEAP_PTR_GLOBAL_INDEX=0, got {MEMORY_INDEX} and {HEAP_PTR_GLOBAL_INDEX}"
        )
        .into());
    }
    Ok(())
}
