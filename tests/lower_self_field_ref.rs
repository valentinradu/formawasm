//! Tests for `lower::lower_self_field_ref`.
//!
//! For Phase 1b mc7 we detect a "method" purely by shape — a function
//! whose first parameter is named `self` with a `ResolvedType::Struct(_)`
//! type. Each test builds a struct, a method that returns one of its
//! fields via `self.x`, and a non-method "make" function that
//! constructs an instance + calls the method (since direct method
//! calls land in mc8).
//!
//! This mc therefore can't run a method end-to-end yet; the tests
//! only validate the emitted module to confirm the lowering is
//! syntactically correct.

use formalang::ast::{ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{
    BindingId, FieldIdx, IrExpr, IrField, IrFunction, IrFunctionParam, IrModule, IrSpan, IrStruct,
    ResolvedType, StructId,
};
use formawasm::module_lowering;
use wasmparser::{Validator, WasmFeatures};

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
                span: IrSpan::default(),
            })
            .collect(),
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
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
        span: IrSpan::default(),
    }
}

fn self_field_ref(field: &str, idx: u32, ty: PrimitiveType) -> IrExpr {
    IrExpr::SelfFieldRef {
        field: field.to_owned(),
        field_idx: FieldIdx(idx),
        ty: primitive(ty),
        span: IrSpan::default(),
    }
}

fn method(name: &str, struct_id: StructId, return_ty: PrimitiveType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: vec![self_param(struct_id, 0)],
        return_type: Some(primitive(return_ty)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

#[test]
fn self_field_ref_loads_first_field() -> TestResult {
    // struct Pair { a: I32, b: I32 }
    // fn first(self: Pair) -> I32 { self.a }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Pair",
        vec![("a", PrimitiveType::I32), ("b", PrimitiveType::I32)],
    ));

    let body = self_field_ref("a", 0, PrimitiveType::I32);
    module
        .functions
        .push(method("first", StructId(0), PrimitiveType::I32, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}

#[test]
fn self_field_ref_loads_field_after_padding() -> TestResult {
    // struct M { flag: Boolean, count: I32, big: I64 }
    // fn read_big(self: M) -> I64 { self.big }
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "M",
        vec![
            ("flag", PrimitiveType::Boolean),
            ("count", PrimitiveType::I32),
            ("big", PrimitiveType::I64),
        ],
    ));

    let body = self_field_ref("big", 2, PrimitiveType::I64);
    module
        .functions
        .push(method("read_big", StructId(0), PrimitiveType::I64, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}

#[test]
fn self_field_ref_loads_boolean_field() -> TestResult {
    let mut module = IrModule::new();
    module.structs.push(struct_def(
        "Flagged",
        vec![("flag", PrimitiveType::Boolean), ("v", PrimitiveType::I32)],
    ));

    let body = self_field_ref("flag", 0, PrimitiveType::Boolean);
    module.functions.push(method(
        "read_flag",
        StructId(0),
        PrimitiveType::Boolean,
        body,
    ));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)
}
