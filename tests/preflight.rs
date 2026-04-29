//! Pre-flight rejection tests. Each test constructs a minimal
//! `IrModule` exhibiting one of the five rejected shapes and asserts
//! [`PreflightError`] surfaces the matching variant.

use formalang::ast::{Literal, ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{
    BindingId, IrEnum, IrEnumVariant, IrExpr, IrField, IrFunction, IrFunctionParam, IrFunctionSig,
    IrLet, IrModule, IrStruct, IrTrait, ResolvedType,
};
use formawasm::preflight::{self, PreflightError};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

// ───────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────

const fn bool_ty() -> ResolvedType {
    ResolvedType::Primitive(PrimitiveType::Boolean)
}

const fn bool_literal() -> IrExpr {
    IrExpr::Literal {
        value: Literal::Boolean(true),
        ty: bool_ty(),
    }
}

fn closure_expr() -> IrExpr {
    IrExpr::Closure {
        params: Vec::new(),
        captures: Vec::new(),
        body: Box::new(bool_literal()),
        ty: ResolvedType::Closure {
            param_tys: Vec::new(),
            return_ty: Box::new(bool_ty()),
        },
    }
}

fn empty_function(name: &str, body: Option<IrExpr>) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(bool_ty()),
        body,
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    }
}

fn field(name: &str, ty: ResolvedType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty,
        mutable: false,
        optional: false,
        default: None,
        doc: None,
        convention: ParamConvention::Let,
    }
}

// ───────────────────────────────────────────────────────────────────
// 1. Unconverted Closure
// ───────────────────────────────────────────────────────────────────

#[test]
fn rejects_closure_in_function_body() -> TestResult {
    let mut module = IrModule::new();
    module
        .functions
        .push(empty_function("leaks_closure", Some(closure_expr())));

    match preflight::check(&module) {
        Err(PreflightError::UnconvertedClosure { location }) => {
            if !location.contains("leaks_closure") {
                return Err(format!("location should mention the function: {location}").into());
            }
            Ok(())
        }
        other => Err(format!("expected UnconvertedClosure, got: {other:?}").into()),
    }
}

#[test]
fn rejects_closure_in_let_value() -> TestResult {
    let mut module = IrModule::new();
    module.lets.push(IrLet {
        name: "bad".to_owned(),
        visibility: Visibility::Private,
        mutable: false,
        ty: bool_ty(),
        value: closure_expr(),
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::UnconvertedClosure { location }) => {
            if !location.contains("bad") {
                return Err(format!("location should mention the let: {location}").into());
            }
            Ok(())
        }
        other => Err(format!("expected UnconvertedClosure, got: {other:?}").into()),
    }
}

// ───────────────────────────────────────────────────────────────────
// 2. Public closure-typed struct field
// ───────────────────────────────────────────────────────────────────

#[test]
fn rejects_closure_field_on_public_struct() -> TestResult {
    let mut module = IrModule::new();
    let closure_ty = ResolvedType::Closure {
        param_tys: Vec::new(),
        return_ty: Box::new(bool_ty()),
    };
    module.structs.push(IrStruct {
        name: "Widget".to_owned(),
        visibility: Visibility::Public,
        traits: Vec::new(),
        fields: vec![field("on_click", closure_ty)],
        generic_params: Vec::new(),
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::PublicClosureField { struct_name, field }) => {
            if struct_name != "Widget" {
                return Err(format!("wrong struct name: {struct_name}").into());
            }
            if field != "on_click" {
                return Err(format!("wrong field name: {field}").into());
            }
            Ok(())
        }
        other => Err(format!("expected PublicClosureField, got: {other:?}").into()),
    }
}

#[test]
fn allows_closure_field_on_private_struct() -> TestResult {
    let mut module = IrModule::new();
    let closure_ty = ResolvedType::Closure {
        param_tys: Vec::new(),
        return_ty: Box::new(bool_ty()),
    };
    module.structs.push(IrStruct {
        name: "InternalCallback".to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: vec![field("invoke", closure_ty)],
        generic_params: Vec::new(),
        doc: None,
    });

    preflight::check(&module).map_err(|e| -> TestError {
        format!("private struct with closure field should pass: {e:?}").into()
    })
}

// ───────────────────────────────────────────────────────────────────
// 3. Generic trait
// ───────────────────────────────────────────────────────────────────

