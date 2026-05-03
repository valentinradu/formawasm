//! Snapshot-style coverage for `wit::emit_wit`.
//!
//! `tests/wit.rs` checks individual lines via `wit.contains(...)`,
//! which catches the line being present but misses ordering /
//! whitespace / sibling-line drift. The tests here capture the full
//! emitted text for representative IR shapes; any change shows up
//! as a snapshot diff that has to be explicitly reviewed (via
//! `cargo insta review`).
//!
//! Snapshot files live in `tests/snapshots/`. New snapshots are
//! auto-accepted on first run; subsequent runs compare against the
//! committed file and fail loudly on divergence.

use formalang::ast::{ExternAbi, ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{
    BindingId, IrEnum, IrEnumVariant, IrField, IrFunction, IrFunctionParam, IrModule, IrSpan,
    IrStruct, ResolvedType,
};
use formawasm::survey;
use formawasm::wit;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn function(
    name: &str,
    params: Vec<(BindingId, &str, ResolvedType)>,
    return_ty: ResolvedType,
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
        body: None,
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
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
fn snapshot_empty_module() -> TestResult {
    let module = IrModule::new();
    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;
    insta::assert_snapshot!("empty_module", wit);
    Ok(())
}

#[test]
fn snapshot_fibonacci_function() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function(
        "fib",
        vec![(BindingId(0), "n", primitive(PrimitiveType::I32))],
        primitive(PrimitiveType::I32),
    ));
    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;
    insta::assert_snapshot!("fibonacci_function", wit);
    Ok(())
}

#[test]
fn snapshot_record_with_two_fields() -> TestResult {
    let mut module = IrModule::new();
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
    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;
    insta::assert_snapshot!("record_with_two_fields", wit);
    Ok(())
}

#[test]
fn snapshot_variant_with_unit_payload_and_tuple_arms() -> TestResult {
    let mut module = IrModule::new();
    module.enums.push(IrEnum {
        name: "Action".to_owned(),
        visibility: Visibility::Public,
        variants: vec![
            IrEnumVariant {
                name: "Reset".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Add".to_owned(),
                fields: vec![primitive_field("amount", PrimitiveType::I32)],
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Replace".to_owned(),
                fields: vec![
                    primitive_field("old", PrimitiveType::I32),
                    primitive_field("new", PrimitiveType::I32),
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
    insta::assert_snapshot!("variant_with_tuple_arms", wit);
    Ok(())
}

#[test]
fn snapshot_extern_import_alongside_export() -> TestResult {
    let mut host = function(
        "host-add",
        vec![
            (BindingId(0), "a", primitive(PrimitiveType::I32)),
            (BindingId(1), "b", primitive(PrimitiveType::I32)),
        ],
        primitive(PrimitiveType::I32),
    );
    host.extern_abi = Some(ExternAbi::C);
    let mut module = IrModule::new();
    module.functions.push(host);
    module
        .functions
        .push(function("use-host", vec![], primitive(PrimitiveType::I32)));
    let surface = survey::survey(&module);
    let wit = wit::emit_wit(&module, &surface)?;
    insta::assert_snapshot!("extern_import_alongside_export", wit);
    Ok(())
}
