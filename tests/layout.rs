//! Tests for `layout::plan_struct`.
//!
//! Cover the empty struct, every primitive size class, a handful of
//! mixed orderings that exercise field-alignment padding, the
//! optional-field rejection, and a non-primitive-typed field
//! rejection.

use formalang::ast::{ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{IrField, IrModule, IrStruct, ResolvedType, StructId};
use formawasm::layout::{self, FieldLayout, LayoutError, StructLayout};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn field(name: &str, ty: ResolvedType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty,
        mutable: false,
        optional: false,
        default: None,
        doc: None,
        convention: ParamConvention::Let,
    }
}

fn make_struct(name: &str, fields: Vec<IrField>) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields,
        generic_params: Vec::new(),
        doc: None,
    }
}

fn assert_layout(
    actual: &StructLayout,
    size: u32,
    align: u32,
    fields: &[FieldLayout],
) -> TestResult {
    if actual.size != size {
        return Err(format!("size: got {}, want {size}", actual.size).into());
    }
    if actual.align != align {
        return Err(format!("align: got {}, want {align}", actual.align).into());
    }
    if actual.fields.len() != fields.len() {
        return Err(format!(
            "field count: got {}, want {}",
            actual.fields.len(),
            fields.len()
        )
        .into());
    }
    for (i, (got, want)) in actual.fields.iter().zip(fields.iter()).enumerate() {
        if got != want {
            return Err(format!("field[{i}]: got {got:?}, want {want:?}").into());
        }
    }
    Ok(())
}

#[test]
fn empty_struct_has_zero_size_and_align_one() -> TestResult {
    let s = make_struct("Empty", Vec::new());
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    assert_layout(&layout, 0, 1, &[])
}

#[test]
fn single_i32_field() -> TestResult {
    let s = make_struct("One", vec![field("x", primitive(PrimitiveType::I32))]);
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    assert_layout(
        &layout,
        4,
        4,
        &[FieldLayout {
            offset: 0,
            size: 4,
            align: 4,
        }],
    )
}

