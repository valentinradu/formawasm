//! Tests for `layout::plan_array`.
//!
//! Cover the three primitive element-size classes (1/4/8 byte), an
//! aggregate (struct) element which collapses to a 4-byte pointer, and
//! the `Never` rejection. Header size/alignment is fixed at 12/4 for
//! every array regardless of element type.

use formalang::ast::PrimitiveType;
use formalang::ir::{IrModule, ResolvedType, StructId};
use formawasm::layout::{self, ARRAY_HEADER_ALIGN, ARRAY_HEADER_SIZE, ArrayLayout, LayoutError};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn assert_array_layout(actual: &ArrayLayout, element_size: u32, element_align: u32) -> TestResult {
    if actual.header_size != ARRAY_HEADER_SIZE {
        return Err(format!(
            "header_size: got {}, want {ARRAY_HEADER_SIZE}",
            actual.header_size
        )
        .into());
    }
    if actual.header_align != ARRAY_HEADER_ALIGN {
        return Err(format!(
            "header_align: got {}, want {ARRAY_HEADER_ALIGN}",
            actual.header_align
        )
        .into());
    }
    if actual.element_size != element_size {
        return Err(format!(
            "element_size: got {}, want {element_size}",
            actual.element_size
        )
        .into());
    }
    if actual.element_align != element_align {
        return Err(format!(
            "element_align: got {}, want {element_align}",
            actual.element_align
        )
        .into());
    }
    Ok(())
}

#[test]
fn array_of_i32_has_four_byte_elements() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_array(&primitive(PrimitiveType::I32), &module)?;
    assert_array_layout(&layout, 4, 4)
}

#[test]
fn array_of_i64_has_eight_byte_elements() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_array(&primitive(PrimitiveType::I64), &module)?;
    assert_array_layout(&layout, 8, 8)
}

#[test]
fn array_of_boolean_has_one_byte_elements() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_array(&primitive(PrimitiveType::Boolean), &module)?;
    assert_array_layout(&layout, 1, 1)
}

#[test]
fn array_of_struct_uses_pointer_sized_elements() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_array(&ResolvedType::Struct(StructId(0)), &module)?;
    assert_array_layout(&layout, 4, 4)
}

#[test]
fn array_of_never_is_not_yet_supported() -> TestResult {
    let module = IrModule::new();
    match layout::plan_array(&primitive(PrimitiveType::Never), &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "Never" => Ok(()),
        other => Err(format!("expected NotYetSupported(Never), got {other:?}").into()),
    }
}
