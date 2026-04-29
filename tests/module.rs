//! Tests that the empty module skeleton and function-signature
//! emission produce bytes that `wasmparser::Validator` accepts.

use formawasm::module::{HEAP_PTR_GLOBAL_INDEX, MEMORY_INDEX, ModuleBuilder};
use wasm_encoder::ValType;
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

#[test]
fn declare_function_returns_zero_for_first_function() -> TestResult {
    let mut builder = ModuleBuilder::new();
    let idx = builder.declare_function(&[ValType::I32], &[ValType::I32]);
    if idx != 0 {
        return Err(format!("expected first function idx 0, got {idx}").into());
    }
    Ok(())
}

#[test]
fn declare_function_returns_increasing_indices() -> TestResult {
    let mut builder = ModuleBuilder::new();
    let a = builder.declare_function(&[], &[]);
    let b = builder.declare_function(&[ValType::I64], &[ValType::I64]);
    let c = builder.declare_function(&[ValType::F32, ValType::F32], &[ValType::F32]);
    if (a, b, c) != (0, 1, 2) {
        return Err(format!("expected (0,1,2), got ({a},{b},{c})").into());
    }
    Ok(())
}

#[test]
fn module_with_functions_validates() -> TestResult {
    let mut builder = ModuleBuilder::new();
    builder.declare_function(&[ValType::I32, ValType::I32], &[ValType::I32]);
    builder.declare_function(&[], &[ValType::I64]);
    let bytes = builder.finish();
    let types = validate(&bytes)?;
    let types = types.as_ref();
    if types.function_count() != 2 {
        return Err(format!("expected 2 functions, got {}", types.function_count()).into());
    }
    Ok(())
}

#[test]
fn function_signature_records_param_and_result_valtypes() -> TestResult {
    let mut builder = ModuleBuilder::new();
    builder.declare_function(&[ValType::I32, ValType::F64], &[ValType::I64]);
    let bytes = builder.finish();
    let types = validate(&bytes)?;
    let types = types.as_ref();
    let core_id = types.core_function_at(0);
    let func_ty = types.get(core_id).ok_or("no func type for index 0")?;
    let composite = match &func_ty.composite_type.inner {
        wasmparser::CompositeInnerType::Func(f) => f,
        wasmparser::CompositeInnerType::Array(_)
        | wasmparser::CompositeInnerType::Struct(_)
        | wasmparser::CompositeInnerType::Cont(_) => {
            return Err("expected Func composite type".into());
        }
    };
    if composite.params().len() != 2 || composite.results().len() != 1 {
        return Err(format!(
            "expected (2 params, 1 result), got ({}, {})",
            composite.params().len(),
            composite.results().len()
        )
        .into());
    }
    Ok(())
}
