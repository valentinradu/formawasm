//! Size benchmark for the optional `wasm-opt` post-pass.
//!
//! Builds a representative module, emits the unoptimized core wasm
//! bytes via `module_lowering::lower_module`, runs them through
//! `optimize_core_module`, and asserts:
//!
//! 1. The optimized bytes still validate.
//! 2. The optimized bytes are strictly smaller than the
//!    unoptimized ones (binaryen's `-Os` profile should always win
//!    on a non-trivial module — if a regression flips this, the
//!    test fails loudly so we know our emitted wasm has grown in a
//!    shape binaryen can't fold).
//!
//! Only compiles with the `wasm-opt` feature enabled.

#![cfg(feature = "wasm-opt")]

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, FunctionId, IrExpr, IrFunction, IrFunctionParam, IrModule, IrSpan, ReferenceTarget,
    ResolvedType,
};
use formawasm::module_lowering;
use formawasm::optimize_core_module;
use wasmparser::{Validator, WasmFeatures};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
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

fn validate(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

fn param_ref(id: u32) -> IrExpr {
    IrExpr::Reference {
        path: vec![format!("p{id}")],
        target: ReferenceTarget::Param(BindingId(id)),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    }
}

fn binary_op(op: BinaryOperator, left: IrExpr, right: IrExpr, ty: PrimitiveType) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(left),
        right: Box::new(right),
        op,
        ty: primitive(ty),
        span: IrSpan::default(),
    }
}

fn function(name: &str, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "n".to_owned(),
            external_label: None,
            ty: Some(primitive(PrimitiveType::I32)),
            default: None,
            convention: ParamConvention::Let,
            span: IrSpan::default(),
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

/// Build a small recursive function module, exercising arithmetic,
/// branches, and self-recursive calls — enough surface that
/// binaryen's `-Os` profile finds folding opportunities. Synthetic
/// in shape but representative of what real consumers emit.
fn build_representative_module() -> IrModule {
    // fn fib(n) -> if n < 2 { n } else { fib(n-1) + fib(n-2) }
    let n_lt_2 = binary_op(
        BinaryOperator::Lt,
        param_ref(0),
        integer_literal(2),
        PrimitiveType::Boolean,
    );
    let n_minus_1 = binary_op(
        BinaryOperator::Sub,
        param_ref(0),
        integer_literal(1),
        PrimitiveType::I32,
    );
    let n_minus_2 = binary_op(
        BinaryOperator::Sub,
        param_ref(0),
        integer_literal(2),
        PrimitiveType::I32,
    );
    let recursive_call = |arg: IrExpr| IrExpr::FunctionCall {
        path: vec!["fib".to_owned()],
        function_id: Some(FunctionId(0)),
        args: vec![(None, arg)],
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    let fib_body = IrExpr::If {
        condition: Box::new(n_lt_2),
        then_branch: Box::new(param_ref(0)),
        else_branch: Some(Box::new(binary_op(
            BinaryOperator::Add,
            recursive_call(n_minus_1),
            recursive_call(n_minus_2),
            PrimitiveType::I32,
        ))),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };

    let mut module = IrModule::new();
    module.functions.push(function("fib", fib_body));
    module
}

#[test]
fn wasm_opt_post_pass_shrinks_a_representative_module() -> TestResult {
    let module = build_representative_module();
    let unoptimized = module_lowering::lower_module(&module)?;
    validate(&unoptimized)?;

    let optimized = optimize_core_module(&unoptimized)?;
    validate(&optimized)?;

    if optimized.len() >= unoptimized.len() {
        return Err(format!(
            "wasm-opt did not shrink the module: unoptimized = {} bytes, \
             optimized = {} bytes (delta = {})",
            unoptimized.len(),
            optimized.len(),
            i64::try_from(optimized.len()).unwrap_or(i64::MAX)
                - i64::try_from(unoptimized.len()).unwrap_or(i64::MAX)
        )
        .into());
    }
    Ok(())
}
