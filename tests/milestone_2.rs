//! Phase 2 closing milestone — a single program that exercises every
//! Phase 2 feature family at once:
//!
//! * **Strings** as parameter, return type, dictionary keys, dictionary
//!   values, and string concatenation (`BinaryOp::Add`).
//! * **Optionals** via a `let x: I32? = ...` Some-wrap site.
//! * **Dictionaries** as a sorted-pairs literal plus key lookup.
//!
//! The program builds end-to-end through `WasmBackend::generate` (with
//! pre-flight + survey + core lowering + WIT + component wrap) and is
//! instantiated under wasmtime's component runtime. The exported
//! `greet(role)` function looks up `role` in a string-keyed
//! dictionary, concatenates a hard-coded prefix with the looked-up
//! value, and wraps a sentinel marker into an `I32?` along the way.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType,
};
use formalang::ir::{
    BindingId, IrBlockStatement, IrExpr, IrFunction, IrFunctionParam, IrModule, ReferenceTarget,
    ResolvedType,
};
use formalang::pipeline::Pipeline;
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

type TestError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), TestError>;

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

const fn primitive(p: PrimitiveType) -> ResolvedType {
    ResolvedType::Primitive(p)
}

fn dict_ty(key: ResolvedType, value: ResolvedType) -> ResolvedType {
    ResolvedType::Dictionary {
        key_ty: Box::new(key),
        value_ty: Box::new(value),
    }
}

fn optional(inner: ResolvedType) -> ResolvedType {
    ResolvedType::Optional(Box::new(inner))
}

fn string_literal(text: &str) -> IrExpr {
    IrExpr::Literal {
        value: Literal::String(text.to_owned()),
        ty: primitive(PrimitiveType::String),
    }
}

const fn integer_literal(value: i128) -> IrExpr {
    IrExpr::Literal {
        value: Literal::Number(NumberLiteral::suffixed(
            NumberValue::Integer(value),
            NumericSuffix::I32,
        )),
        ty: primitive(PrimitiveType::I32),
    }
}

fn add_strings(left: IrExpr, right: IrExpr) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(left),
        right: Box::new(right),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::String),
    }
}

#[test]
fn phase_2_milestone_strings_optionals_dicts_concat() -> TestResult {
    // Source intent (Phase 2 feature dump):
    //
    // ```formalang
    // pub fn greet(role: String) -> String {
    //     let table: [String: String] = [
    //         "captain": "ahoy",
    //         "engineer": "hello",
    //         "guest": "welcome",
    //     ]
    //     let _seen: I32? = 1   // Some-wrap site
    //     "Greetings, " + role + ": " + table[role]
    // }
    // ```
    //
    // Build the equivalent IR by hand, run through the backend, and
    // call the export under the component runtime.

    let role_binding = BindingId(0);
    let table_binding = BindingId(1);
    let seen_binding = BindingId(2);

    let role_ref = || IrExpr::Reference {
        path: vec!["role".to_owned()],
        target: ReferenceTarget::Param(role_binding),
        ty: primitive(PrimitiveType::String),
    };
    let table_ref = || IrExpr::LetRef {
        binding_id: table_binding,
        name: "table".to_owned(),
        ty: dict_ty(
            primitive(PrimitiveType::String),
            primitive(PrimitiveType::String),
        ),
    };

    let table_literal = IrExpr::DictLiteral {
        entries: vec![
            (string_literal("captain"), string_literal("ahoy")),
            (string_literal("engineer"), string_literal("hello")),
            (string_literal("guest"), string_literal("welcome")),
        ],
        ty: dict_ty(
            primitive(PrimitiveType::String),
            primitive(PrimitiveType::String),
        ),
    };

    let seen_let = IrBlockStatement::Let {
        binding_id: seen_binding,
        name: "_seen".to_owned(),
        mutable: false,
        ty: Some(optional(primitive(PrimitiveType::I32))),
        value: integer_literal(1),
    };
    let table_let = IrBlockStatement::Let {
        binding_id: table_binding,
        name: "table".to_owned(),
        mutable: false,
        ty: None,
        value: table_literal,
    };

    let lookup = IrExpr::DictAccess {
        dict: Box::new(table_ref()),
        key: Box::new(role_ref()),
        ty: primitive(PrimitiveType::String),
    };

    // "Greetings, " + role + ": " + table[role]
    let body_expr = add_strings(
        add_strings(
            add_strings(string_literal("Greetings, "), role_ref()),
            string_literal(": "),
        ),
        lookup,
    );

    let body = IrExpr::Block {
        statements: vec![table_let, seen_let],
        result: Box::new(body_expr),
        ty: primitive(PrimitiveType::String),
    };

    let greet = IrFunction {
        name: "greet".to_owned(),
        generic_params: Vec::new(),
        params: vec![IrFunctionParam {
            binding_id: role_binding,
            name: "role".to_owned(),
            external_label: None,
            ty: Some(primitive(PrimitiveType::String)),
            default: None,
            convention: ParamConvention::Let,
        }],
        return_type: Some(primitive(PrimitiveType::String)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
    };
    let mut module = IrModule::new();
    module.functions.push(greet);

    let mut pipeline = Pipeline::new();
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let linker = Linker::<()>::new(&engine);
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let greet_fn = instance.get_typed_func::<(String,), (String,)>(&mut store, "greet")?;

    for (role, expected) in [
        ("captain", "Greetings, captain: ahoy"),
        ("engineer", "Greetings, engineer: hello"),
        ("guest", "Greetings, guest: welcome"),
    ] {
        let (actual,) = greet_fn.call(&mut store, (role.to_owned(),))?;
        if actual != expected {
            return Err(format!("greet({role:?}): got {actual:?}, want {expected:?}").into());
        }
    }
    Ok(())
}
