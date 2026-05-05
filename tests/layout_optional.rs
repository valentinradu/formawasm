#![cfg(any())] // TODO 0.0.4-beta migration: hand-built IR needs prelude-id seeding

//! Tests for `layout::plan_optional`.
//!
//! Cover the four numeric primitives (`I32` / `I64` / `F32` / `F64`),
//! `Boolean` (1-byte payload, drives the post-tag padding decision),
//! `Optional<Never>` (the nil-only static type of the `nil` literal),
//! aggregate payloads (struct collapses to a 4-byte pointer), and the
//! rejection paths for heap-typed primitives, nested optionals, and
//! container payloads not yet supported.

use formalang::ast::PrimitiveType;
use formalang::ir::{IrModule, ResolvedType, StructId};
use formawasm::layout::{self, LayoutError, OPTIONAL_TAG_ALIGN, OPTIONAL_TAG_SIZE, OptionalLayout};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn assert_optional_layout(actual: &OptionalLayout, expected: OptionalLayout) -> TestResult {
    if actual != &expected {
        return Err(format!("layout: got {actual:?}, want {expected:?}").into());
    }
    Ok(())
}

#[test]
fn optional_of_never_is_tag_only() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::Never), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: OPTIONAL_TAG_SIZE,
            align: OPTIONAL_TAG_ALIGN,
            tag_offset: 0,
            payload_offset: OPTIONAL_TAG_SIZE,
            payload_size: 0,
            payload_align: 1,
        },
    )
}

#[test]
fn optional_of_i32_packs_into_eight_bytes() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::I32), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 8,
            align: 4,
            tag_offset: 0,
            payload_offset: 4,
            payload_size: 4,
            payload_align: 4,
        },
    )
}

#[test]
fn optional_of_i64_pads_payload_to_eight_byte_alignment() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::I64), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 16,
            align: 8,
            tag_offset: 0,
            payload_offset: 8,
            payload_size: 8,
            payload_align: 8,
        },
    )
}

#[test]
fn optional_of_f32_matches_i32_layout() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::F32), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 8,
            align: 4,
            tag_offset: 0,
            payload_offset: 4,
            payload_size: 4,
            payload_align: 4,
        },
    )
}

#[test]
fn optional_of_f64_matches_i64_layout() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::F64), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 16,
            align: 8,
            tag_offset: 0,
            payload_offset: 8,
            payload_size: 8,
            payload_align: 8,
        },
    )
}

#[test]
fn optional_of_boolean_payload_sits_directly_after_tag() -> TestResult {
    // Boolean is 1-byte 1-aligned, so the payload starts right after
    // the 4-byte tag with no padding. The total still rounds up to the
    // tag's 4-byte alignment so consecutive optionals remain aligned.
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::Boolean), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 8,
            align: 4,
            tag_offset: 0,
            payload_offset: 4,
            payload_size: 1,
            payload_align: 1,
        },
    )
}

#[test]
fn optional_of_struct_uses_pointer_payload() -> TestResult {
    let module = IrModule::new();
    let layout = layout::plan_optional(&ResolvedType::Struct(StructId(0)), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 8,
            align: 4,
            tag_offset: 0,
            payload_offset: 4,
            payload_size: 4,
            payload_align: 4,
        },
    )
}

#[test]
fn optional_of_string_uses_pointer_payload() -> TestResult {
    // Phase 2 mc13: String / Path / Regex payloads collapse to a
    // 4-byte pointer to their `{ ptr, len }` header, exactly like
    // every other aggregate inner type.
    let module = IrModule::new();
    let layout = layout::plan_optional(&primitive(PrimitiveType::String), &module)?;
    assert_optional_layout(
        &layout,
        OptionalLayout {
            size: 8,
            align: 4,
            tag_offset: 0,
            payload_offset: 4,
            payload_size: 4,
            payload_align: 4,
        },
    )
}

#[test]
fn optional_of_optional_is_not_yet_supported() -> TestResult {
    let module = IrModule::new();
    let inner = ResolvedType::Optional(Box::new(primitive(PrimitiveType::I32)));
    match layout::plan_optional(&inner, &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "Optional<Optional<T>>" => Ok(()),
        other => {
            Err(format!("expected NotYetSupported(Optional<Optional<T>>), got {other:?}").into())
        }
    }
}

#[test]
fn optional_of_range_is_not_yet_supported() -> TestResult {
    let module = IrModule::new();
    let inner = ResolvedType::Range(Box::new(primitive(PrimitiveType::I32)));
    match layout::plan_optional(&inner, &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "Range<T>" => Ok(()),
        other => Err(format!("expected NotYetSupported(Range<T>), got {other:?}").into()),
    }
}
