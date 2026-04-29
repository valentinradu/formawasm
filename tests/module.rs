//! Tests that the empty module skeleton and function-signature
//! emission produce bytes that `wasmparser::Validator` accepts, plus
//! the bump-allocator runtime helper's behaviour under wasmtime.

use formawasm::module::{BUMP_ALLOCATOR_ALIGN, HEAP_PTR_GLOBAL_INDEX, MEMORY_INDEX, ModuleBuilder};
use wasm_encoder::ValType;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::{Engine, Instance, Module, Store};

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

// ── Bump-allocator helper ────────────────────────────────────────────

#[test]
fn declare_bump_allocator_returns_a_function_index_and_validates() -> TestResult {
    let mut builder = ModuleBuilder::new();
    let idx = builder.declare_bump_allocator();
    if builder.bump_allocator_index() != Some(idx) {
        return Err("bump_allocator_index disagrees with declare return value".into());
    }
    let bytes = builder.finish();
    validate(&bytes).map(|_| ())
}

#[test]
fn declare_bump_allocator_is_idempotent() -> TestResult {
    let mut builder = ModuleBuilder::new();
    let first = builder.declare_bump_allocator();
    let second = builder.declare_bump_allocator();
    if first != second {
        return Err(
            format!("expected same index on second call, got {first} then {second}").into(),
        );
    }
    let bytes = builder.finish();
    let types = validate(&bytes)?;
    if types.as_ref().function_count() != 1 {
        return Err(format!(
            "expected exactly 1 function after idempotent calls, got {}",
            types.as_ref().function_count(),
        )
        .into());
    }
    Ok(())
}

#[test]
fn bump_allocator_runs_and_returns_aligned_addresses() -> TestResult {
    let mut builder = ModuleBuilder::new();
    let idx = builder.declare_bump_allocator();
    builder.export_function("__alloc", idx);
    let bytes = builder.finish();
    validate(&bytes)?;

    let engine = Engine::default();
    let module = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[])?;
    let alloc = instance.get_typed_func::<i32, i32>(&mut store, "__alloc")?;

    // Each returned address must be a multiple of `BUMP_ALLOCATOR_ALIGN`.
    let align = i32::try_from(BUMP_ALLOCATOR_ALIGN)
        .map_err(|_| -> TestError { "BUMP_ALLOCATOR_ALIGN doesn't fit in i32".into() })?;

    let a = alloc.call(&mut store, 8)?;
    let b = alloc.call(&mut store, 8)?;
    let c = alloc.call(&mut store, 7)?;
    let d = alloc.call(&mut store, 1)?;

    if a != 0 {
        return Err(format!("first alloc(8) = {a}, want 0").into());
    }
    if b != 8 {
        return Err(format!("second alloc(8) = {b}, want 8").into());
    }
    // `c`'s call sees heap_ptr = 16 (already aligned), so c = 16; the
    // 7-byte allocation leaves the heap_ptr at 23. The next call rounds
    // back up to 24.
    if c != 16 {
        return Err(format!("alloc(7) = {c}, want 16").into());
    }
    if d != 24 {
        return Err(format!("alloc after unaligned bump = {d}, want 24").into());
    }
    for (label, addr) in [("a", a), ("b", b), ("c", c), ("d", d)] {
        if addr % align != 0 {
            return Err(format!("{label} = {addr} not aligned to {align}").into());
        }
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
