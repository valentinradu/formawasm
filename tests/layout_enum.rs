//! Tests for `layout::plan_enum`.

use formalang::ast::{ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{IrEnum, IrEnumVariant, IrField, IrModule, IrSpan, ResolvedType};
use formawasm::layout::{
    self, ENUM_TAG_ALIGN, ENUM_TAG_SIZE, EnumLayout, FieldLayout, LayoutError,
};

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
        span: IrSpan::default(),
    }
}

fn variant(name: &str, fields: Vec<(&str, PrimitiveType)>) -> IrEnumVariant {
    IrEnumVariant {
        name: name.to_owned(),
        fields: fields
            .into_iter()
            .map(|(n, p)| field(n, primitive(p)))
            .collect(),
        span: IrSpan::default(),
    }
}

fn make_enum(name: &str, variants: Vec<IrEnumVariant>) -> IrEnum {
    IrEnum {
        name: name.to_owned(),
        visibility: Visibility::Public,
        variants,
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn assert_enum_size_align(actual: &EnumLayout, size: u32, align: u32) -> TestResult {
    if actual.size != size {
        return Err(format!("size: got {}, want {size}", actual.size).into());
    }
    if actual.align != align {
        return Err(format!("align: got {}, want {align}", actual.align).into());
    }
    Ok(())
}

#[test]
fn unit_variants_only_keep_size_at_tag_size_padded_to_align() -> TestResult {
    // enum Color { Red, Green, Blue }
    let e = make_enum(
        "Color",
        vec![
            variant("Red", vec![]),
            variant("Green", vec![]),
            variant("Blue", vec![]),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_enum(&e, &module)?;

    assert_enum_size_align(&layout, 4, 4)?;
    if layout.tag_offset != 0 || layout.payload_offset != ENUM_TAG_SIZE {
        return Err(format!(
            "tag/payload offsets: got ({},{}), want (0,{ENUM_TAG_SIZE})",
            layout.tag_offset, layout.payload_offset,
        )
        .into());
    }
    let tags: Vec<u32> = layout.variants.iter().map(|v| v.tag).collect();
    if tags != vec![0, 1, 2] {
        return Err(format!("tags: got {tags:?}, want [0,1,2]").into());
    }
    Ok(())
}

#[test]
fn maybe_with_i32_payload_is_eight_bytes() -> TestResult {
    // enum Maybe { None, Some(I32) }
    let e = make_enum(
        "Maybe",
        vec![
            variant("None", vec![]),
            variant("Some", vec![("v", PrimitiveType::I32)]),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_enum(&e, &module)?;

    assert_enum_size_align(&layout, 8, 4)?;
    if layout.payload_offset != 4 {
        return Err(format!("payload_offset: got {}, want 4", layout.payload_offset).into());
    }
    let some = layout
        .variants
        .iter()
        .find(|v| v.name == "Some")
        .ok_or("no Some variant")?;
    let f = some.fields.first().ok_or("Some has no fields")?;
    if f.offset != 4 || f.size != 4 || f.align != 4 {
        return Err(format!("Some.v field: got {f:?}, want offset=4 size=4 align=4").into());
    }
    Ok(())
}

#[test]
fn variant_with_i64_pads_payload_offset_to_eight() -> TestResult {
    // enum E { A(I32), B(I64) }
    // max payload align = 8 → payload_offset = 8 (4 bytes pad after tag)
    // max payload size = 8 (B's i64). enum size = 8 + 8 = 16. align = 8.
    let e = make_enum(
        "E",
        vec![
            variant("A", vec![("a", PrimitiveType::I32)]),
            variant("B", vec![("b", PrimitiveType::I64)]),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_enum(&e, &module)?;

    assert_enum_size_align(&layout, 16, 8)?;
    if layout.payload_offset != 8 {
        return Err(format!("payload_offset: got {}, want 8", layout.payload_offset).into());
    }
    let a = layout
        .variants
        .iter()
        .find(|v| v.name == "A")
        .ok_or("no A")?;
    let b = layout
        .variants
        .iter()
        .find(|v| v.name == "B")
        .ok_or("no B")?;
    let want_a = FieldLayout {
        offset: 8,
        size: 4,
        align: 4,
    };
    let want_b = FieldLayout {
        offset: 8,
        size: 8,
        align: 8,
    };
    if a.fields.first() != Some(&want_a) {
        return Err(format!("A.a: got {:?}, want {want_a:?}", a.fields.first()).into());
    }
    if b.fields.first() != Some(&want_b) {
        return Err(format!("B.b: got {:?}, want {want_b:?}", b.fields.first()).into());
    }
    Ok(())
}

#[test]
fn variants_with_mixed_field_types_share_payload_layout() -> TestResult {
    // enum E { A(Boolean, I32), B(I64) }
    // A's payload (struct-like): bool@0..1, i32@4..8 → size 8, align 4
    // B's payload: i64@0..8 → size 8, align 8
    // max payload align = 8 → payload_offset = 8
    // max payload size = 8 (both equal)
    // enum size = 8 + 8 = 16, align = 8
    let e = make_enum(
        "E",
        vec![
            variant(
                "A",
                vec![
                    ("flag", PrimitiveType::Boolean),
                    ("count", PrimitiveType::I32),
                ],
            ),
            variant("B", vec![("v", PrimitiveType::I64)]),
        ],
    );
    let module = IrModule::new();
    let layout = layout::plan_enum(&e, &module)?;

    assert_enum_size_align(&layout, 16, 8)?;
    let a = layout
        .variants
        .iter()
        .find(|v| v.name == "A")
        .ok_or("no A")?;
    // A.flag at absolute 8; A.count at absolute 8 + 4 = 12.
    let want_flag = FieldLayout {
        offset: 8,
        size: 1,
        align: 1,
    };
    let want_count = FieldLayout {
        offset: 12,
        size: 4,
        align: 4,
    };
    if a.fields.first() != Some(&want_flag) {
        return Err(format!("A.flag: got {:?}, want {want_flag:?}", a.fields.first()).into());
    }
    if a.fields.get(1) != Some(&want_count) {
        return Err(format!("A.count: got {:?}, want {want_count:?}", a.fields.get(1)).into());
    }
    Ok(())
}

#[test]
fn empty_enum_is_uninhabited() -> TestResult {
    let e = make_enum("Empty", vec![]);
    let module = IrModule::new();
    match layout::plan_enum(&e, &module) {
        Err(LayoutError::UninhabitedEnum { name }) if name == "Empty" => Ok(()),
        other => Err(format!("expected UninhabitedEnum, got {other:?}").into()),
    }
}

#[test]
fn tag_align_constant_matches_layout() -> TestResult {
    let e = make_enum("Tag", vec![variant("X", vec![])]);
    let module = IrModule::new();
    let layout = layout::plan_enum(&e, &module)?;

    if layout.align < ENUM_TAG_ALIGN {
        return Err(format!(
            "align ({}) less than tag align ({ENUM_TAG_ALIGN})",
            layout.align
        )
        .into());
    }
    let v = layout.variants.first().ok_or("no variants")?;
    if v.tag != 0 {
        return Err(format!("first tag: got {}, want 0", v.tag).into());
    }
    Ok(())
}
