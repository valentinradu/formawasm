//! Tests for `lower::lower_for`.
//!
//! Build a tiny module whose entry point evaluates a `for var in
//! lo..hi { body }` expression, lower it through the production
//! `module_lowering::lower_module` pipeline, validate, instantiate
//! under wasmtime, and read back the comprehension's output array
//! (`{ ptr, len, cap }` header + element buffer) through the exported
//! memory.

mod common;
use common::{array_ty, range_ty, seed_prelude};

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

fn typed_integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
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

fn typed_let_ref(binding_id: BindingId, name: &str, ty: ResolvedType) -> IrExpr {
    IrExpr::LetRef {
        name: name.to_owned(),
        binding_id,
        ty,
        span: IrSpan::default(),
    }
}

fn for_over_array(
    var: &str,
    var_binding_id: BindingId,
    var_ty: ResolvedType,
    collection: IrExpr,
    body: IrExpr,
    body_ty: ResolvedType,
) -> IrExpr {
    IrExpr::For {
        var: var.to_owned(),
        var_ty,
        var_binding_id,
        collection: Box::new(collection),
        body: Box::new(body),
        ty: array_ty(body_ty),
        span: IrSpan::default(),
    }
}

fn range_expr(start: IrExpr, end: IrExpr) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(start),
        right: Box::new(end),
        op: BinaryOperator::Range,
        ty: range_ty(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    }
}

fn let_ref(binding_id: BindingId, name: &str) -> IrExpr {
    IrExpr::LetRef {
        name: name.to_owned(),
        binding_id,
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
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

#[test]
fn for_over_one_to_five_collects_loop_variable() -> TestResult {
    // for p in 1..5 { p } -> [1, 2, 3, 4]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
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
    seed_prelude(&mut module);
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
fn for_over_i64_range_uses_typed_loop_arithmetic() -> TestResult {
    // for p in 100I64..103I64 { p } -> [100, 101, 102]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(11);
    let i64_ty = primitive(PrimitiveType::I64);

    let lo = IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(100),
            NumericSuffix::I64,
        )),
        ty: i64_ty.clone(),
        span: IrSpan::default(),
    };
    let hi = IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(103),
            NumericSuffix::I64,
        )),
        ty: i64_ty.clone(),
        span: IrSpan::default(),
    };
    let range = IrExpr::BinaryOp {
        left: Box::new(lo),
        right: Box::new(hi),
        op: BinaryOperator::Range,
        ty: range_ty(i64_ty.clone()),
        span: IrSpan::default(),
    };
    let body = typed_let_ref(var_id, "p", i64_ty.clone());
    let for_loop = IrExpr::For {
        var: "p".to_owned(),
        var_ty: i64_ty.clone(),
        var_binding_id: var_id,
        collection: Box::new(range),
        body: Box::new(body),
        ty: array_ty(i64_ty.clone()),
        span: IrSpan::default(),
    };
    module
        .functions
        .push(function("longs", array_ty(i64_ty), for_loop));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "longs")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }
    let buf: [u8; 24] = read_memory(&mut store, &instance, buf_ptr)?;
    let mut vals = [0_i64; 3];
    for (slot, chunk) in vals.iter_mut().zip(buf.chunks_exact(8)) {
        let bytes: [u8; 8] = chunk
            .try_into()
            .map_err(|_| -> TestError { "chunk length not 8".into() })?;
        *slot = i64::from_le_bytes(bytes);
    }
    if vals != [100, 101, 102] {
        return Err(format!("got {vals:?}, want [100, 101, 102]").into());
    }
    Ok(())
}

