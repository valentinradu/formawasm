//! Tests for `wit::emit_wit`.
//!
//! Cover the empty-surface case, a fibonacci-shaped function, the
//! non-primitive rejection path, and a round-trip back through
//! `wit_parser::Resolve` to confirm the emitted text is real WIT.

mod common;
use common::{seed_prelude, optional_ty, array_ty, range_ty, dict_ty};

use formalang::ast::{ExternAbi, ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{
    BindingId, FunctionId, IrEnum, IrEnumVariant, IrField, IrFunction, IrFunctionParam, IrModule,
    IrSpan, IrStruct, ResolvedType, StructId,
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
                span: IrSpan::default(),
            })
            .collect(),
        return_type: return_ty,
        body: None,
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
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
    seed_prelude(&mut module);
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
    seed_prelude(&mut module);
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
    seed_prelude(&mut module);
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
    seed_prelude(&mut module);
    module.functions.push(function("noop", vec![], None));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("export noop: func();") {
        return Err(format!("expected no `->` clause for unit return:\n{wit}").into());
    }
    Ok(())
}


#[test]
fn never_typed_parameter_is_rejected() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
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
fn export_index_out_of_range_is_rejected() -> TestResult {
    let module = IrModule::new();
    let mut surface = PublicSurface::default();
    surface.exports.push(FunctionId(7));

    match wit::emit_wit(&module, &surface) {
        Err(WitEmitError::ExportOutOfRange { index: 7, len: 0 }) => Ok(()),
        other => Err(format!("expected ExportOutOfRange, got {other:?}").into()),
    }
}

