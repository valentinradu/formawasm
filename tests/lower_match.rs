//! Tests for `lower::lower_match`.
//!
//! Each test builds a small enum + a function that constructs a
//! variant and matches on it. The match arm extracts a primitive
//! either via a payload binding or via a fixed literal, and the
//! function returns that primitive so wasmtime can verify it.

use formalang::ast::{
    Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention, PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, EnumId, FieldIdx, IrEnum, IrEnumVariant, IrExpr, IrField, IrFunction, IrMatchArm,
    IrModule, ResolvedType, VariantIdx,
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

const fn primitive(p: PrimitiveType) -> ResolvedType {
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
        ty: primitive(ty),
    }
}

fn field(name: &str, p: PrimitiveType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty: primitive(p),
        mutable: false,
        optional: false,
        default: None,
        doc: None,
        convention: ParamConvention::Let,
    }
}

fn variant(name: &str, fields: Vec<(&str, PrimitiveType)>) -> IrEnumVariant {
    IrEnumVariant {
        name: name.to_owned(),
        fields: fields.into_iter().map(|(n, p)| field(n, p)).collect(),
    }
}

fn enum_def(name: &str, variants: Vec<IrEnumVariant>) -> IrEnum {
    IrEnum {
        name: name.to_owned(),
        visibility: Visibility::Public,
        variants,
        generic_params: Vec::new(),
        doc: None,
    }
}

fn enum_inst(
    enum_id: EnumId,
    variant_idx: u32,
    variant: &str,
    fields: Vec<(&str, IrExpr)>,
) -> IrExpr {
    IrExpr::EnumInst {
        enum_id: Some(enum_id),
        variant: variant.to_owned(),
        variant_idx: VariantIdx(variant_idx),
        fields: fields
            .into_iter()
            .enumerate()
            .map(|(i, (name, e))| (name.to_owned(), FieldIdx(u32::try_from(i).unwrap_or(0)), e))
            .collect(),
        ty: ResolvedType::Enum(enum_id),
    }
}

fn arm(
    variant: &str,
    variant_idx: u32,
    bindings: Vec<(&str, BindingId, PrimitiveType)>,
    body: IrExpr,
) -> IrMatchArm {
    IrMatchArm {
        variant: variant.to_owned(),
        variant_idx: VariantIdx(variant_idx),
        is_wildcard: false,
        bindings: bindings
            .into_iter()
            .map(|(n, id, p)| (n.to_owned(), id, primitive(p)))
            .collect(),
        body,
    }
}

fn match_expr(scrutinee: IrExpr, arms: Vec<IrMatchArm>, ty: PrimitiveType) -> IrExpr {
    IrExpr::Match {
        scrutinee: Box::new(scrutinee),
        arms,
        ty: primitive(ty),
    }
}

fn function(name: &str, return_ty: PrimitiveType, body: IrExpr) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(return_ty)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    }
}

fn binding_ref(id: u32, p: PrimitiveType) -> IrExpr {
    IrExpr::LetRef {
        name: format!("b{id}"),
        binding_id: BindingId(id),
        ty: primitive(p),
    }
}

#[test]
fn match_dispatches_unit_variant_to_correct_arm() -> TestResult {
    // enum Color { Red, Green, Blue }
    // fn pick() -> I32 {
    //   match Color::Green {
    //     Red => 1,
    //     Green => 2,
    //     Blue => 3,
    //   }
    // }
    let mut module = IrModule::new();
    module.enums.push(enum_def(
        "Color",
        vec![
            variant("Red", vec![]),
            variant("Green", vec![]),
            variant("Blue", vec![]),
        ],
    ));
    let scrutinee = enum_inst(EnumId(0), 1, "Green", vec![]);
    let arms = vec![
        arm("Red", 0, vec![], integer_literal(1, PrimitiveType::I32)),
        arm("Green", 1, vec![], integer_literal(2, PrimitiveType::I32)),
        arm("Blue", 2, vec![], integer_literal(3, PrimitiveType::I32)),
    ];
    let body = match_expr(scrutinee, arms, PrimitiveType::I32);
    module
        .functions
        .push(function("pick", PrimitiveType::I32, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "pick")?;
    let got = f.call(&mut store, ())?;
    if got != 2 {
        return Err(format!("got {got}, want 2 (Green arm)").into());
    }
    Ok(())
}

#[test]
fn match_extracts_payload_binding() -> TestResult {
    // enum Maybe { None, Some(I32) }
    // fn unwrap_some() -> I32 {
    //   match Maybe::Some(42) {
    //     None => 0,
    //     Some(v) => v,
    //   }
    // }
    let mut module = IrModule::new();
    module.enums.push(enum_def(
        "Maybe",
        vec![
            variant("None", vec![]),
            variant("Some", vec![("v", PrimitiveType::I32)]),
        ],
    ));
    let scrutinee = enum_inst(
        EnumId(0),
        1,
        "Some",
        vec![("v", integer_literal(42, PrimitiveType::I32))],
    );
    // Binding id 0 = the `v` extracted in the Some arm.
    let arms = vec![
        arm("None", 0, vec![], integer_literal(0, PrimitiveType::I32)),
        arm(
            "Some",
            1,
            vec![("v", BindingId(0), PrimitiveType::I32)],
            binding_ref(0, PrimitiveType::I32),
        ),
    ];
    let body = match_expr(scrutinee, arms, PrimitiveType::I32);
    module
        .functions
        .push(function("unwrap_some", PrimitiveType::I32, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "unwrap-some")?;
    let got = f.call(&mut store, ())?;
    if got != 42 {
        return Err(format!("got {got}, want 42").into());
    }
    Ok(())
}

#[test]
fn match_with_wildcard_runs_default_branch() -> TestResult {
    // enum Color { Red, Green, Blue }
    // fn classify() -> I32 {
    //   match Color::Blue {
    //     Red => 1,
    //     _ => 99,
    //   }
    // }
    let mut module = IrModule::new();
    module.enums.push(enum_def(
        "Color",
        vec![
            variant("Red", vec![]),
            variant("Green", vec![]),
            variant("Blue", vec![]),
        ],
    ));
    let scrutinee = enum_inst(EnumId(0), 2, "Blue", vec![]);
    let mut wildcard_arm = arm("", 0, vec![], integer_literal(99, PrimitiveType::I32));
    wildcard_arm.is_wildcard = true;
    let arms = vec![
        arm("Red", 0, vec![], integer_literal(1, PrimitiveType::I32)),
        wildcard_arm,
    ];
    let body = match_expr(scrutinee, arms, PrimitiveType::I32);
    module
        .functions
        .push(function("classify", PrimitiveType::I32, body));

    let bytes = module_lowering::lower_module(&module)?;
    validate(&bytes)?;
    let engine = Engine::default();
    let m = Module::from_binary(&engine, &bytes)?;
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &m, &[])?;
    let f = instance.get_typed_func::<(), i32>(&mut store, "classify")?;
    let got = f.call(&mut store, ())?;
    if got != 99 {
        return Err(format!("got {got}, want 99 (wildcard arm)").into());
    }
    Ok(())
}
