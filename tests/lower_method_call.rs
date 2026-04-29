//! End-to-end tests for `lower::lower_method_call` (static dispatch).
//!
//! Each test builds a struct + an inherent impl with one method, plus
//! a top-level "make" function that constructs an instance and calls
//! the method. The make function is exported, so wasmtime can drive
//! the whole pipeline and read back the method's primitive return
//! value.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, DispatchKind, FieldIdx, ImplId, ImplTarget, IrExpr, IrField, IrFunction,
    IrFunctionParam, IrImpl, IrModule, IrStruct, MethodIdx, ResolvedType, StructId,
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

fn struct_def(name: &str, fields: Vec<(&str, PrimitiveType)>) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: fields
            .into_iter()
            .map(|(n, ty)| IrField {
                name: n.to_owned(),
                ty: primitive(ty),
                mutable: false,
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

fn struct_inst(struct_id: StructId, fields: Vec<(&str, IrExpr)>) -> IrExpr {
    IrExpr::StructInst {
        struct_id: Some(struct_id),
        type_args: Vec::new(),
        fields: fields
            .into_iter()
            .enumerate()
            .map(|(i, (name, e))| (name.to_owned(), FieldIdx(u32::try_from(i).unwrap_or(0)), e))
            .collect(),
        ty: ResolvedType::Struct(struct_id),
    }
}

fn self_param(struct_id: StructId, binding_id: u32) -> IrFunctionParam {
    IrFunctionParam {
        binding_id: BindingId(binding_id),
        name: "self".to_owned(),
        external_label: None,
        ty: Some(ResolvedType::Struct(struct_id)),
        default: None,
        convention: ParamConvention::Let,
    }
}

fn method_fn(
    name: &str,
    struct_id: StructId,
    return_ty: PrimitiveType,
    body: IrExpr,
) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: vec![self_param(struct_id, 0)],
        return_type: Some(primitive(return_ty)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    }
}

fn method_call(
    receiver: IrExpr,
    method: &str,
    method_idx: u32,
    impl_id: ImplId,
    args: Vec<IrExpr>,
    ty: PrimitiveType,
) -> IrExpr {
    IrExpr::MethodCall {
        receiver: Box::new(receiver),
        method: method.to_owned(),
        method_idx: MethodIdx(method_idx),
        args: args.into_iter().map(|e| (None, e)).collect(),
        dispatch: DispatchKind::Static { impl_id },
        ty: primitive(ty),
    }
}

#[test]
fn calls_inherent_method_returning_self_field() -> TestResult {
    // struct Pair { a: I32, b: I32 }
    // impl Pair { fn first(self) -> I32 { self.a } }
    // fn make_and_call() -> I32 { Pair { a: 11, b: 22 }.first() }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Pair",
        vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I32)],
    ));

    // Inherent impl with one method: `first(self) -> I32 { self.a }`.
    let method_body = IrExpr::SelfFieldRef {
        field: "a".to_owned(),
        field_idx: FieldIdx(0),
        ty: primitive(PrimitiveType::I32),
    };
    module.impls.push(IrImpl {
        target: ImplTarget::Struct(StructId(0)),
        trait_ref: None,
        is_extern: false,
        generic_params: Vec::new(),
        functions: vec![method_fn(
            "first",
            StructId(0),
            PrimitiveType::I32,
            method_body,
        )],
    });

    // Top-level caller.
    let receiver = struct_inst(
        StructId(0),
        vec![
            ("a", integer_literal(11, PrimitiveType::I32)),
            ("b", integer_literal(22, PrimitiveType::I32)),
        ],
    );
    let body = method_call(
        receiver,
        "first",
        0,
        ImplId(0),
        Vec::new(),
        PrimitiveType::I32,
    );
    module.functions.push(IrFunction {
        name: "make_and_call".to_owned(),
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
    let f = instance.get_typed_func::<(), i32>(&mut store, "make_and_call")?;
    let got = f.call(&mut store, ())?;
    if got != 11 {
        return Err(format!("got {got}, want 11").into());
    }
    Ok(())
}

#[test]
fn method_with_extra_arg_is_routed_correctly() -> TestResult {
    // struct Counter { v: I32 }
    // impl Counter { fn add(self, n: I32) -> I32 { self.v + n } }   (we cheat: arm body uses BinaryOp::Add)
    // fn run() -> I32 { Counter { v: 10 }.add(5) }
    use formalang::ast::BinaryOperator;
    use formalang::ir::ReferenceTarget;

    let mut module = IrModule::new();
    module
        .structs
        .push(struct_def("Counter", vec![("v", PrimitiveType::I32)]));

    let self_v = IrExpr::SelfFieldRef {
        field: "v".to_owned(),
        field_idx: FieldIdx(0),
        ty: primitive(PrimitiveType::I32),
    };
    let n_ref = IrExpr::Reference {
        path: vec!["n".to_owned()],
        target: ReferenceTarget::Param(BindingId(1)),
        ty: primitive(PrimitiveType::I32),
    };
    let method_body = IrExpr::BinaryOp {
        left: Box::new(self_v),
        right: Box::new(n_ref),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::I32),
    };
    let mut add_method = method_fn("add", StructId(0), PrimitiveType::I32, method_body);
    add_method.params.push(IrFunctionParam {
        binding_id: BindingId(1),
        name: "n".to_owned(),
        external_label: None,
        ty: Some(primitive(PrimitiveType::I32)),
        default: None,
        convention: ParamConvention::Let,
    });
    module.impls.push(IrImpl {
        target: ImplTarget::Struct(StructId(0)),
        trait_ref: None,
        is_extern: false,
        generic_params: Vec::new(),
        functions: vec![add_method],
    });

    let receiver = struct_inst(
        StructId(0),
        vec![("v", integer_literal(10, PrimitiveType::I32))],
    );
    let body = method_call(
        receiver,
        "add",
        0,
        ImplId(0),
        vec![integer_literal(5, PrimitiveType::I32)],
        PrimitiveType::I32,
    );
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
    if got != 15 {
        return Err(format!("got {got}, want 15").into());
    }
    Ok(())
}
