//! Tests for `wit::emit_wit`.
//!
//! Cover the empty-surface case, a fibonacci-shaped function, the
//! non-primitive rejection path, and a round-trip back through
//! `wit_parser::Resolve` to confirm the emitted text is real WIT.

use formalang::ast::{ParamConvention, PrimitiveType};
use formalang::ir::{
    BindingId, FunctionId, IrFunction, IrFunctionParam, IrModule, ResolvedType, StructId,
};
use formawasm::survey::{self, PublicSurface};
use formawasm::types::TypeMapError;
use formawasm::wit::{self, PACKAGE, WORLD_NAME, WitEmitError};
use wit_parser::Resolve;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn function(
    name: &str,
    params: Vec<(BindingId, &str, ResolvedType)>,
    return_ty: Option<ResolvedType>,
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
            })
            .collect(),
        return_type: return_ty,
        body: None,
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    }
}

#[test]
fn empty_module_emits_world_with_no_exports() -> TestResult {
    let module = IrModule::new();
    let surface = PublicSurface::default();
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains(&format!("package {PACKAGE};")) {
        return Err(format!("missing package line: {wit}").into());
    }
    if !wit.contains(&format!("world {WORLD_NAME} {{")) {
        return Err(format!("missing world line: {wit}").into());
    }
    if wit.contains("export ") {
        return Err(format!("expected no exports: {wit}").into());
    }
    Ok(())
}

#[test]
fn fibonacci_function_emits_export_with_s32_signature() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "fib",
        vec![(BindingId(0), "n", primitive(PrimitiveType::I32))],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export fib: func(n: s32) -> s32;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn all_primitive_value_types_round_trip() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "use-i64",
        vec![(BindingId(0), "n", primitive(PrimitiveType::I64))],
        Some(primitive(PrimitiveType::I64)),
    ));
    module.functions.push(function(
        "use-floats",
        vec![
            (BindingId(0), "a", primitive(PrimitiveType::F32)),
            (BindingId(1), "b", primitive(PrimitiveType::F64)),
        ],
        Some(primitive(PrimitiveType::F64)),
    ));
    module.functions.push(function(
        "is-zero",
        vec![(BindingId(0), "n", primitive(PrimitiveType::I32))],
        Some(primitive(PrimitiveType::Boolean)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    for needle in [
        "export use-i64: func(n: s64) -> s64;",
        "export use-floats: func(a: f32, b: f64) -> f64;",
        "export is-zero: func(n: s32) -> bool;",
    ] {
        if !wit.contains(needle) {
            return Err(format!("missing `{needle}` in:\n{wit}").into());
        }
    }
    Ok(())
}

#[test]
fn never_return_type_omits_result_clause() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "diverges",
        vec![],
        Some(primitive(PrimitiveType::Never)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("export diverges: func();") {
        return Err(format!("expected no `->` clause for Never return:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn unit_return_type_omits_result_clause() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function("noop", vec![], None));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("export noop: func();") {
        return Err(format!("expected no `->` clause for unit return:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn struct_typed_parameter_is_rejected() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "takes-struct",
        vec![(BindingId(0), "p", ResolvedType::Struct(StructId(0)))],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    match wit::emit_wit(&module, &surface) {
        Err(WitEmitError::TypeMap(TypeMapError::NotYetSupported { kind })) if kind == "Struct" => {
            Ok(())
        }
        other => Err(format!("expected NotYetSupported(Struct), got {other:?}").into()),
    }
}

#[test]
fn never_typed_parameter_is_rejected() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "takes-never",
        vec![(BindingId(0), "p", primitive(PrimitiveType::Never))],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    match wit::emit_wit(&module, &surface) {
        Err(WitEmitError::NeverParam { function, param })
            if function == "takes-never" && param == "p" =>
        {
            Ok(())
        }
        other => Err(format!("expected NeverParam, got {other:?}").into()),
    }
}

#[test]
fn missing_param_type_is_rejected() -> TestResult {
    let mut f = function(
        "missing",
        vec![(BindingId(0), "p", primitive(PrimitiveType::I32))],
        Some(primitive(PrimitiveType::I32)),
    );
    let p = f.params.first_mut().ok_or("expected one param")?;
    p.ty = None;

    let mut module = IrModule::new();
    module.functions.push(f);

    let surface = survey::survey(&module);
    match wit::emit_wit(&module, &surface) {
        Err(WitEmitError::MissingParamType { function, param })
            if function == "missing" && param == "p" =>
        {
            Ok(())
        }
        other => Err(format!("expected MissingParamType, got {other:?}").into()),
    }
}

#[test]
fn export_index_out_of_range_is_rejected() -> TestResult {
    let module = IrModule::new();
    let mut surface = PublicSurface::default();
    surface.exports.push(FunctionId(7));

    match wit::emit_wit(&module, &surface) {
        Err(WitEmitError::ExportOutOfRange { index: 7, len: 0 }) => Ok(()),
        other => Err(format!("expected ExportOutOfRange, got {other:?}").into()),
    }
}

#[test]
fn emitted_wit_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "fib",
        vec![(BindingId(0), "n", primitive(PrimitiveType::I32))],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    // Re-parse independently of the in-emit round-trip so we catch any
    // future regression where `emit_wit` skips its own validation.
    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    let world = resolve.select_world(&[pkg], Some(WORLD_NAME))?;

    let world = resolve.worlds.get(world).ok_or("missing world id")?;
    if world.exports.len() != 1 {
        return Err(format!("expected exactly 1 export, got {}", world.exports.len()).into());
    }
    Ok(())
}
