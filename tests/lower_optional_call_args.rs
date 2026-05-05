//! End-to-end coverage for `Optional<T>` Some-wrap at function-call
//! argument sites.
//!
//! Builds a two-function module where the caller passes a plain `T`
//! into a callee whose parameter is typed `Optional<T>`. The callee
//! returns the pointer it receives so the test can read the tag and
//! payload bytes off the wrapped allocation through wasmtime.

mod common;
use common::{seed_prelude, optional_ty, array_ty, range_ty, dict_ty};

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType,
};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget,
    ResolvedType,
};
use formawasm::layout::{OPTIONAL_TAG_SOME, plan_optional};
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

fn optional(inner: ResolvedType) -> ResolvedType {
    optional_ty(inner)
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

fn function_param(name: &str, ty: ResolvedType) -> IrFunctionParam {
    IrFunctionParam {
        binding_id: BindingId(0),
        name: name.to_owned(),
        external_label: None,
        ty: Some(ty),
        default: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

fn function(
    name: &str,
    params: Vec<IrFunctionParam>,
    return_ty: ResolvedType,
    body: IrExpr,
) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params,
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
    let off = usize::try_from(offset).map_err(|_| -> TestError { "ptr negative".into() })?;
    let mut buf = [0u8; N];
    memory.read(&*store, off, &mut buf)?;
    Ok(buf)
}

#[test]
fn function_call_arg_some_wraps_plain_value() -> TestResult {
    // ```
    // fn echo(x: I32?) -> I32? { x }
    // fn caller() -> I32? { echo(42) }
    // ```
    //
    // `caller` calls `echo(42)` where 42 is a plain I32 and echo's
    // parameter is Optional<I32>. The call-arg coercion site wraps
    // 42 into a tagged-Some cell before the call. echo just returns
    // its parameter through, so the test reads the tag and payload
    // off the same wrapped allocation.
    let echo_param_id = BindingId(0);
    let echo_body = IrExpr::Reference {
        path: vec!["x".to_owned()],
        target: ReferenceTarget::Param(echo_param_id),
        ty: optional(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };
    let echo = function(
        "echo",
        vec![function_param("x", optional(primitive(PrimitiveType::I32)))],
        optional(primitive(PrimitiveType::I32)),
        echo_body,
    );

    let caller_body = IrExpr::FunctionCall {
        path: vec!["echo".to_owned()],
        function_id: Some(FunctionId(0)),
        args: vec![(Some("x".to_owned()), integer_literal(42))],
        ty: optional(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };
    let caller = function(
        "caller",
        Vec::new(),
        optional(primitive(PrimitiveType::I32)),
        caller_body,
    );

    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(echo);
    module.functions.push(caller);

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "caller")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    if layout.size != 8 {
        return Err(format!("Optional<I32> size: got {}, want 8", layout.size).into());
    }
    let buf: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
    let tag = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if payload != 42 {
        return Err(format!("payload: got {payload}, want 42").into());
    }
    Ok(())
}
