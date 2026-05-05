#![cfg(any())] // TODO 0.0.4-beta migration: hand-built IR needs prelude-id seeding

//! Tests for the formalang → core-wasm type mapping.

use formalang::ast::PrimitiveType;
use formalang::ir::ResolvedType;
use formawasm::types::{self, TypeMapError};
use wasm_encoder::ValType;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

#[test]
fn integer_primitives_map_to_their_native_valtype() -> TestResult {
    let cases: [(PrimitiveType, ValType); 4] = [
        (PrimitiveType::I32, ValType::I32),
        (PrimitiveType::I64, ValType::I64),
        (PrimitiveType::F32, ValType::F32),
        (PrimitiveType::F64, ValType::F64),
    ];
    for (p, expected) in cases {
        match types::primitive_value_type(p)? {
            Some(vt) if vt == expected => {}
            other => return Err(format!("{p:?} mapped to {other:?}, expected {expected:?}").into()),
        }
    }
    Ok(())
}

#[test]
fn boolean_maps_to_i32() -> TestResult {
    match types::primitive_value_type(PrimitiveType::Boolean)? {
        Some(ValType::I32) => Ok(()),
        other => Err(format!("Boolean mapped to {other:?}, expected Some(I32)").into()),
    }
}

#[test]
fn never_maps_to_no_value() -> TestResult {
    match types::primitive_value_type(PrimitiveType::Never)? {
        None => Ok(()),
        other => Err(format!("Never mapped to {other:?}, expected None").into()),
    }
}

#[test]
fn string_maps_to_i32_pointer() -> TestResult {
    // Phase 2 mc9: strings live in linear memory as `{ ptr, len }`
    // headers, so the wasm value type is i32. Path / Regex share the
    // same in-body representation; the WIT boundary is what
    // distinguishes them.
    match types::primitive_value_type(PrimitiveType::String) {
        Ok(Some(ValType::I32)) => Ok(()),
        other => Err(format!("expected Ok(Some(I32)) for String, got {other:?}").into()),
    }
}

#[test]
fn path_maps_to_i32_pointer() -> TestResult {
    match types::primitive_value_type(PrimitiveType::Path) {
        Ok(Some(ValType::I32)) => Ok(()),
        other => Err(format!("expected Ok(Some(I32)) for Path, got {other:?}").into()),
    }
}

#[test]
fn regex_maps_to_i32_pointer() -> TestResult {
    match types::primitive_value_type(PrimitiveType::Regex) {
        Ok(Some(ValType::I32)) => Ok(()),
        other => Err(format!("expected Ok(Some(I32)) for Regex, got {other:?}").into()),
    }
}

#[test]
fn aggregate_types_are_not_yet_supported() -> TestResult {
    let array = ResolvedType::Array(Box::new(ResolvedType::Primitive(PrimitiveType::I32)));
    match types::resolved_value_type(&array) {
        Err(TypeMapError::NotYetSupported { kind }) if kind.contains("Array") => Ok(()),
        other => Err(format!("expected NotYetSupported(Array<T>), got {other:?}").into()),
    }
}

#[test]
fn result_types_for_no_return_is_empty() -> TestResult {
    let r = types::result_types(None)?;
    if r.is_empty() {
        Ok(())
    } else {
        Err(format!("expected empty result, got {r:?}").into())
    }
}

#[test]
fn result_types_for_never_is_empty() -> TestResult {
    let never = ResolvedType::Primitive(PrimitiveType::Never);
    let r = types::result_types(Some(&never))?;
    if r.is_empty() {
        Ok(())
    } else {
        Err(format!("expected empty result for Never, got {r:?}").into())
    }
}

#[test]
fn result_types_for_i64_is_one_i64() -> TestResult {
    let i64_ty = ResolvedType::Primitive(PrimitiveType::I64);
    let r = types::result_types(Some(&i64_ty))?;
    if r == vec![ValType::I64] {
        Ok(())
    } else {
        Err(format!("expected [I64], got {r:?}").into())
    }
}

#[test]
fn external_type_is_rejected_with_design_note_breadcrumb() -> TestResult {
    // Cross-module type lowering is upstream-blocked. The error
    // message embeds a path to the upstream design note so anyone
    // hitting this from the consumer side gets a one-hop
    // explanation. Confirm both the variant and the breadcrumb so
    // a refactor that drops the hint fails this test loudly.
    let external = ResolvedType::External {
        module_path: vec!["helper".to_owned()],
        name: "Helper".to_owned(),
        kind: formalang::ir::ImportedKind::Struct,
        type_args: Vec::new(),
    };
    match types::resolved_value_type(&external) {
        Err(TypeMapError::NotYetSupported { kind })
            if kind.contains("External(Helper)")
                && kind.contains("upstream invariant violation") =>
        {
            Ok(())
        }
        other => {
            Err(format!("expected NotYetSupported with External breadcrumb, got {other:?}").into())
        }
    }
}
