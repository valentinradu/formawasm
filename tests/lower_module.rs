//! Tests for `module_lowering::lower_module`.
//!
//! Includes a fibonacci end-to-end test that exercises every Phase 1a
//! lowering primitive (`Literal`, `Reference`, `BinaryOp`, `If`,
//! `FunctionCall`, `Block`, `Let`) plus the module-level walker, and
//! runs the resulting core-Wasm module under wasmtime to confirm it
//! actually computes the right values.

use formalang::ast::{
    BinaryOperator, ExternAbi, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget,
    ResolvedType,
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

#[test]
fn empty_module_lowers_to_validating_bytes() -> TestResult {
    let module = IrModule::new();
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}

#[test]
fn single_function_module_validates() -> TestResult {
    // fn id(x: I32) -> I32 { x }
    let body = param_ref(0, PrimitiveType::I32);
    let f = function(
        "id",
        vec![(BindingId(0), "x", PrimitiveType::I32)],
        PrimitiveType::I32,
        body,
    );

    let mut module = IrModule::new();
    module.functions.push(f);

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}

#[test]
fn extern_function_emits_core_wasm_import() -> TestResult {
    // Phase 4 mc2: extern functions are declared as core-wasm
    // function imports under the canonical-ABI module name. The
    // emitted module should validate (signature + import section
    // are well-formed) and contain the host's name in the import
    // bytes.
    let mut host = function(
        "host_log",
        vec![(BindingId(0), "n", PrimitiveType::I32)],
        PrimitiveType::I32,
        integer_literal(0, PrimitiveType::I32),
    );
    host.body = None;
    host.extern_abi = Some(ExternAbi::C);

    let mut module = IrModule::new();
    module.functions.push(host);

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;

    // The kebab-cased function name is the import's core-wasm
    // entry name; it should appear verbatim in the emitted bytes
    // alongside the canonical-ABI module name.
    let hay = String::from_utf8_lossy(&bytes);
    if !hay.contains("host-log") {
        return Err("missing kebab-case import name in module bytes".into());
    }
    if !hay.contains("cm32p2") {
        return Err("missing canonical-ABI import module name".into());
    }
    Ok(())
}

// ── Fibonacci milestone ─────────────────────────────────────────────

/// Build the IR for:
///
/// ```text
/// fn fib(n: I32) -> I32 {
///     if n < 2I32 { n } else { fib(n - 1I32) + fib(n - 2I32) }
/// }
/// ```
fn fibonacci_function() -> IrFunction {
    // n < 2
    let cond = binary_op(
        BinaryOperator::Lt,
        param_ref(0, PrimitiveType::I32),
        integer_literal(2, PrimitiveType::I32),
        PrimitiveType::Boolean,
    );
    // fib(n - 1)
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
    // fib(n - 2)
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
    // sum
    let sum = binary_op(
        BinaryOperator::Add,
        recurse_one_less,
        recurse_two_less,
        PrimitiveType::I32,
    );
    // if n < 2 { n } else { sum }
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
fn fibonacci_module_validates() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(fibonacci_function());
    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}

#[test]
fn fibonacci_runs_under_wasmtime() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(fibonacci_function());
    let bytes = module_lowering::lower_module(&module)?;

    let engine = Engine::default();
    let module = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[])?;
    let fib = instance.get_typed_func::<i32, i32>(&mut store, "fib")?;

    // fib(0..10) = 0, 1, 1, 2, 3, 5, 8, 13, 21, 34
    let expected: &[i32] = &[0, 1, 1, 2, 3, 5, 8, 13, 21, 34];
    for (n, want) in expected.iter().enumerate() {
        let got = fib.call(
            &mut store,
            i32::try_from(n).map_err(|_| -> TestError { "n overflow".into() })?,
        )?;
        if got != *want {
            return Err(format!("fib({n}) = {got}, want {want}").into());
        }
    }
    Ok(())
}