#[test]
fn for_with_body_arithmetic_collects_squared_values() -> TestResult {
    // for p in 2..6 { p * p } -> [4, 9, 16, 25]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(7);
    let body = IrExpr::BinaryOp {
        left: Box::new(let_ref(var_id, "p")),
        right: Box::new(let_ref(var_id, "p")),
        op: BinaryOperator::Mul,
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
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

#[test]
fn for_over_i32_array_passes_each_element_into_body() -> TestResult {
    // for x in [10, 20, 30] { x + 1 } -> [11, 21, 31]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(11);
    let arr = array_literal(
        vec![
            integer_literal(10),
            integer_literal(20),
            integer_literal(30),
        ],
        primitive(PrimitiveType::I32),
    );
    let body = IrExpr::BinaryOp {
        left: Box::new(let_ref(var_id, "x")),
        right: Box::new(integer_literal(1)),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    module.functions.push(function(
        "bumped",
        array_ty(primitive(PrimitiveType::I32)),
        for_over_array(
            "x",
            var_id,
            primitive(PrimitiveType::I32),
            arr,
            body,
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "bumped")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }
    let buf: [u8; 12] = read_memory(&mut store, &instance, buf_ptr)?;
    let v0 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let v1 = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let v2 = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if (v0, v1, v2) != (11, 21, 31) {
        return Err(format!("got ({v0}, {v1}, {v2}), want (11, 21, 31)").into());
    }
    Ok(())
}

#[test]
fn for_over_empty_array_returns_empty_array() -> TestResult {
    // for x in [] { x } -> []
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(11);
    let arr = array_literal(Vec::new(), primitive(PrimitiveType::I32));
    module.functions.push(function(
        "nothing",
        array_ty(primitive(PrimitiveType::I32)),
        for_over_array(
            "x",
            var_id,
            primitive(PrimitiveType::I32),
            arr,
            let_ref(var_id, "x"),
            primitive(PrimitiveType::I32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "nothing")?;
    let header_ptr = f.call(&mut store, ())?;

    let (_buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (0, 0) {
        return Err(format!("len/cap: got ({len}, {cap}), want (0, 0)").into());
    }
    Ok(())
}

#[test]
fn for_over_i64_array_uses_eight_byte_stride_for_element_load() -> TestResult {
    // for x in [10I64, 20I64, 30I64] { x * x } -> [100, 400, 900]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(11);
    let arr = array_literal(
        vec![
            typed_integer_literal(10, PrimitiveType::I64),
            typed_integer_literal(20, PrimitiveType::I64),
            typed_integer_literal(30, PrimitiveType::I64),
        ],
        primitive(PrimitiveType::I64),
    );
    let body = IrExpr::BinaryOp {
        left: Box::new(typed_let_ref(var_id, "x", primitive(PrimitiveType::I64))),
        right: Box::new(typed_let_ref(var_id, "x", primitive(PrimitiveType::I64))),
        op: BinaryOperator::Mul,
        ty: primitive(PrimitiveType::I64),
        span: IrSpan::default(),
    };
    module.functions.push(function(
        "squared",
        array_ty(primitive(PrimitiveType::I64)),
        for_over_array(
            "x",
            var_id,
            primitive(PrimitiveType::I64),
            arr,
            body,
            primitive(PrimitiveType::I64),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "squared")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }
    let buf: [u8; 24] = read_memory(&mut store, &instance, buf_ptr)?;
    let mut vals = [0_i64; 3];
    for (slot, chunk) in vals.iter_mut().zip(buf.chunks_exact(8)) {
        let bytes: [u8; 8] = chunk
            .try_into()
            .map_err(|_| -> TestError { "chunk length not 8".into() })?;
        *slot = i64::from_le_bytes(bytes);
    }
    if vals != [100, 400, 900] {
        return Err(format!("got {vals:?}, want [100, 400, 900]").into());
    }
    Ok(())
}

#[test]
fn for_over_array_inside_block_uses_outer_let_binding() -> TestResult {
    // {
    //   let xs = [4, 5, 6];
    //   for x in xs { x * 10 }
    // } -> [40, 50, 60]
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let xs_id = BindingId(1);
    let var_id = BindingId(2);

    let arr_lit = array_literal(
        vec![integer_literal(4), integer_literal(5), integer_literal(6)],
        primitive(PrimitiveType::I32),
    );
    let body = IrExpr::BinaryOp {
        left: Box::new(let_ref(var_id, "x")),
        right: Box::new(integer_literal(10)),
        op: BinaryOperator::Mul,
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let for_loop = for_over_array(
        "x",
        var_id,
        primitive(PrimitiveType::I32),
        typed_let_ref(xs_id, "xs", array_ty(primitive(PrimitiveType::I32))),
        body,
        primitive(PrimitiveType::I32),
    );
    let block = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            binding_id: xs_id,
            name: "xs".to_owned(),
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
        "scaled",
        array_ty(primitive(PrimitiveType::I32)),
        block,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "scaled")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }
    let buf: [u8; 12] = read_memory(&mut store, &instance, buf_ptr)?;
    let v0 = i32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let v1 = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let v2 = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    if (v0, v1, v2) != (40, 50, 60) {
        return Err(format!("got ({v0}, {v1}, {v2}), want (40, 50, 60)").into());
    }
    Ok(())
}

#[test]
fn for_over_f32_array_uses_f32_load_for_var() -> TestResult {
    // for x in [1.5, 2.5] { x + x } -> [3.0, 5.0] — exercises the F32 load path.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(11);
    let arr = IrExpr::Array {
        elements: vec![
            IrExpr::Literal {
                value: Literal::Number(NumberLiteral::suffixed(
                    NumberValue::Float(1.5),
                    NumericSuffix::F32,
                )),
                ty: primitive(PrimitiveType::F32),
                span: IrSpan::default(),
            },
            IrExpr::Literal {
                value: Literal::Number(NumberLiteral::suffixed(
                    NumberValue::Float(2.5),
                    NumericSuffix::F32,
                )),
                ty: primitive(PrimitiveType::F32),
                span: IrSpan::default(),
            },
        ],
        ty: array_ty(primitive(PrimitiveType::F32)),
        span: IrSpan::default(),
    };
    let body = IrExpr::BinaryOp {
        left: Box::new(typed_let_ref(var_id, "x", primitive(PrimitiveType::F32))),
        right: Box::new(typed_let_ref(var_id, "x", primitive(PrimitiveType::F32))),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::F32),
        span: IrSpan::default(),
    };
    module.functions.push(function(
        "doubled",
        array_ty(primitive(PrimitiveType::F32)),
        for_over_array(
            "x",
            var_id,
            primitive(PrimitiveType::F32),
            arr,
            body,
            primitive(PrimitiveType::F32),
        ),
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "doubled")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (2, 2) {
        return Err(format!("len/cap: got ({len}, {cap}), want (2, 2)").into());
    }
    let buf: [u8; 8] = read_memory(&mut store, &instance, buf_ptr)?;
    let v0 = f32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let v1 = f32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if (v0 - 3.0).abs() > f32::EPSILON || (v1 - 5.0).abs() > f32::EPSILON {
        return Err(format!("got ({v0}, {v1}), want (3.0, 5.0)").into());
    }
    Ok(())
}

#[test]
fn for_over_f32_range_iterates_one_per_step() -> TestResult {
    // for x in 0.0..3.0 { x * x } -> [0.0, 1.0, 4.0]. The loop
    // counter advances by 1.0 each iteration; `len = ceil(3.0) = 3`
    // so the output buffer holds three F32 squares.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(13);
    let f32_ty = primitive(PrimitiveType::F32);

    let lo = IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Float(0.0),
            NumericSuffix::F32,
        )),
        ty: f32_ty.clone(),
        span: IrSpan::default(),
    };
    let hi = IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Float(3.0),
            NumericSuffix::F32,
        )),
        ty: f32_ty.clone(),
        span: IrSpan::default(),
    };
    let range = IrExpr::BinaryOp {
        left: Box::new(lo),
        right: Box::new(hi),
        op: BinaryOperator::Range,
        ty: range_ty(f32_ty.clone()),
        span: IrSpan::default(),
    };
    let body = IrExpr::BinaryOp {
        left: Box::new(typed_let_ref(var_id, "x", f32_ty.clone())),
        right: Box::new(typed_let_ref(var_id, "x", f32_ty.clone())),
        op: BinaryOperator::Mul,
        ty: f32_ty.clone(),
        span: IrSpan::default(),
    };
    let for_loop = IrExpr::For {
        var: "x".to_owned(),
        var_ty: f32_ty.clone(),
        var_binding_id: var_id,
        collection: Box::new(range),
        body: Box::new(body),
        ty: array_ty(f32_ty.clone()),
        span: IrSpan::default(),
    };
    module
        .functions
        .push(function("squares", array_ty(f32_ty), for_loop));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "squares")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }
    let buf: [u8; 12] = read_memory(&mut store, &instance, buf_ptr)?;
    let mut vals = [0_f32; 3];
    for (slot, chunk) in vals.iter_mut().zip(buf.chunks_exact(4)) {
        let bytes: [u8; 4] = chunk
            .try_into()
            .map_err(|_| -> TestError { "chunk length not 4".into() })?;
        *slot = f32::from_le_bytes(bytes);
    }
    let want = [0.0_f32, 1.0, 4.0];
    if vals
        .iter()
        .zip(want.iter())
        .any(|(g, w)| (g - w).abs() > f32::EPSILON)
    {
        return Err(format!("got {vals:?}, want {want:?}").into());
    }
    Ok(())
}

