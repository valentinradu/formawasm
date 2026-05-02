//! Targeted negative tests for `LowerError` variants that don't
//! have a dedicated home in the per-variant lower_*.rs files.
//!
//! Each test pins one variant by constructing the smallest IR
//! shape that triggers it. The set focuses on variants users could
//! realistically hit through malformed IR construction (rather than
//! the "internal invariant violation" variants that exist as
//! defense-in-depth).

use formalang::ast::{ParamConvention, PrimitiveType};
use formalang::ir::{
    BindingId, FieldIdx, ImplTarget, IrBlockStatement, IrExpr, IrField, IrFunction,
    IrFunctionParam, IrImpl, IrModule, IrStruct, MethodIdx, ResolvedType, StructId, TraitId,
    VariantIdx,
};
use formawasm::module_lowering::{self, ModuleLowerError};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn function_with_body(name: &str, return_ty: ResolvedType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(return_ty),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    }
}

#[test]
fn zero_sized_let_binding_is_rejected() -> TestResult {
    // A `let x: Never = ...` produces a binding with no wasm value
    // representation — `body_value_type(Never) -> None` — so the
    // lowering can't allocate a local for it. This should surface
    // as `LowerError::ZeroSizedLetBinding` rather than producing a
    // structurally invalid module.
    let never_ty = primitive(PrimitiveType::Never);
    let body = IrExpr::Block {
        statements: vec![IrBlockStatement::Let {
            name: "doomed".to_owned(),
            binding_id: BindingId(1),
            ty: Some(never_ty.clone()),
            mutable: false,
            value: IrExpr::Literal {
                value: formalang::ast::Literal::Boolean(false),
                ty: never_ty,
            },
        }],
        result: Box::new(IrExpr::Literal {
            value: formalang::ast::Literal::Number(formalang::ast::NumberLiteral::suffixed(
                formalang::ast::NumberValue::Integer(0),
                formalang::ast::NumericSuffix::I32,
            )),
            ty: primitive(PrimitiveType::I32),
        }),
        ty: primitive(PrimitiveType::I32),
    };
    let mut module = IrModule::new();
    module.functions.push(function_with_body(
        "trips",
        primitive(PrimitiveType::I32),
        body,
    ));

    match module_lowering::lower_module(&module) {
        Err(ModuleLowerError::Lower(formawasm::LowerError::ZeroSizedLetBinding { name, ty }))
            if name == "doomed" && matches!(ty, ResolvedType::Primitive(PrimitiveType::Never)) =>
        {
            Ok(())
        }
        other => Err(format!("expected ZeroSizedLetBinding, got {other:?}").into()),
    }
}

#[test]
fn external_struct_inst_is_rejected() -> TestResult {
    // `StructInst { struct_id: None }` represents a cross-module
    // struct constructor — Phase 4 / R2 territory. Until upstream
    // unblocks, it's rejected here as `ExternalStructInst` so a
    // future regression that silently emits something for it would
    // trip this test.
    let body = IrExpr::StructInst {
        struct_id: None,
        type_args: Vec::new(),
        fields: Vec::new(),
        ty: ResolvedType::External {
            module_path: vec!["other".to_owned()],
            name: "Foreign".to_owned(),
            kind: formalang::ir::ImportedKind::Struct,
            type_args: Vec::new(),
        },
    };
    let mut module = IrModule::new();
    module.functions.push(function_with_body(
        "trips",
        primitive(PrimitiveType::I32),
        body,
    ));

    match module_lowering::lower_module(&module) {
        Err(ModuleLowerError::Lower(formawasm::LowerError::ExternalStructInst)) => Ok(()),
        other => Err(format!("expected ExternalStructInst, got {other:?}").into()),
    }
}

