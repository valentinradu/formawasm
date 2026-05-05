//! Smoke tests for the `WasmBackend::generate` entry point.
//!
//! These exercise the assembled Phase 1a pipeline (preflight, lower,
//! WIT, component wrap) against the smallest valid inputs and confirm
//! the output validates as a Component-Model artifact.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget,
    ResolvedType,
};
use formawasm::{Backend, WasmBackend};
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

#[test]
fn empty_module_generates_a_valid_component() -> TestResult {
    let backend = WasmBackend::new();
    let module = IrModule::new();
    let bytes = backend.generate(&module)?;
    validate_component(&bytes)
}

#[test]
fn emitted_module_carries_a_name_section_for_user_functions() -> TestResult {
    // The custom `name` section is what lets debug tooling show
    // `fib` instead of `func[N]`. A module with one user function
    // should embed both the runtime helpers' names (bump
    // allocator, string helpers, cabi_realloc) and the user fn's
    // kebab-cased name in the emitted bytes.
    let mut module = IrModule::new();
    module.functions.push(fibonacci_function());
    let bytes = WasmBackend::new().generate(&module)?;

    let hay = String::from_utf8_lossy(&bytes);
    for needle in ["__alloc", "__str_eq", "__str_concat", "cabi_realloc", "fib"] {
        if !hay.contains(needle) {
            return Err(format!("missing `{needle}` in name section of emitted bytes").into());
        }
    }
    Ok(())
}

#[test]
fn with_validation_runs_wasmparser_internally() -> TestResult {
    // The `with_validation` builder switches on a wasmparser pass
    // inside `generate`. On a known-good module it should succeed
    // and produce the same bytes as the non-validating path.
    let module = IrModule::new();
    let plain = WasmBackend::new().generate(&module)?;
    let validated = WasmBackend::new().with_validation().generate(&module)?;
    if plain != validated {
        return Err(format!(
            "validation should not alter output: plain = {} bytes, validated = {} bytes",
            plain.len(),
            validated.len()
        )
        .into());
    }
    Ok(())
}

#[test]
fn fibonacci_module_generates_and_runs_under_wasmtime() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(fibonacci_function());

    let backend = WasmBackend::new();
    let bytes = backend.generate(&module)?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let mut linker = Linker::<()>::new(&engine);
    // formalang's prelude declares `pub extern fn assert(condition: Boolean)`;
    // every program parsed via `compile_to_ir_*` imports it. Wire to a host
    // function that traps on `false` so failed assertions surface as wasmtime
    // traps to the test caller.
    linker
        .root()
        .func_wrap("assert", |_store, (cond,): (bool,)| {
            if cond {
                Ok(())
            } else {
                Err(wasmtime::Error::msg("assert(false)"))
            }
        })?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let fib = instance.get_typed_func::<(i32,), (i32,)>(&mut store, "fib")?;

    let (got,) = fib.call(&mut store, (10,))?;
    if got != 55 {
        return Err(format!("fib(10) = {got}, want 55").into());
    }
    Ok(())
}

// ── Fibonacci IR helpers (mirror `tests/lower_module.rs`) ────────────

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
    IrFunction {
        name: "fib".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "n".to_owned(),
            external_label: None,
            ty: Some(primitive_ty(PrimitiveType::I32)),
            default: None,
            convention: ParamConvention::Let,
            span: IrSpan::default(),
        }],
        return_type: Some(primitive_ty(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}
