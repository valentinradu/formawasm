//! End-to-end tests for `IrBlockStatement::Assign` field writes.
//!
//! Phase 1b mc9 lowers assigns whose target is a `SelfFieldRef` or
//! `FieldAccess` — i.e. mutating a struct/tuple field through a known
//! pointer. Mutable primitive `let` bindings (boxing in linear
//! memory) ride later phases.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, DispatchKind, FieldIdx, ImplId, ImplTarget, IrBlockStatement, IrExpr, IrField,
    IrFunction, IrFunctionParam, IrImpl, IrModule, IrStruct, MethodIdx, ResolvedType, StructId,
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

fn struct_def(name: &str, fields: Vec<(&str, PrimitiveType, bool)>) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: fields
            .into_iter()
            .map(|(n, ty, mutable)| IrField {
                name: n.to_owned(),
                ty: primitive(ty),
                mutable,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
            })
            .collect(),
        generic_params: Vec::new(),
        doc: None,
    }
}

#[test]
fn method_can_mutate_self_field_and_caller_observes_change() -> TestResult {
    // struct Counter { mut v: I32 }
    // impl Counter { fn bump(self) -> I32 { self.v = self.v + 1; self.v } }
    // fn run() -> I32 { Counter { v: 41 }.bump() }
    use formalang::ast::BinaryOperator;

    let mut module = IrModule::new();
    module
        .structs
        .push(struct_def("Counter", vec![("v", PrimitiveType::I32, true)]));

    // Method body: a Block that first assigns self.v = self.v + 1,
    // then evaluates self.v as the result.
    let read_v = IrExpr::SelfFieldRef {
        field: "v".to_owned(),
        field_idx: FieldIdx(0),
        ty: primitive(PrimitiveType::I32),
    };
    let inc = IrExpr::BinaryOp {
        left: Box::new(read_v.clone()),
        right: Box::new(integer_literal(1, PrimitiveType::I32)),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::I32),
    };
    let assign_stmt = IrBlockStatement::Assign {
        target: IrExpr::SelfFieldRef {
            field: "v".to_owned(),
            field_idx: FieldIdx(0),
            ty: primitive(PrimitiveType::I32),
        },
        value: inc,
    };
    let method_body = IrExpr::Block {
        statements: vec![assign_stmt],
        result: Box::new(read_v),
        ty: primitive(PrimitiveType::I32),
    };

    let bump_method = IrFunction {
        name: "bump".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "self".to_owned(),
            external_label: None,
            ty: Some(ResolvedType::Struct(StructId(0))),
            default: None,
            convention: ParamConvention::Let,
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(method_body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };
    module.impls.push(IrImpl {
        target: ImplTarget::Struct(StructId(0)),
        trait_ref: None,
        is_extern: false,
        generic_params: Vec::new(),
        functions: vec![bump_method],
    });

    let receiver = IrExpr::StructInst {
        struct_id: Some(StructId(0)),
        type_args: Vec::new(),
        fields: vec![(
            "v".to_owned(),
            FieldIdx(0),
            integer_literal(41, PrimitiveType::I32),
        )],
        ty: ResolvedType::Struct(StructId(0)),
    };
    let body = IrExpr::MethodCall {
        receiver: Box::new(receiver),
        method: "bump".to_owned(),
        method_idx: MethodIdx(0),
        args: Vec::new(),
        dispatch: DispatchKind::Static { impl_id: ImplId(0) },
        ty: primitive(PrimitiveType::I32),
    };
    module.functions.push(IrFunction {
        name: "run".to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(PrimitiveType::I32)),
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
    let f = instance.get_typed_func::<(), i32>(&mut store, "run")?;
    let got = f.call(&mut store, ())?;
    if got != 42 {
        return Err(format!("got {got}, want 42").into());
    }
    Ok(())
}