fn primitive_field(name: &str, p: PrimitiveType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty: primitive(p),
        mutable: false,
        optional: false,
        default: None,
        doc: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

#[test]
fn public_struct_emits_record_with_kebab_case_fields() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.structs.push(IrStruct {
        name: "Pair".to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields: vec![
            primitive_field("a", PrimitiveType::I32),
            primitive_field("b", PrimitiveType::I64),
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("record pair {") {
        return Err(format!("missing record pair declaration:\n{wit}").into());
    }
    if !wit.contains("a: s32") {
        return Err(format!("missing field a:\n{wit}").into());
    }
    if !wit.contains("b: s64") {
        return Err(format!("missing field b:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn public_enum_emits_variant_with_unit_and_payload_arms() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.enums.push(IrEnum {
        name: "Maybe".to_owned(),
        visibility: Visibility::Public,
        variants: vec![
            IrEnumVariant {
                name: "None".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Some".to_owned(),
                fields: vec![primitive_field("v", PrimitiveType::I32)],
                span: IrSpan::default(),
            },
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("variant maybe {") {
        return Err(format!("missing variant maybe declaration:\n{wit}").into());
    }
    if !wit.contains("none,") {
        return Err(format!("missing unit arm `none`:\n{wit}").into());
    }
    if !wit.contains("some(s32),") {
        return Err(format!("missing payload arm `some(s32)`:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn multi_field_variant_payload_emits_tuple_arm() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.enums.push(IrEnum {
        name: "Pair".to_owned(),
        visibility: Visibility::Public,
        variants: vec![IrEnumVariant {
            name: "Both".to_owned(),
            fields: vec![
                primitive_field("a", PrimitiveType::I32),
                primitive_field("b", PrimitiveType::I32),
            ],
            span: IrSpan::default(),
        }],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("variant pair {") {
        return Err(format!("missing variant pair declaration:\n{wit}").into());
    }
    if !wit.contains("both(tuple<s32, s32>),") {
        return Err(format!("missing tuple-payload arm `both(tuple<s32, s32>)`:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn mixed_type_variant_payload_emits_typed_tuple() -> TestResult {
    // Three-field arm with a mix of primitive widths exercises the
    // tuple-element emitter through `resolved_wit_type` for each
    // field independently — no per-field-type fast path can hide a
    // bug.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.enums.push(IrEnum {
        name: "Triple".to_owned(),
        visibility: Visibility::Public,
        variants: vec![IrEnumVariant {
            name: "Mix".to_owned(),
            fields: vec![
                primitive_field("a", PrimitiveType::I32),
                primitive_field("b", PrimitiveType::I64),
                primitive_field("c", PrimitiveType::Boolean),
            ],
            span: IrSpan::default(),
        }],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("mix(tuple<s32, s64, bool>),") {
        return Err(format!("missing tuple arm `mix(tuple<s32, s64, bool>)`:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn multi_field_variant_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.enums.push(IrEnum {
        name: "Pair".to_owned(),
        visibility: Visibility::Public,
        variants: vec![
            IrEnumVariant {
                name: "Single".to_owned(),
                fields: vec![primitive_field("v", PrimitiveType::I32)],
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Both".to_owned(),
                fields: vec![
                    primitive_field("a", PrimitiveType::I32),
                    primitive_field("b", PrimitiveType::I32),
                ],
                span: IrSpan::default(),
            },
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    let _world = resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn record_and_variant_round_trip_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.structs.push(IrStruct {
        name: "Point".to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields: vec![
            primitive_field("x", PrimitiveType::I32),
            primitive_field("y", PrimitiveType::I32),
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });
    module.enums.push(IrEnum {
        name: "Color".to_owned(),
        visibility: Visibility::Public,
        variants: vec![
            IrEnumVariant {
                name: "Red".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Green".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Blue".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn dictionary_emits_list_of_tuple_pairs() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "table",
        vec![],
        Some(dict_ty(
            primitive(PrimitiveType::String),
            primitive(PrimitiveType::I32),
        )),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export table: func() -> list<tuple<string, s32>>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn dictionary_signature_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "lookup-table",
        vec![(
            BindingId(0),
            "entries",
            dict_ty(
                primitive(PrimitiveType::String),
                primitive(PrimitiveType::I32),
            ),
        )],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn string_parameter_emits_string_wit_type() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "echo",
        vec![(BindingId(0), "s", primitive(PrimitiveType::String))],
        Some(primitive(PrimitiveType::String)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export echo: func(s: string) -> string;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn path_and_regex_map_to_string_at_the_boundary() -> TestResult {
    // Path / Regex are distinct primitive types inside the component
    // but lower to the same canonical-ABI `string` shape at the WIT
    // boundary — there's no native WIT type for either, and tooling
    // that consumes the generated component sees them as plain
    // strings.
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "use-path",
        vec![(BindingId(0), "p", primitive(PrimitiveType::Path))],
        Some(primitive(PrimitiveType::String)),
    ));
    module.functions.push(function(
        "use-regex",
        vec![(BindingId(0), "r", primitive(PrimitiveType::Regex))],
        Some(primitive(PrimitiveType::Boolean)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    for needle in [
        "export use-path: func(p: string) -> string;",
        "export use-regex: func(r: string) -> bool;",
    ] {
        if !wit.contains(needle) {
            return Err(format!("missing `{needle}` in:\n{wit}").into());
        }
    }
    Ok(())
}

#[test]
fn list_of_string_emits_list_string() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "lines",
        vec![],
        Some(array_ty(primitive(PrimitiveType::String))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export lines: func() -> list<string>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn option_of_string_emits_option_string() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "find",
        vec![(BindingId(0), "needle", primitive(PrimitiveType::String))],
        Some(optional_ty(primitive(PrimitiveType::String))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export find: func(needle: string) -> option<string>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn string_signature_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "greet",
        vec![(BindingId(0), "name", primitive(PrimitiveType::String))],
        Some(primitive(PrimitiveType::String)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn array_parameter_emits_list_of_element_wit_type() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "sum",
        vec![(BindingId(0), "xs", array_ty(primitive(PrimitiveType::I32)))],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export sum: func(xs: list<s32>) -> s32;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn array_return_type_emits_list_in_result_clause() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "ones",
        vec![(BindingId(0), "n", primitive(PrimitiveType::I32))],
        Some(array_ty(primitive(PrimitiveType::I32))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export ones: func(n: s32) -> list<s32>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn array_of_bool_and_i64_use_correct_wit_element_names() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "checks",
        vec![],
        Some(array_ty(primitive(PrimitiveType::Boolean))),
    ));
    module.functions.push(function(
        "bigs",
        vec![],
        Some(array_ty(primitive(PrimitiveType::I64))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    for needle in [
        "export checks: func() -> list<bool>;",
        "export bigs: func() -> list<s64>;",
    ] {
        if !wit.contains(needle) {
            return Err(format!("missing `{needle}` in:\n{wit}").into());
        }
    }
    Ok(())
}

#[test]
fn list_signature_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "primes",
        vec![(BindingId(0), "limit", primitive(PrimitiveType::I32))],
        Some(array_ty(primitive(PrimitiveType::I32))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn nested_list_of_list_emits_nested_list_type() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "matrix",
        vec![],
        Some(array_ty(array_ty(primitive(PrimitiveType::I32)))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export matrix: func() -> list<list<s32>>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn optional_parameter_emits_option_of_inner_wit_type() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "set",
        vec![(
            BindingId(0),
            "v",
            optional_ty(primitive(PrimitiveType::I32)),
        )],
        Some(primitive(PrimitiveType::Boolean)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export set: func(v: option<s32>) -> bool;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn optional_return_type_emits_option_in_result_clause() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "lookup",
        vec![(BindingId(0), "key", primitive(PrimitiveType::I64))],
        Some(optional_ty(primitive(PrimitiveType::I64))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export lookup: func(key: s64) -> option<s64>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}

#[test]
fn optional_signature_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "find",
        vec![(BindingId(0), "needle", primitive(PrimitiveType::I32))],
        Some(optional_ty(primitive(PrimitiveType::I32))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn list_of_optional_emits_nested_option_in_list() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(function(
        "rows",
        vec![],
        Some(array_ty(optional_ty(primitive(PrimitiveType::I32)))),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "export rows: func() -> list<option<s32>>;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    Ok(())
}




#[test]
fn record_with_list_field_round_trips() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.structs.push(IrStruct {
        name: "Stats".to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields: vec![
            primitive_field("count", PrimitiveType::I32),
            IrField {
                name: "values".to_owned(),
                ty: array_ty(primitive(PrimitiveType::I32)),
                mutable: false,
                optional: false,
                default: None,
                doc: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            },
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    });

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    if !wit.contains("values: list<s32>") {
        return Err(format!("missing list-typed field in record:\n{wit}").into());
    }
    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    Ok(())
}

#[test]
fn emitted_wit_round_trips_through_wit_parser() -> TestResult {
    let mut module = IrModule::new();
    seed_prelude(&mut module);
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

#[test]
fn extern_function_emits_import_with_signature() -> TestResult {
    let mut f = function(
        "host-add",
        vec![
            (BindingId(0), "a", primitive(PrimitiveType::I32)),
            (BindingId(1), "b", primitive(PrimitiveType::I32)),
        ],
        Some(primitive(PrimitiveType::I32)),
    );
    f.extern_abi = Some(ExternAbi::C);

    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(f);

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let expected = "import host-add: func(a: s32, b: s32) -> s32;";
    if !wit.contains(expected) {
        return Err(format!("missing `{expected}` in:\n{wit}").into());
    }
    if wit.contains("export host-add") {
        return Err(format!("extern fn must not be exported: {wit}").into());
    }
    Ok(())
}

#[test]
fn import_signature_round_trips_through_wit_parser() -> TestResult {
    let mut host = function(
        "host-add",
        vec![
            (BindingId(0), "a", primitive(PrimitiveType::I32)),
            (BindingId(1), "b", primitive(PrimitiveType::I32)),
        ],
        Some(primitive(PrimitiveType::I32)),
    );
    host.extern_abi = Some(ExternAbi::C);

    let mut module = IrModule::new();
    seed_prelude(&mut module);
    module.functions.push(host);
    module.functions.push(function(
        "use-host",
        vec![],
        Some(primitive(PrimitiveType::I32)),
    ));

    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;

    let mut resolve = Resolve::default();
    let pkg = resolve.push_str("formawasm-test.wit", &wit)?;
    let world_id = resolve.select_world(&[pkg], Some(WORLD_NAME))?;
    let world = resolve.worlds.get(world_id).ok_or("missing world id")?;
    if world.imports.len() != 1 {
        return Err(format!("expected exactly 1 import, got {}", world.imports.len()).into());
    }
    if world.exports.len() != 1 {
        return Err(format!("expected exactly 1 export, got {}", world.exports.len()).into());
    }
    Ok(())
}

