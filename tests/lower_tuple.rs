//! Tests for `lower::lower_tuple`.
//!
//! Tuples lower as anonymous structs synthesized from their
//! `ResolvedType::Tuple(...)`. Construction should produce a
//! pointer; `FieldAccess` on a tuple should read fields back at the
//! canonical-ABI offsets.

use formalang::ast::{Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType};
use formalang::ir::{FieldIdx, IrExpr, IrFunction, IrModule, ResolvedType};
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
    }
}

fn tuple_ty(fields: Vec<(&str, PrimitiveType)>) -> ResolvedType {
    ResolvedType::Tuple(
        fields
            .into_iter()
            .map(|(n, p)| (n.to_owned(), primitive(p)))
            .collect(),
    )
}

fn tuple_inst(ty: ResolvedType, fields: Vec<(&str, IrExpr)>) -> IrExpr {
    IrExpr::Tuple {
        fields: fields.into_iter().map(|(n, e)| (n.to_owned(), e)).collect(),
        ty,
    }
}

fn field_access(object: IrExpr, field: &str, idx: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::FieldAccess {
        object: Box::new(object),
        field: field.to_owned(),
        field_idx: FieldIdx(idx),
        ty: primitive(ty),
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
    }
}

#[test]
fn tuple_construction_returns_a_valid_pointer() -> TestResult {
    // fn make() -> tuple { (x: 7, y: 13) }
    let mut module = IrModule::new();
    let ty = tuple_ty(vec![("x", PrimitiveType::I32), ("y", PrimitiveType::I32)]);
    // Build the IrFunction by hand because the helper above wraps the
    // return_ty in `primitive`, but we need the tuple type.
    let body = tuple_inst(
        ty.clone(),
        vec![
            ("x", integer_literal(7, PrimitiveType::I32)),
            ("y", integer_literal(13, PrimitiveType::I32)),
        ],
    );
    module.functions.push(IrFunction {
        name: "make".to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(ty),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    });

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let make = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let ptr = make.call(&mut store, ())?;
    if ptr < 0 || ptr % 8 != 0 {
        return Err(format!("expected non-neg 8-aligned ptr, got {ptr}").into());
    }
    Ok(())
}

#[test]
fn tuple_field_access_reads_back_value() -> TestResult {
    // fn first() -> I32 { (x: 7, y: 13).x }
    let mut module = IrModule::new();
    let ty = tuple_ty(vec![("x", PrimitiveType::I32), ("y", PrimitiveType::I32)]);
    let inst = tuple_inst(
        ty,
        vec![
            ("x", integer_literal(7, PrimitiveType::I32)),
            ("y", integer_literal(13, PrimitiveType::I32)),
        ],
    );
    let body = field_access(inst, "x", 0, PrimitiveType::I32);
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
    if got != 7 {
        return Err(format!("got {got}, want 7").into());
    }
    Ok(())
}

#[test]
fn tuple_with_mixed_alignments_reads_back_i64() -> TestResult {
    // fn read_big() -> I64 { (a: 7i32, b: 0xDEADBEEFi64).b }
    let mut module = IrModule::new();
    let ty = tuple_ty(vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I64)]);
    let inst = tuple_inst(
        ty,
        vec![
            ("a", integer_literal(7, PrimitiveType::I32)),
            (
                "b",
                integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
            ),
        ],
    );
    let body = field_access(inst, "b", 1, PrimitiveType::I64);
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
