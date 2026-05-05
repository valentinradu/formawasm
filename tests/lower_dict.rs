//! End-to-end coverage for `Dictionary<K, V>` literal construction
//! and lookup.
//!
//! Phase 2 v1 represents `Dictionary<K, V>` as a `{ ptr, len, cap }`
//! header pointing at a buffer of pointers, each pointing to a
//! `(k: K, v: V)` pair tuple. Lookup walks the buffer linearly
//! comparing keys (string keys via `__str_eq`).

mod common;
use common::{seed_prelude, dict_ty};

use formalang::ast::{Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType};
use formalang::ir::{IrExpr, IrFunction, IrModule, IrSpan, ResolvedType};
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


fn string_literal(text: &str) -> IrExpr {
    IrExpr::Literal {
        value: Literal::String(text.to_owned()),
        ty: primitive(PrimitiveType::String),
        span: IrSpan::default(),
    }
}

fn integer_literal(value: i128) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(value),
            NumericSuffix::I32,
        )),
        ty: primitive(PrimitiveType::I32),
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

fn instantiate(bytes: &[u8]) -> Result<(Store<()>, Instance), TestError> {
    let engine = Engine::default();
    let m = Module::from_binary(&engine, bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    Ok((store, instance))
}

#[test]
fn dict_literal_lookup_returns_matching_value() -> TestResult {
    // `fn lookup_a() -> I32 { ["a": 1, "b": 2, "c": 3]["b"] }`
    let dict_type = dict_ty(
        primitive(PrimitiveType::String),
        primitive(PrimitiveType::I32),
    );
    let dict_literal = IrExpr::DictLiteral {
        entries: vec![
            (string_literal("a"), integer_literal(1)),
            (string_literal("b"), integer_literal(2)),
            (string_literal("c"), integer_literal(3)),
        ],
        ty: dict_type,
        span: IrSpan::default(),
    };
    let access = IrExpr::DictAccess {
        dict: Box::new(dict_literal),
        key: Box::new(string_literal("b")),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module
        .functions
        .push(function("lookup-b", primitive(PrimitiveType::I32), access));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "lookup-b")?;
    let r = f.call(&mut store, ())?;
    if r != 2 {
        return Err(format!("dict[\"b\"]: got {r}, want 2").into());
    }
    Ok(())
}

#[test]
fn dict_lookup_first_and_last_entries() -> TestResult {
    // Exercise both ends of the linear scan to catch off-by-one
    // errors on either bound.
    let dict_type = dict_ty(
        primitive(PrimitiveType::String),
        primitive(PrimitiveType::I32),
    );
    let make_lookup = |key: &str| {
        let dict_literal = IrExpr::DictLiteral {
            entries: vec![
                (string_literal("first"), integer_literal(10)),
                (string_literal("middle"), integer_literal(20)),
                (string_literal("last"), integer_literal(30)),
            ],
            ty: dict_type.clone(),
            span: IrSpan::default(),
        };
        IrExpr::DictAccess {
            dict: Box::new(dict_literal),
            key: Box::new(string_literal(key)),
            ty: primitive(PrimitiveType::I32),
            span: IrSpan::default(),
        }
    };
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "first",
        primitive(PrimitiveType::I32),
        make_lookup("first"),
    ));
    module.functions.push(function(
        "last",
        primitive(PrimitiveType::I32),
        make_lookup("last"),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let first = instance.get_typed_func::<(), i32>(&mut store, "first")?;
    let last = instance.get_typed_func::<(), i32>(&mut store, "last")?;
    let r1 = first.call(&mut store, ())?;
    let r2 = last.call(&mut store, ())?;
    if r1 != 10 {
        return Err(format!("dict[\"first\"]: got {r1}, want 10").into());
    }
    if r2 != 30 {
        return Err(format!("dict[\"last\"]: got {r2}, want 30").into());
    }
    Ok(())
}

#[test]
fn dict_missing_key_traps() -> TestResult {
    let dict_type = dict_ty(
        primitive(PrimitiveType::String),
        primitive(PrimitiveType::I32),
    );
    let dict_literal = IrExpr::DictLiteral {
        entries: vec![(string_literal("only"), integer_literal(7))],
        ty: dict_type,
        span: IrSpan::default(),
    };
    let access = IrExpr::DictAccess {
        dict: Box::new(dict_literal),
        key: Box::new(string_literal("missing")),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module
        .functions
        .push(function("lookup", primitive(PrimitiveType::I32), access));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "lookup")?;
    let result = f.call(&mut store, ());
    if result.is_ok() {
        return Err("expected wasm trap on missing key, call returned Ok".into());
    }
    Ok(())
}