#[test]
fn for_over_f64_range_iterates_one_per_step() -> TestResult {
    // for x in 1.0..4.0 { x + x } -> [2.0, 4.0, 6.0]. Mirrors the
    // F32 path above against the F64 typed-arithmetic helpers.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    let var_id = BindingId(14);
    let f64_ty = primitive(PrimitiveType::F64);

    let lo = IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Float(1.0),
            NumericSuffix::F64,
        )),
        ty: f64_ty.clone(),
        span: IrSpan::default(),
    };
    let hi = IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Float(4.0),
            NumericSuffix::F64,
        )),
        ty: f64_ty.clone(),
        span: IrSpan::default(),
    };
    let range = IrExpr::BinaryOp {
        left: Box::new(lo),
        right: Box::new(hi),
        op: BinaryOperator::Range,
        ty: range_ty(f64_ty.clone()),
        span: IrSpan::default(),
    };
    let body = IrExpr::BinaryOp {
        left: Box::new(typed_let_ref(var_id, "x", f64_ty.clone())),
        right: Box::new(typed_let_ref(var_id, "x", f64_ty.clone())),
        op: BinaryOperator::Add,
        ty: f64_ty.clone(),
        span: IrSpan::default(),
    };
    let for_loop = IrExpr::For {
        var: "x".to_owned(),
        var_ty: f64_ty.clone(),
        var_binding_id: var_id,
        collection: Box::new(range),
        body: Box::new(body),
        ty: array_ty(f64_ty.clone()),
        span: IrSpan::default(),
    };
    module
        .functions
        .push(function("doubled", array_ty(f64_ty), for_loop));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "doubled")?;
    let header_ptr = f.call(&mut store, ())?;

    let (buf_ptr, len, cap) = read_header(&mut store, &instance, header_ptr)?;
    if (len, cap) != (3, 3) {
        return Err(format!("len/cap: got ({len}, {cap}), want (3, 3)").into());
    }
    let buf: [u8; 24] = read_memory(&mut store, &instance, buf_ptr)?;
    let mut vals = [0_f64; 3];
    for (slot, chunk) in vals.iter_mut().zip(buf.chunks_exact(8)) {
        let bytes: [u8; 8] = chunk
            .try_into()
            .map_err(|_| -> TestError { "chunk length not 8".into() })?;
        *slot = f64::from_le_bytes(bytes);
    }
    let want = [2.0_f64, 4.0, 6.0];
    if vals
        .iter()
        .zip(want.iter())
        .any(|(g, w)| (g - w).abs() > f64::EPSILON)
    {
        return Err(format!("got {vals:?}, want {want:?}").into());
    }
    Ok(())
}
