//! Coverage for the cross-site Optional<T> coercion paths beyond
//! let-bindings (Phase 2 mc4 covered Let; this file exercises function
//! returns and if branches).
//!
//! Each scenario builds a small `IrModule`, runs it through the
//! production `module_lowering::lower_module` pipeline, validates the
//! emitted core wasm, instantiates under wasmtime, and reads back the
//! tag/payload bytes from linear memory.

mod common;
use common::{seed_prelude, optional_ty, array_ty, range_ty, dict_ty};

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget, ResolvedType,
};
use formawasm::layout::{OPTIONAL_TAG_NIL, OPTIONAL_TAG_SOME, plan_optional};
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

fn nil_literal() -> IrExpr {
    IrExpr::Literal {
        value: Literal::Nil,
        ty: optional(primitive(PrimitiveType::Never)),
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

#[test]
fn function_return_some_wraps_plain_value() -> TestResult {
    // `fn maybe() -> I32? { 42 }` — body's static type is I32, return
    // type is Optional<I32>. The return-coercion site wraps the I32
    // into a Some(Optional<I32>) cell.
    let body = integer_literal(42);
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "maybe",
        Vec::new(),
        optional(primitive(PrimitiveType::I32)),
        body,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "maybe")?;
    let ptr = f.call(&mut store, ())?;

    let layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    if layout.size != 8 {
        return Err(format!("Optional<I32> size: got {}, want 8", layout.size).into());
    }
    let mem: [u8; 8] = read_memory(&mut store, &instance, ptr)?;
    let tag = u32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i32::from_le_bytes([mem[4], mem[5], mem[6], mem[7]]);
    if payload != 42 {
        return Err(format!("payload: got {payload}, want 42").into());
    }
    Ok(())
}

#[test]
fn if_branch_some_wrap_then_arm_passthrough_else_arm_nil() -> TestResult {
    // `fn classify(n: I32) -> I32? {
    //     if n > 0 { n } else { nil }
    // }`
    //
    // The if-expression's static type is `Optional<I32>` (unification
    // of `I32` and `Optional<Never>`). The then-arm produces a plain
    // I32 and gets Some-wrapped at the if-coercion site; the else-arm
    // is already `Optional<Never>` and flows through as a tag-only
    // pointer. The function-return site sees a value already typed
    // `Optional<I32>` so no further wrap fires.
    let n_binding = BindingId(0);
    let n_ref = IrExpr::Reference {
        path: vec!["n".to_owned()],
        target: ReferenceTarget::Param(n_binding),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let cond = IrExpr::BinaryOp {
        left: Box::new(n_ref.clone()),
        op: BinaryOperator::Gt,
        right: Box::new(integer_literal(0)),
        ty: primitive(PrimitiveType::Boolean),
        span: IrSpan::default(),
    };
    let body = IrExpr::If {
        condition: Box::new(cond),
        then_branch: Box::new(n_ref),
        else_branch: Some(Box::new(nil_literal())),
        ty: optional(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };

    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "classify",
        vec![function_param("n", primitive(PrimitiveType::I32))],
        optional(primitive(PrimitiveType::I32)),
        body,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<i32, i32>(&mut store, "classify")?;

    // Positive input: Some-wrapped value.
    let ptr_some = f.call(&mut store, 7)?;
    let layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    if layout.size != 8 {
        return Err(format!("Optional<I32> size: got {}, want 8", layout.size).into());
    }
    let mem: [u8; 8] = read_memory(&mut store, &instance, ptr_some)?;
    let tag = u32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("then-arm tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i32::from_le_bytes([mem[4], mem[5], mem[6], mem[7]]);
    if payload != 7 {
        return Err(format!("then-arm payload: got {payload}, want 7").into());
    }

    // Non-positive input: nil pointer (tag-only allocation).
    let ptr_nil = f.call(&mut store, -3)?;
    let mem: [u8; 4] = read_memory(&mut store, &instance, ptr_nil)?;
    let tag = u32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]);
    if tag != OPTIONAL_TAG_NIL {
        return Err(format!("else-arm tag: got {tag}, want {OPTIONAL_TAG_NIL}").into());
    }
    Ok(())
}

#[test]
fn if_branch_nil_then_some_else_swaps_arms_correctly() -> TestResult {
    // Same widening logic as the previous test, but with the arms
    // swapped to confirm the coercion fires on either side.
    //
    // `fn classify(n: I32) -> I32? {
    //     if n > 0 { nil } else { n }
    // }`
    let n_binding = BindingId(0);
    let n_ref = IrExpr::Reference {
        path: vec!["n".to_owned()],
        target: ReferenceTarget::Param(n_binding),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let cond = IrExpr::BinaryOp {
        left: Box::new(n_ref.clone()),
        op: BinaryOperator::Gt,
        right: Box::new(integer_literal(0)),
        ty: primitive(PrimitiveType::Boolean),
        span: IrSpan::default(),
    };
    let body = IrExpr::If {
        condition: Box::new(cond),
        then_branch: Box::new(nil_literal()),
        else_branch: Some(Box::new(n_ref)),
        ty: optional(primitive(PrimitiveType::I32)),
        span: IrSpan::default(),
    };

    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "classify",
        vec![function_param("n", primitive(PrimitiveType::I32))],
        optional(primitive(PrimitiveType::I32)),
        body,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let (mut store, instance) = instantiate(&bytes)?;
    let f = instance.get_typed_func::<i32, i32>(&mut store, "classify")?;

    let ptr_nil = f.call(&mut store, 5)?;
    let mem: [u8; 4] = read_memory(&mut store, &instance, ptr_nil)?;
    let tag = u32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]);
    if tag != OPTIONAL_TAG_NIL {
        return Err(format!("then-arm (nil) tag: got {tag}, want {OPTIONAL_TAG_NIL}").into());
    }

    let ptr_some = f.call(&mut store, -8)?;
    let layout = plan_optional(&primitive(PrimitiveType::I32), &module)?;
    if layout.size != 8 {
        return Err(format!("Optional<I32> size: got {}, want 8", layout.size).into());
    }
    let mem: [u8; 8] = read_memory(&mut store, &instance, ptr_some)?;
    let tag = u32::from_le_bytes([mem[0], mem[1], mem[2], mem[3]]);
    if tag != OPTIONAL_TAG_SOME {
        return Err(format!("else-arm tag: got {tag}, want {OPTIONAL_TAG_SOME}").into());
    }
    let payload = i32::from_le_bytes([mem[4], mem[5], mem[6], mem[7]]);
    if payload != -8 {
        return Err(format!("else-arm payload: got {payload}, want -8").into());
    }
    Ok(())
}
