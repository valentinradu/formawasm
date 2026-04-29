//! Tests for `ParamConvention::Mut` and `ParamConvention::Sink`.
//!
//! In our linear-memory model both conventions lower identically to
//! `Let` at the wasm level: aggregates flow through as `i32`
//! pointers and primitives flow through as their wasm-native value
//! types. The conventions matter at the IR / type-checker level (no
//! reads of a `Sink`'d binding after the call, etc.) but the backend
//! has no extra work to do for them — these tests pin that contract.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, FieldIdx, IrExpr, IrField, IrFunction, IrFunctionParam, IrModule, IrStruct,
    ReferenceTarget, ResolvedType, StructId,
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
    }
}

fn pair_struct() -> IrStruct {
    IrStruct {
        name: "Pair".to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: vec![
            IrField {
                name: "a".to_owned(),
                ty: primitive(PrimitiveType::I32),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
            },
            IrField {
                name: "b".to_owned(),
                ty: primitive(PrimitiveType::I32),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
            },
        ],
        generic_params: Vec::new(),
        doc: None,
    }
}

fn pair_inst(a: i128, b: i128) -> IrExpr {
    IrExpr::StructInst {
        struct_id: Some(StructId(0)),
        type_args: Vec::new(),
        fields: vec![
            (
                "a".to_owned(),
                FieldIdx(0),
                integer_literal(a, PrimitiveType::I32),
            ),
            (
                "b".to_owned(),
                FieldIdx(1),
                integer_literal(b, PrimitiveType::I32),
            ),
        ],
        ty: ResolvedType::Struct(StructId(0)),
    }
}

#[test]
fn sink_aggregate_param_routes_pointer() -> TestResult {
    // struct Pair { a: I32, b: I32 }
    // fn first(sink p: Pair) -> I32 { p.a }
    // fn run() -> I32 { first(Pair { a: 5, b: 7 }) }
    let mut module = IrModule::new();
    module.structs.push(pair_struct());

    // first(sink p: Pair) -> I32 { p.a }
    let p_ref = IrExpr::Reference {
        path: vec!["p".to_owned()],
        target: ReferenceTarget::Param(BindingId(0)),
        ty: ResolvedType::Struct(StructId(0)),
    };
    let body = IrExpr::FieldAccess {
        object: Box::new(p_ref),
        field: "a".to_owned(),
        field_idx: FieldIdx(0),
        ty: primitive(PrimitiveType::I32),
    };
    let first = IrFunction {
        name: "first".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "p".to_owned(),
            external_label: None,
            ty: Some(ResolvedType::Struct(StructId(0))),
            default: None,
            convention: ParamConvention::Sink,
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };
    module.functions.push(first);

    // fn run() -> I32 { first(Pair { a: 5, b: 7 }) }
    let pair = pair_inst(5, 7);
    let call = IrExpr::FunctionCall {
        path: vec!["first".to_owned()],
        function_id: Some(formalang::ir::FunctionId(0)),
        args: vec![(None, pair)],
        ty: primitive(PrimitiveType::I32),
    };
    module.functions.push(IrFunction {
        name: "run".to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(call),
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
    let f = instance.get_typed_func::<(), i32>(&mut store, "run")?;
    let got = f.call(&mut store, ())?;
    if got != 5 {
        return Err(format!("got {got}, want 5").into());
    }
    Ok(())
}

#[test]
fn mut_aggregate_param_passes_through() -> TestResult {
    // Same shape as the sink test but the param convention is Mut.
    // The behavior is identical at the wasm level — the test pins
    // that the backend doesn't reject Mut.
    let mut module = IrModule::new();
    module.structs.push(pair_struct());

    let p_ref = IrExpr::Reference {
        path: vec!["p".to_owned()],
        target: ReferenceTarget::Param(BindingId(0)),
        ty: ResolvedType::Struct(StructId(0)),
    };
    let body = IrExpr::FieldAccess {
        object: Box::new(p_ref),
        field: "b".to_owned(),
        field_idx: FieldIdx(1),
        ty: primitive(PrimitiveType::I32),
    };
    let f = IrFunction {
        name: "second".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "p".to_owned(),
            external_label: None,
            ty: Some(ResolvedType::Struct(StructId(0))),
            default: None,
            convention: ParamConvention::Mut,
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };
    module.functions.push(f);

    let pair = pair_inst(5, 7);
    let call = IrExpr::FunctionCall {
        path: vec!["second".to_owned()],
        function_id: Some(formalang::ir::FunctionId(0)),
        args: vec![(None, pair)],
        ty: primitive(PrimitiveType::I32),
    };
    module.functions.push(IrFunction {
        name: "run".to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(call),
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
    let f = instance.get_typed_func::<(), i32>(&mut store, "run")?;
    let got = f.call(&mut store, ())?;
    if got != 7 {
        return Err(format!("got {got}, want 7").into());
    }
    Ok(())
}
