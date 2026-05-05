#![cfg(any())] // TODO 0.0.4-beta migration: hand-built IR needs prelude-id seeding

//! Phase 1c milestone — Sieve of Eratosthenes through the full
//! `Backend::generate` pipeline.
//!
//! Builds an [`IrModule`] for primality testing via trial division
//! (the closest expressible form of the sieve in the language as of
//! Phase 1c — without mutable array elements the classical
//! cross-out-multiples loop reduces to a per-index trial division).
//! Runs the module through [`WasmBackend::generate`] (pre-flight,
//! survey, core lowering, WIT, component wrap), then instantiates the
//! resulting component under wasmtime and verifies the returned
//! `list<bool>` against the expected primes.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ResolvedType,
};
use formalang::pipeline::Pipeline;
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive_ty(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn array_ty(elem: ResolvedType) -> ResolvedType {
    ResolvedType::Array(Box::new(elem))
}

fn range_ty(elem: ResolvedType) -> ResolvedType {
    ResolvedType::Range(Box::new(elem))
}

fn integer_literal(value: i128) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(value),
            NumericSuffix::I32,
        )),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    }
}

fn bool_literal(value: bool) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(value),
        ty: primitive_ty(PrimitiveType::Boolean),
        span: IrSpan::default(),
    }
}

