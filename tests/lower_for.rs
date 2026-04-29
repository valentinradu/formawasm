//! Tests for `lower::lower_for`.
//!
//! Build a tiny module whose entry point evaluates a `for var in
//! lo..hi { body }` expression, lower it through the production
//! `module_lowering::lower_module` pipeline, validate, instantiate
//! under wasmtime, and read back the comprehension's output array
//! (`{ ptr, len, cap }` header + element buffer) through the exported
//! memory.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, PrimitiveType,
};
use formalang::ir::{BindingId, IrExpr, IrFunction, IrModule, ResolvedType};
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

fn array_ty(elem: ResolvedType) -> ResolvedType {
    ResolvedType::Array(Box::new(elem))
}

fn range_ty(elem: ResolvedType) -> ResolvedType {
    ResolvedType::Range(Box::new(elem))
}

const fn integer_literal(value: i128) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(value),
            NumericSuffix::I32,
        )),
        ty: primitive(PrimitiveType::I32),
    }
}

fn range_expr(start: IrExpr, end: IrExpr) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(start),
        right: Box::new(end),
        op: BinaryOperator::Range,
        ty: range_ty(primitive(PrimitiveType::I32)),
    }
}

fn let_ref(binding_id: BindingId, name: &str) -> IrExpr {
    IrExpr::LetRef {
        name: name.to_owned(),
        binding_id,
        ty: primitive(PrimitiveType::I32),
    }
}

fn for_expr(
    var: &str,
    var_binding_id: BindingId,
    collection: IrExpr,
    body: IrExpr,
    body_ty: ResolvedType,
) -> IrExpr {
    IrExpr::For {
        var: var.to_owned(),
        var_ty: primitive(PrimitiveType::I32),
        var_binding_id,
        collection: Box::new(collection),
        body: Box::new(body),
        ty: array_ty(body_ty),
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

#[test]
fn for_over_one_to_five_collects_loop_variable() -> TestResult {
    // for p in 1..5 { p } -> [1, 2, 3, 4]
    let mut module = IrModule::new();
    let var_id = BindingId(7);
    module.functions.push(function(
        "make",
        array_ty(primitive(PrimitiveType::I32)),
        for_expr(
            "p",
            var_id,
            range_expr(integer_literal(1), integer_literal(5)),
            let_ref(var_id, "p"),
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (4, 4) {
        return Err(format!("len/cap: got ({len}, {cap}), want (4, 4)").into());
    }
    let buf: [u8; 16] = read_memory(&mut store, &instance, buf_ptr)?;
    let v0 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let v1 = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let v2 = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let v3 = i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    if (v0, v1, v2, v3) != (1, 2, 3, 4) {
        return Err(format!("got ({v0}, {v1}, {v2}, {v3}), want (1, 2, 3, 4)").into());
    }
    Ok(())
}

#[test]
fn empty_range_for_loop_returns_empty_array() -> TestResult {
    // for p in 0..0 { p } -> []
    let mut module = IrModule::new();
    let var_id = BindingId(7);
    module.functions.push(function(
        "make",
        array_ty(primitive(PrimitiveType::I32)),
        for_expr(
            "p",
            var_id,
            range_expr(integer_literal(0), integer_literal(0)),
            let_ref(var_id, "p"),
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make")?;
    let header_ptr = f.call(&mut store, ())?;

    let (_buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (0, 0) {
        return Err(format!("len/cap: got ({len}, {cap}), want (0, 0)").into());
    }
    Ok(())
}

#[test]
fn for_with_body_arithmetic_collects_squared_values() -> TestResult {
    // for p in 2..6 { p * p } -> [4, 9, 16, 25]
    let mut module = IrModule::new();
    let var_id = BindingId(7);
    let body = IrExpr::BinaryOp {
        left: Box::new(let_ref(var_id, "p")),
        right: Box::new(let_ref(var_id, "p")),
        op: BinaryOperator::Mul,
        ty: primitive(PrimitiveType::I32),
    };
    module.functions.push(function(
        "squares",
        array_ty(primitive(PrimitiveType::I32)),
        for_expr(
            "p",
            var_id,
            range_expr(integer_literal(2), integer_literal(6)),
            body,
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "squares")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (4, 4) {
        return Err(format!("len/cap: got ({len}, {cap}), want (4, 4)").into());
    }
    let buf: [u8; 16] = read_memory(&mut store, &instance, buf_ptr)?;
    let v0 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let v1 = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let v2 = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let v3 = i32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]);
    if (v0, v1, v2, v3) != (4, 9, 16, 25) {
        return Err(format!("got ({v0}, {v1}, {v2}, {v3}), want (4, 9, 16, 25)").into());
    }
    Ok(())
}
