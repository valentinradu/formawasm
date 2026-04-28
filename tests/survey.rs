//! Tests for the public-surface survey.

use formalang::ast::{ExternAbi, ParamConvention, PrimitiveType, Visibility};
use formalang::ir::{
    EnumId, FunctionId, IrEnum, IrField, IrFunction, IrModule, IrStruct, ResolvedType, StructId,
};
use formawasm::survey;

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

const fn bool_ty() -> ResolvedType {
    ResolvedType::Primitive(PrimitiveType::Boolean)
}

fn function(name: &str, extern_abi: Option<ExternAbi>) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(bool_ty()),
        body: None,
        extern_abi,
        attributes: Vec::new(),
        doc: None,
    }
}

fn empty_struct(name: &str, visibility: Visibility) -> IrStruct {
    IrStruct {
        name: name.to_owned(),
        visibility,
        traits: Vec::new(),
        fields: vec![IrField {
            name: "ok".to_owned(),
            ty: bool_ty(),
            mutable: false,
            optional: false,
            default: None,
            doc: None,
            convention: ParamConvention::Let,
        }],
        generic_params: Vec::new(),
        doc: None,
    }
}

fn empty_enum(name: &str, visibility: Visibility) -> IrEnum {
    IrEnum {
        name: name.to_owned(),
        visibility,
        variants: Vec::new(),
        generic_params: Vec::new(),
        doc: None,
    }
}

#[test]
fn empty_module_has_empty_surface() -> TestResult {
    let surface = survey::survey(&IrModule::new());
    if !surface.exports.is_empty() {
        return Err(format!("expected no exports, got {:?}", surface.exports).into());
    }
    if !surface.imports.is_empty() {
        return Err(format!("expected no imports, got {:?}", surface.imports).into());
    }
    if !surface.exported_structs.is_empty() {
        return Err(format!(
            "expected no exported_structs, got {:?}",
            surface.exported_structs
        )
        .into());
    }
    if !surface.exported_enums.is_empty() {
        return Err(format!(
            "expected no exported_enums, got {:?}",
            surface.exported_enums
        )
        .into());
    }
    Ok(())
}

#[test]
fn non_extern_function_is_an_export() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function("compute", None));
    let surface = survey::survey(&module);
    if surface.exports != vec![FunctionId(0)] {
        return Err(format!(
            "expected exports [FunctionId(0)], got {:?}",
            surface.exports
        )
        .into());
    }
    if !surface.imports.is_empty() {
        return Err(format!("expected no imports, got {:?}", surface.imports).into());
    }
    Ok(())
}

#[test]
fn extern_function_is_an_import() -> TestResult {
    let mut module = IrModule::new();
    module
        .functions
        .push(function("host_log", Some(ExternAbi::C)));
    let surface = survey::survey(&module);
    if !surface.exports.is_empty() {
        return Err(format!("expected no exports, got {:?}", surface.exports).into());
    }
    if surface.imports != vec![FunctionId(0)] {
        return Err(format!(
            "expected imports [FunctionId(0)], got {:?}",
            surface.imports
        )
        .into());
    }
    Ok(())
}

#[test]
fn mixed_module_partitions_correctly() -> TestResult {
    let mut module = IrModule::new();
    module.functions.push(function("internal", None));
    module
        .functions
        .push(function("host_call", Some(ExternAbi::C)));
    module.functions.push(function("public_api", None));

    let surface = survey::survey(&module);
    if surface.exports != vec![FunctionId(0), FunctionId(2)] {
        return Err(format!("wrong exports: {:?}", surface.exports).into());
    }
    if surface.imports != vec![FunctionId(1)] {
        return Err(format!("wrong imports: {:?}", surface.imports).into());
    }
    Ok(())
}

#[test]
fn only_public_structs_are_exported() -> TestResult {
    let mut module = IrModule::new();
    module
        .structs
        .push(empty_struct("Hidden", Visibility::Private));
    module
        .structs
        .push(empty_struct("Visible", Visibility::Public));
    module
        .structs
        .push(empty_struct("AlsoHidden", Visibility::Private));

    let surface = survey::survey(&module);
    if surface.exported_structs != vec![StructId(1)] {
        return Err(format!("wrong exported_structs: {:?}", surface.exported_structs).into());
    }
    Ok(())
}

#[test]
fn only_public_enums_are_exported() -> TestResult {
    let mut module = IrModule::new();
    module
        .enums
        .push(empty_enum("Internal", Visibility::Private));
    module.enums.push(empty_enum("Status", Visibility::Public));

    let surface = survey::survey(&module);
    if surface.exported_enums != vec![EnumId(1)] {
        return Err(format!("wrong exported_enums: {:?}", surface.exported_enums).into());
    }
    Ok(())
}
