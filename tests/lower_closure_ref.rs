//! Tests for `lower::lower_closure_ref`.
//!
//! Phase 1b mc11 lays out a closure value as an `(i32 funcref,
//! i32 env_ptr)` pair in linear memory. Indirect invocation through a
//! funcref table lands later, so the test confirms only that the
//! emitted module validates and that the materialized pair holds the
//! right wasm function index plus a valid env pointer.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrStruct, ResolvedType, StructId,
};
use formawasm::module_lowering;
use formawasm::types::{CLOSURE_ENV_OFFSET, CLOSURE_FUNCREF_OFFSET};
use wasmparser::{Validator, WasmFeatures};
use wasmtime::{Engine, Instance, Module, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

fn integer_literal(value: i128, ty: PrimitiveType) -> IrExpr {
    let suffix = if ty == PrimitiveType::I64 {
        NumericSuffix::I64
    } else {
        NumericSuffix::I32
    };
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(NumberValue::Integer(value), suffix)),
        ty: ResolvedType::Primitive(ty),
    }
}

#[test]
fn closure_ref_materializes_a_pair_in_linear_memory() -> TestResult {
    // Lifted "closure body" function: `__closure_0(env: ()) -> I32 { 7 }`.
    // The env type for our test is the empty struct `Env`.
    // ClosureRef points to __closure_0 with `Env { }` as the env value.
    // The caller `make_closure() -> ClosurePtr (i32)` returns the
    // closure value's base pointer.
    let mut module = IrModule::new();

    // Empty env struct.
    module.structs.push(IrStruct {
        name: "Env".to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: Vec::new(),
        generic_params: Vec::new(),
        doc: None,
    });

    // The lifted closure function (just returns 7).
    let lifted = IrFunction {
        name: "__closure_0".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "env".to_owned(),
            external_label: None,
            ty: Some(ResolvedType::Struct(StructId(0))),
            default: None,
            convention: ParamConvention::Let,
        }],
        return_type: Some(ResolvedType::Primitive(PrimitiveType::I32)),
        body: Some(integer_literal(7, PrimitiveType::I32)),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };

    // Build the env-struct construction expression.
    let env = IrExpr::StructInst {
        struct_id: Some(StructId(0)),
        type_args: Vec::new(),
        fields: Vec::new(),
        ty: ResolvedType::Struct(StructId(0)),
    };
    let closure_ty = ResolvedType::Closure {
        param_tys: Vec::new(),
        return_ty: Box::new(ResolvedType::Primitive(PrimitiveType::I32)),
    };
    let closure_ref = IrExpr::ClosureRef {
        funcref: vec!["__closure_0".to_owned()],
        env_struct: Box::new(env),
        ty: closure_ty.clone(),
    };

    let make_closure = IrFunction {
        name: "make_closure".to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(closure_ty),
        body: Some(closure_ref),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };

    module.functions.push(lifted);
    module.functions.push(make_closure);

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "make_closure")?;
    let ptr = f.call(&mut store, ())?;
    let memory = instance
        .exports(&mut store)
        .find_map(wasmtime::Export::into_memory)
        .ok_or("no memory in instance")?;

    let mut buf = [0u8; 8];
    let offset = usize::try_from(ptr).map_err(|_| -> TestError { "ptr negative".into() })?;
    memory.read(&store, offset, &mut buf)?;
    let funcref_bytes: [u8; 4] = buf
        .get(CLOSURE_FUNCREF_OFFSET as usize..CLOSURE_FUNCREF_OFFSET as usize + 4)
        .ok_or("buf too small for funcref")?
        .try_into()?;
    let env_bytes: [u8; 4] = buf
        .get(CLOSURE_ENV_OFFSET as usize..CLOSURE_ENV_OFFSET as usize + 4)
        .ok_or("buf too small for env")?
        .try_into()?;
    let funcref_idx = i32::from_le_bytes(funcref_bytes);
    let env_ptr = i32::from_le_bytes(env_bytes);

    // funcref_idx is the wasm function index of `__closure_0`.
    // With 1 bump-allocator (idx 0) + __closure_0 (idx 1) + make_closure (idx 2),
    // the lifted function lives at wasm index 1.
    if funcref_idx != 1 {
        return Err(format!("expected funcref idx 1, got {funcref_idx}").into());
    }
    // env_ptr should be a non-negative aligned offset (an empty struct
    // still allocates 0 bytes via the bump allocator, so it gets some
    // valid pointer).
    if env_ptr < 0 {
        return Err(format!("env_ptr negative: {env_ptr}").into());
    }
    Ok(())
}