#[test]
fn unsupported_virtual_receiver_is_rejected() -> TestResult {
    // Virtual dispatch on a primitive receiver isn't a shape the
    // language can produce, but a malformed IR fixture can express
    // it. The lowering rejects with `UnsupportedVirtualReceiver`
    // carrying the offending type — locks in the rejection path
    // for the new Phase-3 virtual-dispatch surface.
    let body = IrExpr::MethodCall {
        receiver: Box::new(IrExpr::Literal {
            value: formalang::ast::Literal::Number(formalang::ast::NumberLiteral::suffixed(
                formalang::ast::NumberValue::Integer(0),
                formalang::ast::NumericSuffix::I32,
            )),
            ty: primitive(PrimitiveType::I32),
        }),
        method: "value".to_owned(),
        method_idx: MethodIdx(0),
        args: Vec::new(),
        dispatch: formalang::ir::DispatchKind::Virtual {
            trait_id: TraitId(0),
            method_name: "value".to_owned(),
        },
        ty: primitive(PrimitiveType::I32),
    };
    // Wire up enough trait/impl context that the module-level pre-
    // walk produces a vtable plumbing entry — otherwise the lowering
    // surfaces a `MissingContext` for `method_table` first.
    let mut module = IrModule::new();
    module.traits.push(formalang::ir::IrTrait {
        name: "Greet".to_owned(),
        visibility: formalang::ast::Visibility::Private,
        composed_traits: Vec::new(),
        fields: Vec::new(),
        methods: vec![formalang::ir::IrFunctionSig {
            name: "value".to_owned(),
            params: vec![IrFunctionParam {
                binding_id: BindingId(0),
                name: "self".to_owned(),
                external_label: None,
                ty: None,
                default: None,
                convention: ParamConvention::Let,
            }],
            return_type: Some(primitive(PrimitiveType::I32)),
            attributes: Vec::new(),
        }],
        generic_params: Vec::new(),
        doc: None,
    });
    module.structs.push(IrStruct {
        name: "Receiver".to_owned(),
        visibility: formalang::ast::Visibility::Private,
        traits: vec![formalang::ir::IrTraitRef::simple(TraitId(0))],
        fields: vec![IrField {
            name: "tag".to_owned(),
            ty: primitive(PrimitiveType::I32),
            mutable: false,
            optional: false,
            default: None,
            doc: None,
            convention: ParamConvention::Let,
        }],
        generic_params: Vec::new(),
        doc: None,
    });
    module.impls.push(IrImpl {
        target: ImplTarget::Struct(StructId(0)),
        trait_ref: Some(formalang::ir::IrTraitRef::simple(TraitId(0))),
        is_extern: false,
        generic_params: Vec::new(),
        functions: vec![IrFunction {
            name: "value".to_owned(),
            generic_params: Vec::new(),
            params: vec![IrFunctionParam {
                binding_id: BindingId(0),
                name: "self".to_owned(),
                external_label: None,
                ty: Some(ResolvedType::Struct(StructId(0))),
                default: None,
                convention: ParamConvention::Let,
            }],
            return_type: Some(primitive(PrimitiveType::I32)),
            body: Some(IrExpr::Literal {
                value: formalang::ast::Literal::Number(formalang::ast::NumberLiteral::suffixed(
                    formalang::ast::NumberValue::Integer(0),
                    formalang::ast::NumericSuffix::I32,
                )),
                ty: primitive(PrimitiveType::I32),
            }),
            extern_abi: None,
            attributes: Vec::new(),
            doc: None,
        }],
    });
    module.functions.push(function_with_body(
        "trips",
        primitive(PrimitiveType::I32),
        body,
    ));

    match module_lowering::lower_module(&module) {
        Err(ModuleLowerError::Lower(formawasm::LowerError::UnsupportedVirtualReceiver {
            ty: ResolvedType::Primitive(PrimitiveType::I32),
        })) => Ok(()),
        other => Err(format!("expected UnsupportedVirtualReceiver, got {other:?}").into()),
    }
}