#[test]
fn rejects_generic_trait() -> TestResult {
    let mut module = IrModule::new();
    module.traits.push(IrTrait {
        name: "Container".to_owned(),
        visibility: Visibility::Public,
        composed_traits: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        generic_params: vec![formalang::ir::IrGenericParam {
            name: "T".to_owned(),
            constraints: Vec::new(),
        }],
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::GenericTrait {
            trait_name,
            param_count,
        }) => {
            if trait_name != "Container" {
                return Err(format!("wrong trait name: {trait_name}").into());
            }
            if param_count != 1 {
                return Err(format!("wrong param count: {param_count}").into());
            }
            Ok(())
        }
        other => Err(format!("expected GenericTrait, got: {other:?}").into()),
    }
}

#[test]
fn allows_non_generic_trait() -> TestResult {
    let mut module = IrModule::new();
    module.traits.push(IrTrait {
        name: "Drawable".to_owned(),
        visibility: Visibility::Public,
        composed_traits: Vec::new(),
        fields: Vec::new(),
        methods: vec![IrFunctionSig {
            name: "draw".to_owned(),
            params: Vec::new(),
            return_type: Some(bool_ty()),
            attributes: Vec::new(),
        }],
        generic_params: Vec::new(),
        doc: None,
    });

    preflight::check(&module)
        .map_err(|e| -> TestError { format!("non-generic trait should pass: {e:?}").into() })
}

// ───────────────────────────────────────────────────────────────────
// 4. Unresolved TypeParam
// ───────────────────────────────────────────────────────────────────

#[test]
fn rejects_type_param_in_function_param() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(IrFunction {
        name: "identity".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: BindingId(0),
            name: "value".to_owned(),
            external_label: None,
            ty: Some(ResolvedType::TypeParam("T".to_owned())),
            default: None,
            convention: ParamConvention::Let,
        }],
        return_type: Some(bool_ty()),
        body: None,
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::UnresolvedTypeParam { name, location }) => {
            if name != "T" {
                return Err(format!("wrong param name: {name}").into());
            }
            if !location.contains("identity") {
                return Err(format!("location should mention function: {location}").into());
            }
            Ok(())
        }
        other => Err(format!("expected UnresolvedTypeParam, got: {other:?}").into()),
    }
}

#[test]
fn rejects_type_param_nested_in_array() -> TestResult {
    let mut module = IrModule::new();
    module.lets.push(IrLet {
        name: "items".to_owned(),
        visibility: Visibility::Private,
        mutable: false,
        ty: ResolvedType::Array(Box::new(ResolvedType::TypeParam("E".to_owned()))),
        value: bool_literal(),
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::UnresolvedTypeParam { name, location: _ }) => {
            if name != "E" {
                return Err(format!("wrong param name: {name}").into());
            }
            Ok(())
        }
        other => Err(format!("expected UnresolvedTypeParam, got: {other:?}").into()),
    }
}

// ───────────────────────────────────────────────────────────────────
// 5. ResolvedType::Error placeholder
// ───────────────────────────────────────────────────────────────────

#[test]
fn rejects_error_type_in_struct_field() -> TestResult {
    let mut module = IrModule::new();
    module.structs.push(IrStruct {
        name: "Broken".to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: vec![field("oops", ResolvedType::Error)],
        generic_params: Vec::new(),
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::ErrorTypePlaceholder { location }) => {
            if !location.contains("Broken") || !location.contains("oops") {
                return Err(format!("location should mention struct + field: {location}").into());
            }
            Ok(())
        }
        other => Err(format!("expected ErrorTypePlaceholder, got: {other:?}").into()),
    }
}

#[test]
fn rejects_error_type_in_enum_variant_field() -> TestResult {
    let mut module = IrModule::new();
    module.enums.push(IrEnum {
        name: "Status".to_owned(),
        visibility: Visibility::Private,
        variants: vec![IrEnumVariant {
            name: "Failed".to_owned(),
            fields: vec![field("cause", ResolvedType::Error)],
        }],
        generic_params: Vec::new(),
        doc: None,
    });

    match preflight::check(&module) {
        Err(PreflightError::ErrorTypePlaceholder { location }) => {
            if !location.contains("Status") || !location.contains("Failed") {
                return Err(format!("location should mention enum + variant: {location}").into());
            }
            Ok(())
        }
        other => Err(format!("expected ErrorTypePlaceholder, got: {other:?}").into()),
    }
}

// ───────────────────────────────────────────────────────────────────
// Empty modules pass
// ───────────────────────────────────────────────────────────────────

#[test]
fn empty_module_passes() -> TestResult {
    preflight::check(&IrModule::new())
        .map_err(|e| -> TestError { format!("empty module should pass: {e:?}").into() })
}
