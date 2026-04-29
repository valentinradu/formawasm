//! Tests for `layout::plan_range`.
//!
//! Cover the four numeric primitive bound types (`I32` / `I64` /
//! `F32` / `F64`), `Boolean` (technically valid even if semantically
//! odd), and the rejection paths: `Never`, the heap-typed primitives,
//! and aggregate-bound ranges.

use formalang::ast::PrimitiveType;
use formalang::ir::{IrModule, ResolvedType, StructId};
use formawasm::layout::{self, LayoutError, RangeLayout};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn assert_range_layout(actual: &RangeLayout, expected: RangeLayout) -> TestResult {
    if actual != &expected {
        return Err(format!("layout: got {actual:?}, want {expected:?}").into());
    }
    Ok(())
}

#[test]
fn range_of_i32_is_two_four_byte_bounds() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_range(&primitive(PrimitiveType::I32), &module)?;
    assert_range_layout(
        &layout,
        RangeLayout {
            size: 8,
            align: 4,
            bound_size: 4,
            bound_align: 4,
            end_offset: 4,
        },
    )
}

#[test]
fn range_of_i64_is_two_eight_byte_bounds() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_range(&primitive(PrimitiveType::I64), &module)?;
    assert_range_layout(
        &layout,
        RangeLayout {
            size: 16,
            align: 8,
            bound_size: 8,
            bound_align: 8,
            end_offset: 8,
        },
    )
}

#[test]
fn range_of_f32_matches_i32_layout() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_range(&primitive(PrimitiveType::F32), &module)?;
    assert_range_layout(
        &layout,
        RangeLayout {
            size: 8,
            align: 4,
            bound_size: 4,
            bound_align: 4,
            end_offset: 4,
        },
    )
}

#[test]
fn range_of_f64_matches_i64_layout() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_range(&primitive(PrimitiveType::F64), &module)?;
    assert_range_layout(
        &layout,
        RangeLayout {
            size: 16,
            align: 8,
            bound_size: 8,
            bound_align: 8,
            end_offset: 8,
        },
    )
}

#[test]
fn range_of_boolean_is_two_one_byte_bounds() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_range(&primitive(PrimitiveType::Boolean), &module)?;
    assert_range_layout(
        &layout,
        RangeLayout {
            size: 2,
            align: 1,
            bound_size: 1,
            bound_align: 1,
            end_offset: 1,
        },
    )
}

#[test]
fn range_of_never_is_not_yet_supported() -> TestResult {
    let module = IrModule::new();
    match layout::plan_range(&primitive(PrimitiveType::Never), &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "Never" => Ok(()),
        other => Err(format!("expected NotYetSupported(Never), got {other:?}").into()),
    }
}

#[test]
fn range_of_string_is_not_yet_supported() -> TestResult {
    let module = IrModule::new();
    match layout::plan_range(&primitive(PrimitiveType::String), &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "String" => Ok(()),
        other => Err(format!("expected NotYetSupported(String), got {other:?}").into()),
    }
}

#[test]
fn range_of_struct_is_not_yet_supported() -> TestResult {
    let module = IrModule::new();
    match layout::plan_range(&ResolvedType::Struct(StructId(0)), &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind.starts_with("Range<Struct(") => Ok(()),
        other => Err(format!("expected NotYetSupported(Range<Struct...>), got {other:?}").into()),
    }
}
