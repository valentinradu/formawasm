//! Tests for `component::wrap_component`.
//!
//! Cover the empty-module case, the fibonacci end-to-end case under
//! wasmtime's component runtime, and the obvious failure modes (bad
//! WIT, mismatched export names).

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget,
    ResolvedType,
};
use formawasm::component::{self, ComponentWrapError};
use formawasm::module_lowering;
use formawasm::survey;
use formawasm::wit;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

const fn primitive_ty(p: PrimitiveType) -> ResolvedType {
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
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

fn param_ref(id: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::Reference {
        path: vec![format!("p{id}")],
        target: ReferenceTarget::Param(BindingId(id)),
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

fn binary_op(op: BinaryOperator, left: IrExpr, right: IrExpr, ty: PrimitiveType) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(left),
        right: Box::new(right),
        op,
        ty: primitive_ty(ty),
        span: IrSpan::default(),
    }
}

fn function(
    name: &str,
    params: Vec<(BindingId, &str, PrimitiveType)>,
    return_ty: PrimitiveType,
    body: IrExpr,
) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: params
            .into_iter()
            .map(|(id, n, ty)| IrFunctionParam {
                binding_id: id,
                name: n.to_owned(),
                external_label: None,
                ty: Some(primitive_ty(ty)),
                default: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            })
            .collect(),
        return_type: Some(primitive_ty(return_ty)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn fibonacci_function() -> IrFunction {
    let cond = binary_op(
        BinaryOperator::Lt,
        param_ref(0, PrimitiveType::I32),
        integer_literal(2, PrimitiveType::I32),
        PrimitiveType::Boolean,
    );
    let recurse_one_less = IrExpr::FunctionCall {
        path: vec!["fib".to_owned()],
        function_id: Some(FunctionId(0)),
        args: vec![(
            None,
            binary_op(
                BinaryOperator::Sub,
                param_ref(0, PrimitiveType::I32),
                integer_literal(1, PrimitiveType::I32),
                PrimitiveType::I32,
            ),
        )],
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let recurse_two_less = IrExpr::FunctionCall {
        path: vec!["fib".to_owned()],
        function_id: Some(FunctionId(0)),
        args: vec![(
            None,
            binary_op(
                BinaryOperator::Sub,
                param_ref(0, PrimitiveType::I32),
                integer_literal(2, PrimitiveType::I32),
                PrimitiveType::I32,
            ),
        )],
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let sum = binary_op(
        BinaryOperator::Add,
        recurse_one_less,
        recurse_two_less,
        PrimitiveType::I32,
    );
    let body = IrExpr::If {
        condition: Box::new(cond),
        then_branch: Box::new(param_ref(0, PrimitiveType::I32)),
        else_branch: Some(Box::new(sum)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    function(
        "fib",
        vec![(BindingId(0), "n", PrimitiveType::I32)],
        PrimitiveType::I32,
        body,
    )
}

#[test]
fn empty_module_wraps_to_valid_component() -> TestResult {
    let module = IrModule::new();
    let surface = survey::survey(&module);
    let core = module_lowering::lower_module(&module)?;
    let wit = wit::emit_wit(&module, &surface)?;

    let bytes = component::wrap_component(core, &wit)?;
    validate_component(&bytes)
}

#[test]
fn fibonacci_runs_under_wasmtime_component() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(fibonacci_function());
    let surface = survey::survey(&module);
    let core = module_lowering::lower_module(&module)?;
    let wit = wit::emit_wit(&module, &surface)?;
    let bytes = component::wrap_component(core, &wit)?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let fib = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "fib")?;

    let expected: &[i32] = &[0, 1, 1, 2, 3, 5, 8, 13, 21, 34];
    for (n, want) in expected.iter().enumerate() {
        let n = i32::try_from(n).map_err(|_| -> TestError { "n overflow".into() })?;
        let (got,) = fib.call(&mut store, (n,))?;
        if got != *want {
            return Err(format!("fib({n}) = {got}, want {want}").into());
        }
    }
    Ok(())
}

#[test]
fn unparseable_wit_is_rejected() -> TestResult {
    let module = IrModule::new();
    let core = module_lowering::lower_module(&module)?;
    match component::wrap_component(core, "not valid wit at all") {
        Err(ComponentWrapError::WitParse { .. }) => Ok(()),
        other => Err(format!("expected WitParse, got {other:?}").into()),
    }
}

#[test]
fn wit_without_named_world_is_rejected() -> TestResult {
    // Parses, but declares a world named differently from `WORLD_NAME`,
    // so `select_world` fails with `WorldMissing`.
    let wit_text = "package formawasm:generated;\nworld other-world {}\n";
    let module = IrModule::new();
    let core = module_lowering::lower_module(&module)?;
    match component::wrap_component(core, wit_text) {
        Err(ComponentWrapError::WorldMissing { .. }) => Ok(()),
        other => Err(format!("expected WorldMissing, got {other:?}").into()),
    }
}
