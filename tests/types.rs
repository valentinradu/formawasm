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
fn string_is_not_yet_supported() -> TestResult {
    match types::primitive_value_type(PrimitiveType::String) {
        Err(TypeMapError::NotYetSupported { kind }) if kind == "String" => Ok(()),
        other => Err(format!("expected NotYetSupported(String), got {other:?}").into()),
    }
}

#[test]
fn path_is_not_yet_supported() -> TestResult {
    match types::primitive_value_type(PrimitiveType::Path) {
        Err(TypeMapError::NotYetSupported { kind }) if kind == "Path" => Ok(()),
        other => Err(format!("expected NotYetSupported(Path), got {other:?}").into()),
    }
}

#[test]
fn regex_is_not_yet_supported() -> TestResult {
    match types::primitive_value_type(PrimitiveType::Regex) {
        Err(TypeMapError::NotYetSupported { kind }) if kind == "Regex" => Ok(()),
        other => Err(format!("expected NotYetSupported(Regex), got {other:?}").into()),
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