// Variant-name lookup is best exercised through the IrEnum path —
// gluing a Match arm to a non-existent variant. The lowering plans
// the enum, then `resolve_variant` falls through to the by-name
// scan and surfaces `UnknownVariant` with both the enum and the
// missing arm name.
#[test]
fn unknown_variant_in_match_arm_is_rejected() -> TestResult {
    use formalang::ir::{IrEnum, IrEnumVariant, IrMatchArm};

    let scrutinee = IrExpr::EnumInst {
        enum_id: Some(formalang::ir::EnumId(0)),
        variant: "A".to_owned(),
        variant_idx: VariantIdx(0),
        fields: Vec::new(),
        ty: ResolvedType::Enum(formalang::ir::EnumId(0)),
    };
    let body = IrExpr::Match {
        scrutinee: Box::new(scrutinee),
        arms: vec![IrMatchArm {
            variant: "Phantom".to_owned(),
            variant_idx: VariantIdx(99),
            bindings: Vec::new(),
            is_wildcard: false,
            body: IrExpr::Literal {
                value: formalang::ast::Literal::Number(formalang::ast::NumberLiteral::suffixed(
                    formalang::ast::NumberValue::Integer(0),
                    formalang::ast::NumericSuffix::I32,
                )),
                ty: primitive(PrimitiveType::I32),
            },
        }],
        ty: primitive(PrimitiveType::I32),
    };

    let mut module = IrModule::new();
    module.enums.push(IrEnum {
        name: "Real".to_owned(),
        visibility: formalang::ast::Visibility::Private,
        variants: vec![IrEnumVariant {
            name: "A".to_owned(),
            fields: Vec::new(),
        }],
        generic_params: Vec::new(),
        doc: None,
    });
    module.functions.push(function_with_body(
        "trips",
        primitive(PrimitiveType::I32),
        body,
    ));

    match module_lowering::lower_module(&module) {
        Err(ModuleLowerError::Lower(formawasm::LowerError::UnknownVariant { variant, .. }))
            if variant == "Phantom" =>
        {
            Ok(())
        }
        other => Err(format!("expected UnknownVariant, got {other:?}").into()),
    }
}

#[test]
fn field_index_out_of_range_is_rejected_for_unknown_field_name() -> TestResult {
    // A `FieldAccess` with an out-of-range `FieldIdx` and a name
    // that doesn't match any of the struct's actual fields rolls
    // through to `FieldIndexOutOfRange` (the planner falls back to
    // a name lookup, then surfaces this when neither matches).
    use formalang::ir::ReferenceTarget;

    let mut module = IrModule::new();
    module.structs.push(IrStruct {
        name: "OneField".to_owned(),
        visibility: formalang::ast::Visibility::Private,
        traits: Vec::new(),
        fields: vec![IrField {
            name: "x".to_owned(),
            ty: primitive(PrimitiveType::I32),
            mutable: false,
            optional: false,
            default: None,
            doc: None,
            convention: ParamConvention::Let,
        }],
        generic_params: Vec::new(),
        doc: None,
    });
    let s_ty = ResolvedType::Struct(StructId(0));
    let body = IrExpr::FieldAccess {
        object: Box::new(IrExpr::Reference {
            path: vec!["s".to_owned()],
            target: ReferenceTarget::Param(BindingId(0)),
            ty: s_ty.clone(),
        }),
        field: "y".to_owned(),
        field_idx: FieldIdx(99),
        ty: primitive(PrimitiveType::I32),
    };
    module.functions.push(IrFunction {
        name: "trips".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "s".to_owned(),
            external_label: None,
            ty: Some(s_ty),
            default: None,
            convention: ParamConvention::Let,
        }],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    });

    match module_lowering::lower_module(&module) {
        Err(ModuleLowerError::Lower(formawasm::LowerError::FieldIndexOutOfRange {
            field_count: 1,
            ..
        })) => Ok(()),
        other => Err(format!("expected FieldIndexOutOfRange, got {other:?}").into()),
    }
}