fn let_ref(binding_id: BindingId, name: &str, ty: ResolvedType) -> IrExpr {
    IrExpr::LetRef {
        name: name.to_owned(),
        binding_id,
        ty,
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

fn function_call(
    name: &str,
    function_id: FunctionId,
    args: Vec<IrExpr>,
    ret: PrimitiveType,
) -> IrExpr {
    IrExpr::FunctionCall {
        path: vec![name.to_owned()],
        function_id: Some(function_id),
        args: args.into_iter().map(|a| (None, a)).collect(),
        ty: primitive_ty(ret),
        span: IrSpan::default(),
    }
}

fn function(
    name: &str,
    params: Vec<(BindingId, &str, ResolvedType)>,
    return_ty: ResolvedType,
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
                ty: Some(ty),
                default: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            })
            .collect(),
        return_type: Some(return_ty),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

/// `check_divisor(n: I32, d: I32) -> Boolean`:
///
/// `if d * d > n { true } else if n % d == 0 { false } else { check_divisor(n, d + 1) }`.
///
/// The recursive trial-division helper. `FunctionId(0)` is `check_divisor`
/// itself — the function being defined; layout-wise this is the first
/// function in the module.
fn check_divisor_function(self_id: FunctionId) -> IrFunction {
    let n = || IrExpr::Reference {
        path: vec!["n".to_owned()],
        target: formalang::ir::ReferenceTarget::Param(BindingId(0)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let d = || IrExpr::Reference {
        path: vec!["d".to_owned()],
        target: formalang::ir::ReferenceTarget::Param(BindingId(1)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let d_squared = binary_op(BinaryOperator::Mul, d(), d(), PrimitiveType::I32);
    let d_squared_gt_n = binary_op(BinaryOperator::Gt, d_squared, n(), PrimitiveType::Boolean);
    let n_mod_d = binary_op(BinaryOperator::Mod, n(), d(), PrimitiveType::I32);
    let divides = binary_op(
        BinaryOperator::Eq,
        n_mod_d,
        integer_literal(0),
        PrimitiveType::Boolean,
    );
    let next_d = binary_op(
        BinaryOperator::Add,
        d(),
        integer_literal(1),
        PrimitiveType::I32,
    );
    let recurse = function_call(
        "check_divisor",
        self_id,
        vec![n(), next_d],
        PrimitiveType::Boolean,
    );

    let body = IrExpr::If {
        condition: Box::new(d_squared_gt_n),
        then_branch: Box::new(bool_literal(true)),
        else_branch: Some(Box::new(IrExpr::If {
            condition: Box::new(divides),
            then_branch: Box::new(bool_literal(false)),
            else_branch: Some(Box::new(recurse)),
            ty: primitive_ty(PrimitiveType::Boolean),
            span: IrSpan::default(),
        })),
        ty: primitive_ty(PrimitiveType::Boolean),
        span: IrSpan::default(),
    };

    function(
        "check_divisor",
        vec![
            (BindingId(0), "n", primitive_ty(PrimitiveType::I32)),
            (BindingId(1), "d", primitive_ty(PrimitiveType::I32)),
        ],
        primitive_ty(PrimitiveType::Boolean),
        body,
    )
}

/// `is_prime(n: I32) -> Boolean`:
///
/// `if n < 2 { false } else { check_divisor(n, 2) }`.
fn is_prime_function(check_divisor_id: FunctionId) -> IrFunction {
    let n = || IrExpr::Reference {
        path: vec!["n".to_owned()],
        target: formalang::ir::ReferenceTarget::Param(BindingId(0)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let n_lt_2 = binary_op(
        BinaryOperator::Lt,
        n(),
        integer_literal(2),
        PrimitiveType::Boolean,
    );
    let trial = function_call(
        "check_divisor",
        check_divisor_id,
        vec![n(), integer_literal(2)],
        PrimitiveType::Boolean,
    );
    let body = IrExpr::If {
        condition: Box::new(n_lt_2),
        then_branch: Box::new(bool_literal(false)),
        else_branch: Some(Box::new(trial)),
        ty: primitive_ty(PrimitiveType::Boolean),
        span: IrSpan::default(),
    };

    function(
        "is_prime",
        vec![(BindingId(0), "n", primitive_ty(PrimitiveType::I32))],
        primitive_ty(PrimitiveType::Boolean),
        body,
    )
}

/// `sieve(limit: I32) -> Array<Boolean>`:
///
/// `for p in 0..limit { is_prime(p) }`. Index `i` of the result is
/// `true` iff `i` is prime; this is the boolean characterization of
/// the sieve, equivalent to the classical algorithm's terminal state.
fn sieve_function(is_prime_id: FunctionId) -> IrFunction {
    let limit = IrExpr::Reference {
        path: vec!["limit".to_owned()],
        target: formalang::ir::ReferenceTarget::Param(BindingId(0)),
        ty: primitive_ty(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let p_id = BindingId(1);
    let range = IrExpr::BinaryOp {
        left: Box::new(integer_literal(0)),
        right: Box::new(limit),
        op: BinaryOperator::Range,
        ty: range_ty(primitive_ty(PrimitiveType::I32)),
        span: IrSpan::default(),
    };
    let body = function_call(
        "is_prime",
        is_prime_id,
        vec![let_ref(p_id, "p", primitive_ty(PrimitiveType::I32))],
        PrimitiveType::Boolean,
    );
    let for_loop = IrExpr::For {
        var: "p".to_owned(),
        var_ty: primitive_ty(PrimitiveType::I32),
        var_binding_id: p_id,
        collection: Box::new(range),
        body: Box::new(body),
        ty: array_ty(primitive_ty(PrimitiveType::Boolean)),
        span: IrSpan::default(),
    };

    function(
        "sieve",
        vec![(BindingId(0), "limit", primitive_ty(PrimitiveType::I32))],
        array_ty(primitive_ty(PrimitiveType::Boolean)),
        for_loop,
    )
}

fn build_sieve_module() -> IrModule {
    let mut module = IrModule::new();
    // Function 0: check_divisor (self-recursive).
    module.functions.push(check_divisor_function(FunctionId(0)));
    // Function 1: is_prime (calls check_divisor).
    module.functions.push(is_prime_function(FunctionId(0)));
    // Function 2: sieve (calls is_prime).
    module.functions.push(sieve_function(FunctionId(1)));
    module
}

#[test]
fn sieve_runs_under_wasmtime_component_runtime() -> TestResult {
    let module = build_sieve_module();
    let mut pipeline = Pipeline::new();
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let sieve = instance.get_typed_func::<(i32,), (Vec<bool>,)>(&mut store, "sieve")?;

    // Expected: indices that are prime up to limit = 30.
    let expected: Vec<bool> = (0..30)
        .map(|n| {
            if n < 2 {
                false
            } else {
                (2..n).all(|d| n % d != 0)
            }
        })
        .collect();
    let (got,) = sieve.call(&mut store, (30,))?;
    if got != expected {
        return Err(format!("sieve(30) mismatch:\n got {got:?}\n want {expected:?}").into());
    }
    Ok(())
}