#[test]
fn two_i32_fields_pack_tightly() -> TestResult {
    let s = make_struct(
        "Pair",
        vec![
            field("a", primitive(PrimitiveType::I32)),
            field("b", primitive(PrimitiveType::I32)),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    assert_layout(
        &layout,
        8,
        4,
        &[
            FieldLayout {
                offset: 0,
                size: 4,
                align: 4,
            },
            FieldLayout {
                offset: 4,
                size: 4,
                align: 4,
            },
        ],
    )
}

#[test]
fn i32_then_i64_pads_to_eight_byte_alignment() -> TestResult {
    let s = make_struct(
        "Mixed",
        vec![
            field("a", primitive(PrimitiveType::I32)),
            field("b", primitive(PrimitiveType::I64)),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    // a at 0..4, 4..8 padding, b at 8..16, total 16, align 8.
    assert_layout(
        &layout,
        16,
        8,
        &[
            FieldLayout {
                offset: 0,
                size: 4,
                align: 4,
            },
            FieldLayout {
                offset: 8,
                size: 8,
                align: 8,
            },
        ],
    )
}

#[test]
fn i64_then_i32_pads_struct_size_to_alignment() -> TestResult {
    let s = make_struct(
        "TrailingPad",
        vec![
            field("a", primitive(PrimitiveType::I64)),
            field("b", primitive(PrimitiveType::I32)),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    // a at 0..8, b at 8..12, trailing pad to 16 because struct align is 8.
    assert_layout(
        &layout,
        16,
        8,
        &[
            FieldLayout {
                offset: 0,
                size: 8,
                align: 8,
            },
            FieldLayout {
                offset: 8,
                size: 4,
                align: 4,
            },
        ],
    )
}

#[test]
fn bool_then_i32_pads_to_four_byte_alignment() -> TestResult {
    let s = make_struct(
        "BoolThenI32",
        vec![
            field("flag", primitive(PrimitiveType::Boolean)),
            field("count", primitive(PrimitiveType::I32)),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    // flag at 0..1, 1..4 padding, count at 4..8, total 8, align 4.
    assert_layout(
        &layout,
        8,
        4,
        &[
            FieldLayout {
                offset: 0,
                size: 1,
                align: 1,
            },
            FieldLayout {
                offset: 4,
                size: 4,
                align: 4,
            },
        ],
    )
}

#[test]
fn three_byte_run_packs_before_alignment() -> TestResult {
    let s = make_struct(
        "ThreeBools",
        vec![
            field("a", primitive(PrimitiveType::Boolean)),
            field("b", primitive(PrimitiveType::Boolean)),
            field("c", primitive(PrimitiveType::Boolean)),
            field("count", primitive(PrimitiveType::I32)),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    // a..c at 0..3, 3..4 padding, count at 4..8, total 8, align 4.
    assert_layout(
        &layout,
        8,
        4,
        &[
            FieldLayout {
                offset: 0,
                size: 1,
                align: 1,
            },
            FieldLayout {
                offset: 1,
                size: 1,
                align: 1,
            },
            FieldLayout {
                offset: 2,
                size: 1,
                align: 1,
            },
            FieldLayout {
                offset: 4,
                size: 4,
                align: 4,
            },
        ],
    )
}

#[test]
fn float_fields_share_integer_alignments() -> TestResult {
    let s = make_struct(
        "Floats",
        vec![
            field("x", primitive(PrimitiveType::F32)),
            field("y", primitive(PrimitiveType::F64)),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    assert_layout(
        &layout,
        16,
        8,
        &[
            FieldLayout {
                offset: 0,
                size: 4,
                align: 4,
            },
            FieldLayout {
                offset: 8,
                size: 8,
                align: 8,
            },
        ],
    )
}

#[test]
fn optional_field_lays_out_as_pointer() -> TestResult {
    // Phase 2 mc7: `Optional<T>` fields collapse to a 4-byte pointer
    // payload (the tagged optional cell lives elsewhere in linear
    // memory). The redundant `optional: true` AST flag is preserved
    // alongside the resolved `Optional(T)` type but no longer drives
    // a layout rejection.
    let mut f = field(
        "maybe",
        ResolvedType::Optional(Box::new(primitive(PrimitiveType::I32))),
    );
    f.optional = true;
    let s = make_struct("Opt", vec![f]);
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    assert_layout(
        &layout,
        4,
        4,
        &[FieldLayout {
            offset: 0,
            size: 4,
            align: 4,
        }],
    )
}

#[test]
fn nested_struct_field_is_not_yet_supported() -> TestResult {
    let s = make_struct(
        "Outer",
        vec![field("inner", ResolvedType::Struct(StructId(0)))],
    );
    let module = IrModule::new();
    match layout::plan_struct(&s, &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "Struct" => Ok(()),
        other => Err(format!("expected NotYetSupported(Struct), got {other:?}").into()),
    }
}

#[test]
fn never_field_is_not_yet_supported() -> TestResult {
    let s = make_struct(
        "WithNever",
        vec![field("nope", primitive(PrimitiveType::Never))],
    );
    let module = IrModule::new();
    match layout::plan_struct(&s, &module) {
        Err(LayoutError::NotYetSupported { kind }) if kind == "Never" => Ok(()),
        other => Err(format!("expected NotYetSupported(Never), got {other:?}").into()),
    }
}

#[test]
fn string_field_lays_out_as_pointer() -> TestResult {
    // Phase 2 mc13: String / Path / Regex fields collapse to a
    // 4-byte pointer payload — the `{ ptr, len }` header lives
    // elsewhere in linear memory.
    let s = make_struct(
        "Stringy",
        vec![field("s", primitive(PrimitiveType::String))],
    );
    let module = IrModule::new();
    let layout = layout::plan_struct(&s, &module)?;
    assert_layout(
        &layout,
        4,
        4,
        &[FieldLayout {
            offset: 0,
            size: 4,
            align: 4,
        }],
    )
}
