//! Tests for `lower::lower_enum_inst`.
//!
//! Each test builds a small module with one enum and one function
//! that constructs an instance of one of its variants, then reads
//! back either the discriminant tag or one of the variant's fields
//! through linear-memory inspection.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    EnumId, FieldIdx, IrEnum, IrEnumVariant, IrExpr, IrField, IrFunction, IrModule, IrSpan,
    ResolvedType, VariantIdx,
};
use formawasm::module_lowering;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::{Engine, Instance, Module, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
    let suffix = if ty == PrimitiveType::I64 {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: primitive(ty),
        span: IrSpan::default(),
    }
}

fn field(name: &str, p: PrimitiveType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty: primitive(p),
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
        fields: fields.into_iter().map(|(n, p)| field(n, p)).collect(),
        span: IrSpan::default(),
    }
}

fn enum_def(name: &str, variants: Vec<IrEnumVariant>) -> IrEnum {
    IrEnum {
        name: name.to_owned(),
        visibility: Visibility::Public,
        variants,
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn enum_inst(
    enum_id: EnumId,
    variant_idx: u32,
    variant: &str,
    fields: Vec<(&str, IrExpr)>,
) -> IrExpr {
    IrExpr::EnumInst {
        enum_id: Some(enum_id),
        variant: variant.to_owned(),
        variant_idx: VariantIdx(variant_idx),
        fields: fields
            .into_iter()
            .enumerate()
            .map(|(i, (name, e))| (name.to_owned(), FieldIdx(u32::try_from(i).unwrap_or(0)), e))
            .collect(),
        ty: ResolvedType::Enum(enum_id),
        span: IrSpan::default(),
    }
}

fn function(name: &str, return_ty: ResolvedType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(return_ty),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

#[test]
fn unit_variant_writes_correct_tag() -> TestResult {
    // enum Color { Red, Green, Blue }
    // fn make_green() -> Color { Color::Green }
    let mut module = IrModule::new();
    module.enums.push(enum_def(
        "Color",
        vec![
            variant("Red", vec![]),
            variant("Green", vec![]),
            variant("Blue", vec![]),
        ],
    ));
    let body = enum_inst(EnumId(0), 1, "Green", vec![]);
    module
        .functions
        .push(function("make_green", ResolvedType::Enum(EnumId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make-green")?;
    let ptr = f.call(&mut store, ())?;
    let memory = instance
        .exports(&mut store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;

    let mut buf = [0u8; 4];
    let offset = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    memory.read(&store, offset, &mut buf)?;
    let tag = i32::from_le_bytes(buf);
    if tag != 1 {
        return Err(format!("expected tag 1 (Green), got {tag}").into());
    }
    Ok(())
}

#[test]
fn variant_with_i32_payload_writes_tag_and_field() -> TestResult {
    // enum Maybe { None, Some(I32) }
    // fn make_some() -> Maybe { Maybe::Some(42) }
    let mut module = IrModule::new();
    module.enums.push(enum_def(
        "Maybe",
        vec![
            variant("None", vec![]),
            variant("Some", vec![("v", PrimitiveType::I32)]),
        ],
    ));
    let body = enum_inst(
        EnumId(0),
        1,
        "Some",
        vec![("v", integer_literal(42, PrimitiveType::I32))],
    );
    module
        .functions
        .push(function("make_some", ResolvedType::Enum(EnumId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make-some")?;
    let ptr = f.call(&mut store, ())?;
    let memory = instance
        .exports(&mut store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;

    let mut buf = [0u8; 8];
    let offset = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    memory.read(&store, offset, &mut buf)?;
    let tag = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let v = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if tag != 1 {
        return Err(format!("tag: got {tag}, want 1 (Some)").into());
    }
    if v != 42 {
        return Err(format!("payload: got {v}, want 42").into());
    }
    Ok(())
}

#[test]
fn variant_with_i64_payload_uses_eight_byte_offset() -> TestResult {
    // enum E { A(I32), B(I64) }
    // fn make_b() -> E { E::B(0xDEADBEEF) }
    let mut module = IrModule::new();
    module.enums.push(enum_def(
        "E",
        vec![
            variant("A", vec![("a", PrimitiveType::I32)]),
            variant("B", vec![("b", PrimitiveType::I64)]),
        ],
    ));
    let body = enum_inst(
        EnumId(0),
        1,
        "B",
        vec![(
            "b",
            integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
        )],
    );
    module
        .functions
        .push(function("make_b", ResolvedType::Enum(EnumId(0)), body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make-b")?;
    let ptr = f.call(&mut store, ())?;
    let memory = instance
        .exports(&mut store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;

    let mut buf = [0u8; 16];
    let offset = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    memory.read(&store, offset, &mut buf)?;
    let tag = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    // Bytes 4..8 are alignment padding; payload at 8..16.
    let payload = i64::from_le_bytes([
        buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
    ]);
    if tag != 1 {
        return Err(format!("tag: got {tag}, want 1 (B)").into());
    }
    if payload != 0x0BAD_F00D_DEAD_BEEF {
        return Err(format!("payload: got {payload:#x}, want 0x0BADF00DDEADBEEF").into());
    }
    Ok(())
}
