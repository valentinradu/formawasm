//! Tests for `lower::lower_dict_access` over `Array<T>` collections.
//!
//! Phase 1c mc5 lights up `arr[i]` reads. The Phase 2 dictionary form
//! (`dict["key"]`) rides later. Each test builds a tiny module whose
//! entry point evaluates an indexed read, lowers it through the
//! production `module_lowering::lower_module` pipeline, validates the
//! emitted bytes, and instantiates under wasmtime to read back the
//! returned scalar — or, for the composed For-loop test, the resulting
//! comprehension array.

mod common;
use common::{seed_prelude, array_ty, range_ty};

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType,
};
use formalang::ir::{
    BindingId, IrBlockStatement, IrExpr, IrFunction, IrModule, IrSpan, ResolvedType,
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

fn array_literal(elements: Vec<IrExpr>, elem_ty: ResolvedType) -> IrExpr {
    IrExpr::Array {
        elements,
        ty: array_ty(elem_ty),
        span: IrSpan::default(),
    }
}

fn dict_access(dict: IrExpr, key: IrExpr, value_ty: ResolvedType) -> IrExpr {
    IrExpr::DictAccess {
        dict: Box::new(dict),
        key: Box::new(key),
        ty: value_ty,
        span: IrSpan::default(),
    }
}

fn let_ref(binding_id: BindingId, name: &str, ty: ResolvedType) -> IrExpr {
    IrExpr::LetRef {
        name: name.to_owned(),
        binding_id,
        ty,
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

fn read_memory<const N: usize>(
    store: &mut Store<()>,
    instance: &Instance,
    offset: i32,
) -> Result<[u8; N], TestError> {
    let memory = instance
        .exports(&mut *store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;
    let mut buf = [0u8; N];
    let off = usize::try_from(offset).map_err(|_| -> TestError { "offset negative".into() })?;
    memory.read(&*store, off, &mut buf)?;
    Ok(buf)
}

fn read_header(
    store: &mut Store<()>,
    instance: &Instance,
    header_ptr: i32,
) -> Result<(i32, i32, i32), TestError> {
    let buf: [u8; 12] = read_memory(store, instance, header_ptr)?;
    let ptr = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let len = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let cap = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    Ok((ptr, len, cap))
}

/// Build `[10, 20, 30][idx]` and assert the call returns `expected`.
fn run_i32_index(idx: i128, expected: i32) -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let arr = array_literal(
        vec![
            integer_literal(10, PrimitiveType::I32),
            integer_literal(20, PrimitiveType::I32),
            integer_literal(30, PrimitiveType::I32),
        ],
        primitive(PrimitiveType::I32),
    );
    module.functions.push(function(
        "pick",
        primitive(PrimitiveType::I32),
        dict_access(
            arr,
            integer_literal(idx, PrimitiveType::I32),
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "pick")?;
    let got = f.call(&mut store, ())?;
    if got != expected {
        return Err(format!("[10,20,30][{idx}]: got {got}, want {expected}").into());
    }
    Ok(())
}

#[test]
fn index_zero_returns_first_i32_element() -> TestResult {
    run_i32_index(0, 10)
}

#[test]
fn index_one_returns_second_i32_element() -> TestResult {
    run_i32_index(1, 20)
}

#[test]
fn index_two_returns_third_i32_element() -> TestResult {
    run_i32_index(2, 30)
}

#[test]
fn index_into_i64_array_uses_eight_byte_stride() -> TestResult {
    // [0x0BAD_F00D_DEAD_BEEF, 0x1234_5678_9ABC_DEF0][1]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let arr = array_literal(
        vec![
            integer_literal(0x0BAD_F00D_DEAD_BEEF, PrimitiveType::I64),
            integer_literal(0x1234_5678_9ABC_DEF0, PrimitiveType::I64),
        ],
        primitive(PrimitiveType::I64),
    );
    module.functions.push(function(
        "pick",
        primitive(PrimitiveType::I64),
        dict_access(
            arr,
            integer_literal(1, PrimitiveType::I32),
            primitive(PrimitiveType::I64),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i64>(&mut store, "pick")?;
    let got = f.call(&mut store, ())?;
    if got != 0x1234_5678_9ABC_DEF0_i64 {
        return Err(format!("got {got:#x}, want 0x1234_5678_9ABC_DEF0").into());
    }
    Ok(())
}

#[test]
fn for_over_range_indexes_outer_array_with_modulus() -> TestResult {
    // {
    //   let arr = [10, 20, 30];
    //   for p in 0..6 { arr[p % 3] }
    // } -> [10, 20, 30, 10, 20, 30]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let arr_id = BindingId(1);
    let p_id = BindingId(2);

    let arr_lit = array_literal(
        vec![
            integer_literal(10, PrimitiveType::I32),
            integer_literal(20, PrimitiveType::I32),
            integer_literal(30, PrimitiveType::I32),
        ],
        primitive(PrimitiveType::I32),
    );

    let arr_ref = let_ref(arr_id, "arr", array_ty(primitive(PrimitiveType::I32)));
    let p_ref = let_ref(p_id, "p", primitive(PrimitiveType::I32));
    let mod_three = IrExpr::BinaryOp {
        left: Box::new(p_ref),
        right: Box::new(integer_literal(3, PrimitiveType::I32)),
        op: BinaryOperator::Mod,
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let body = dict_access(arr_ref, mod_three, primitive(PrimitiveType::I32));

    let range = IrExpr::BinaryOp {
        left: Box::new(integer_literal(0, PrimitiveType::I32)),
        right: Box::new(integer_literal(6, PrimitiveType::I32)),
        op: BinaryOperator::Range,
        ty: range_ty(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };

    let for_loop = IrExpr::For {
        var: "p".to_owned(),
        var_ty: primitive(PrimitiveType::I32),
        var_binding_id: p_id,
        collection: Box::new(range),
        body: Box::new(body),
        ty: array_ty(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };

    let block = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id: arr_id,
            name: "arr".to_owned(),
            mutable: false,
            ty: Some(array_ty(primitive(PrimitiveType::I32))),
            value: arr_lit,
            span: IrSpan::default(),
        }],
        result: Box::new(for_loop),
        ty: array_ty(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };

    module.functions.push(function(
        "rotate",
        array_ty(primitive(PrimitiveType::I32)),
        block,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "rotate")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (6, 6) {
        return Err(format!("len/cap: got ({len}, {cap}), want (6, 6)").into());
    }
    let buf: [u8; 24] = read_memory(&mut store, &instance, buf_ptr)?;
    let mut vals = [0_i32; 6];
    for (slot, chunk) in vals.iter_mut().zip(buf.chunks_exact(4)) {
        let bytes: [u8; 4] = chunk
            .try_into()
            .map_err(|_| -> TestError { "chunk length not 4".into() })?;
        *slot = i32::from_le_bytes(bytes);
    }
    if vals != [10, 20, 30, 10, 20, 30] {
        return Err(format!("got {vals:?}, want [10, 20, 30, 10, 20, 30]").into());
    }
    Ok(())
}
