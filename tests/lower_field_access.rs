//! Tests for `lower::lower_field_access`.
//!
//! Each test builds a small module with a struct and a function that
//! constructs an instance and reads back one of its fields. The
//! function returns a primitive, so we can inspect the result
//! directly via wasmtime without poking memory by hand.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    FieldIdx, IrExpr, IrField, IrFunction, IrModule, IrSpan, IrStruct, ResolvedType, StructId,
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

fn boolean_literal(b: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(b),
        ty: primitive(PrimitiveType::Boolean),
        span: IrSpan::default(),
    }
}

fn struct_def(name: &str, fields: Vec<(&str, PrimitiveType)>) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: fields
            .into_iter()
            .map(|(n, ty)| IrField {
                name: n.to_owned(),
                ty: primitive(ty),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            })
            .collect(),
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn struct_inst(struct_id: StructId, fields: Vec<(&str, IrExpr)>) -> IrExpr {
    IrExpr::StructInst {
        struct_id: Some(struct_id),
        type_args: Vec::new(),
        fields: fields
            .into_iter()
            .enumerate()
            .map(|(i, (name, e))| (name.to_owned(), FieldIdx(u32::try_from(i).unwrap_or(0)), e))
            .collect(),
        ty: ResolvedType::Struct(struct_id),
        span: IrSpan::default(),
    }
}

fn field_access(object: IrExpr, field: &str, idx: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::FieldAccess {
        object: Box::new(object),
        field: field.to_owned(),
        field_idx: FieldIdx(idx),
        ty: primitive(ty),
        span: IrSpan::default(),
    }
}

fn function(name: &str, return_ty: PrimitiveType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(return_ty)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

#[test]
fn read_back_first_i32_field() -> TestResult {
    // struct Pair { a: I32, b: I32 }
    // fn first() -> I32 { Pair { a: 11, b: 22 }.a }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Pair",
        vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I32)],
    ));
    let inst = struct_inst(
        StructId(0),
        vec![
            ("a", integer_literal(11, PrimitiveType::I32)),
            ("b", integer_literal(22, PrimitiveType::I32)),
        ],
    );
    let body = field_access(inst, "a", 0, PrimitiveType::I32);
    module
        .functions
        .push(function("first", PrimitiveType::I32, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "first")?;
    let got = f.call(&mut store, ())?;
    if got != 11 {
        return Err(format!("got {got}, want 11").into());
    }
    Ok(())
}

#[test]
fn read_back_second_i32_field() -> TestResult {
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Pair",
        vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I32)],
    ));
    let inst = struct_inst(
        StructId(0),
        vec![
            ("a", integer_literal(11, PrimitiveType::I32)),
            ("b", integer_literal(22, PrimitiveType::I32)),
        ],
    );
    let body = field_access(inst, "b", 1, PrimitiveType::I32);
    module
        .functions
        .push(function("second", PrimitiveType::I32, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "second")?;
    let got = f.call(&mut store, ())?;
    if got != 22 {
        return Err(format!("got {got}, want 22").into());
    }
    Ok(())
}

#[test]
fn read_back_i64_field_after_padding() -> TestResult {
    // struct M { flag: Boolean, count: I32, big: I64 }
    // fn read_big() -> I64 { M { flag: true, count: 0, big: 0xDEADBEEF }.big }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "M",
        vec![
            ("flag", PrimitiveType::Boolean),
            ("count", PrimitiveType::I32),
            ("big", PrimitiveType::I64),
        ],
    ));
    let inst = struct_inst(
        StructId(0),
        vec![
            ("flag", boolean_literal(true)),
            ("count", integer_literal(0, PrimitiveType::I32)),
            (
                "big",
                integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
            ),
        ],
    );
    let body = field_access(inst, "big", 2, PrimitiveType::I64);
    module
        .functions
        .push(function("read_big", PrimitiveType::I64, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i64>(&mut store, "read-big")?;
    let got = f.call(&mut store, ())?;
    if got != 0x0BAD_F00D_DEAD_BEEF {
        return Err(format!("got {got:#x}, want 0x0BADF00DDEADBEEF").into());
    }
    Ok(())
}

#[test]
fn read_back_boolean_field() -> TestResult {
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Flagged",
        vec![("flag", PrimitiveType::Boolean), ("v", PrimitiveType::I32)],
    ));
    let inst = struct_inst(
        StructId(0),
        vec![
            ("flag", boolean_literal(true)),
            ("v", integer_literal(99, PrimitiveType::I32)),
        ],
    );
    let body = field_access(inst, "flag", 0, PrimitiveType::Boolean);
    // Boolean is wasm-i32, return as i32.
    module
        .functions
        .push(function("read_flag", PrimitiveType::Boolean, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "read-flag")?;
    let got = f.call(&mut store, ())?;
    if got != 1 {
        return Err(format!("got {got}, want 1").into());
    }
    Ok(())
}
