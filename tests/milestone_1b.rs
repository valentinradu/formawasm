//! Phase 1b unified milestone — struct + enum + method + Match +
//! mut-self assignment composed into one program.
//!
//! Hand-builds an `IrModule` that exercises every Phase 1b mc whose
//! lowering is wired today, runs it through the full
//! `WasmBackend::generate` pipeline, validates the component, and
//! verifies the result under wasmtime's component runtime.
//!
//! Indirect closure invocation is intentionally *not* exercised here:
//! the language does not yet have closure-application syntax (see
//! formalang's `closure_conv/mod.rs`). The Phase 1b mc11 lowering
//! still materialises a closure value as a `(funcref, env_ptr)` pair
//! in linear memory; that piece is covered by
//! `tests/lower_closure_ref.rs` independently of this milestone.

use formalang::ast::{
    BinaryOperator, Literal, NumberLiteral, NumberValue, NumericSuffix, ParamConvention,
    PrimitiveType, Visibility,
};
use formalang::ir::{
    BindingId, DispatchKind, EnumId, FieldIdx, ImplId, ImplTarget, IrBlockStatement, IrEnum,
    IrEnumVariant, IrExpr, IrField, IrFunction, IrFunctionParam, IrImpl, IrMatchArm, IrModule,
    IrSpan, IrStruct, MethodIdx, ReferenceTarget, ResolvedType, StructId, VariantIdx,
};
use formalang::pipeline::Pipeline;
use formawasm::WasmBackend;
use wasmparser::{Validator, WasmFeatures};
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store};

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

fn validate_component(bytes: &[u8]) -> Result<(), TestError> {
    let mut validator = Validator::new_with_features(WasmFeatures::default());
    validator.validate_all(bytes)?;
    Ok(())
}

/// Build the milestone module:
///
/// ```text
/// struct Counter { value: I32 }
///
/// enum Action {
///     Inc,           // unit variant
///     Add(I32),      // payload variant
///     Reset,         // unit variant — used as the wildcard-arm fall-through
/// }
///
/// impl Counter {
///     // Match over Action with one payload-binding arm and two unit
///     // arms; each arm constructs a fresh Counter so the method
///     // returns a Counter pointer.
///     fn apply(self, action: Action) -> Counter {
///         match action {
///             Inc    => Counter { value: self.value + 1 },
///             Add(n) => Counter { value: self.value + n },
///             Reset  => Counter { value: 0 },
///         }
///     }
///
///     // Mut-self path: mutates self.value through the implicit self
///     // pointer, then reads it back. Confirms IrBlockStatement::Assign
///     // on a SelfFieldRef target lands at the right offset.
///     fn double(self) -> I32 {
///         self.value = self.value + self.value;
///         self.value
///     }
/// }
///
/// fn run() -> I32 {
///     // value = 10
///     // .apply(Add(5)) -> 15
///     // .apply(Inc)    -> 16
///     // .double()       -> 32
///     // → 32
///     ...
/// }
/// ```
const COUNTER_ID: StructId = StructId(0);
const ACTION_ID: EnumId = EnumId(0);

const fn counter_ty() -> ResolvedType {
    ResolvedType::Struct(COUNTER_ID)
}

const fn action_ty() -> ResolvedType {
    ResolvedType::Enum(ACTION_ID)
}

fn self_value() -> IrExpr {
    IrExpr::SelfFieldRef {
        field: "value".to_owned(),
        field_idx: FieldIdx(0),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    }
}

fn new_counter(value: IrExpr) -> IrExpr {
    IrExpr::StructInst {
        struct_id: Some(COUNTER_ID),
        type_args: Vec::new(),
        fields: vec![("value".to_owned(), FieldIdx(0), value)],
        ty: counter_ty(),
        span: IrSpan::default(),
    }
}

fn add_op(left: IrExpr, right: IrExpr) -> IrExpr {
    IrExpr::BinaryOp {
        left: Box::new(left),
        right: Box::new(right),
        op: BinaryOperator::Add,
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    }
}

fn primitive_field(name: &str, ty: PrimitiveType) -> IrField {
    IrField {
        name: name.to_owned(),
        ty: primitive(ty),
        mutable: false,
        optional: false,
        default: None,
        doc: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

fn self_param() -> IrFunctionParam {
    IrFunctionParam {
        binding_id: BindingId(0),
        name: "self".to_owned(),
        external_label: None,
        ty: Some(counter_ty()),
        default: None,
        convention: ParamConvention::Let,
        span: IrSpan::default(),
    }
}

fn build_counter_struct() -> IrStruct {
    IrStruct {
        name: "Counter".to_owned(),
        visibility: Visibility::Private,
        traits: Vec::new(),
        fields: vec![IrField {
            mutable: true,
            ..primitive_field("value", PrimitiveType::I32)
        }],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_action_enum() -> IrEnum {
    IrEnum {
        name: "Action".to_owned(),
        visibility: Visibility::Private,
        variants: vec![
            IrEnumVariant {
                name: "Inc".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Add".to_owned(),
                fields: vec![primitive_field("0", PrimitiveType::I32)],
                span: IrSpan::default(),
            },
            IrEnumVariant {
                name: "Reset".to_owned(),
                fields: Vec::new(),
                span: IrSpan::default(),
            },
        ],
        generic_params: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_apply_method() -> IrFunction {
    let action_param_id = BindingId(1);
    let n_binding = BindingId(2);
    let body = IrExpr::Match {
        scrutinee: Box::new(IrExpr::Reference {
            path: vec!["action".to_owned()],
            target: ReferenceTarget::Param(action_param_id),
            ty: action_ty(),
            span: IrSpan::default(),
        }),
        arms: vec![
            IrMatchArm {
                variant: "Inc".to_owned(),
                variant_idx: VariantIdx(0),
                bindings: Vec::new(),
                is_wildcard: false,
                body: new_counter(add_op(self_value(), integer_literal(1))),
            },
            IrMatchArm {
                variant: "Add".to_owned(),
                variant_idx: VariantIdx(1),
                bindings: vec![("n".to_owned(), n_binding, primitive(PrimitiveType::I32))],
                is_wildcard: false,
                body: new_counter(add_op(
                    self_value(),
                    IrExpr::LetRef {
                        name: "n".to_owned(),
                        binding_id: n_binding,
                        ty: primitive(PrimitiveType::I32),
                        span: IrSpan::default(),
                    },
                )),
            },
            IrMatchArm {
                variant: "Reset".to_owned(),
                variant_idx: VariantIdx(2),
                bindings: Vec::new(),
                is_wildcard: false,
                body: new_counter(integer_literal(0)),
            },
        ],
        ty: counter_ty(),
        span: IrSpan::default(),
    };
    IrFunction {
        name: "apply".to_owned(),
        generic_params: Vec::new(),
        params: vec![
            self_param(),
            IrFunctionParam {
                binding_id: action_param_id,
                name: "action".to_owned(),
                external_label: None,
                ty: Some(action_ty()),
                default: None,
                convention: ParamConvention::Let,
                span: IrSpan::default(),
            },
        ],
        return_type: Some(counter_ty()),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_double_method() -> IrFunction {
    // `self.value = self.value + self.value; self.value`
    let body = IrExpr::Block {
        statements: vec![IrBlockStatement::Assign {
            target: self_value(),
            value: add_op(self_value(), self_value()),
            span: IrSpan::default(),
        }],
        result: Box::new(self_value()),
        ty: primitive(PrimitiveType::I32),
        span: IrSpan::default(),
    };
    IrFunction {
        name: "double".to_owned(),
        generic_params: Vec::new(),
        params: vec![self_param()],
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(body),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn action_value(variant: &str, idx: u32, fields: Vec<(String, FieldIdx, IrExpr)>) -> IrExpr {
    IrExpr::EnumInst {
        enum_id: Some(ACTION_ID),
        variant: variant.to_owned(),
        variant_idx: VariantIdx(idx),
        fields,
        ty: action_ty(),
        span: IrSpan::default(),
    }
}

fn method_chain(
    receiver: IrExpr,
    method: &str,
    method_idx: u32,
    args: Vec<IrExpr>,
    ret_ty: ResolvedType,
) -> IrExpr {
    IrExpr::MethodCall {
        receiver: Box::new(receiver),
        method: method.to_owned(),
        method_idx: MethodIdx(method_idx),
        args: args.into_iter().map(|a| (None, a)).collect(),
        dispatch: DispatchKind::Static { impl_id: ImplId(0) },
        ty: ret_ty,
        span: IrSpan::default(),
    }
}

fn build_run_function() -> IrFunction {
    let initial = IrExpr::StructInst {
        struct_id: Some(COUNTER_ID),
        type_args: Vec::new(),
        fields: vec![("value".to_owned(), FieldIdx(0), integer_literal(10))],
        ty: counter_ty(),
        span: IrSpan::default(),
    };
    let after_add = method_chain(
        initial,
        "apply",
        0,
        vec![action_value(
            "Add",
            1,
            vec![("0".to_owned(), FieldIdx(0), integer_literal(5))],
        )],
        counter_ty(),
    );
    let after_inc = method_chain(
        after_add,
        "apply",
        0,
        vec![action_value("Inc", 0, Vec::new())],
        counter_ty(),
    );
    let doubled = method_chain(
        after_inc,
        "double",
        1,
        Vec::new(),
        primitive(PrimitiveType::I32),
    );

    IrFunction {
        name: "run".to_owned(),
        generic_params: Vec::new(),
        params: Vec::new(),
        return_type: Some(primitive(PrimitiveType::I32)),
        body: Some(doubled),
        extern_abi: None,
        attributes: Vec::new(),
        doc: None,
        span: IrSpan::default(),
    }
}

fn build_milestone_module() -> IrModule {
    let mut module = IrModule::new();
    module.structs.push(build_counter_struct());
    module.enums.push(build_action_enum());
    module.impls.push(IrImpl {
        target: ImplTarget::Struct(COUNTER_ID),
        trait_ref: None,
        is_extern: false,
        generic_params: Vec::new(),
        functions: vec![build_apply_method(), build_double_method()],
        span: IrSpan::default(),
    });
    module.functions.push(build_run_function());
    module
}

#[test]
fn struct_enum_method_match_assign_compose_through_component_pipeline() -> TestResult {
    let module = build_milestone_module();
    let mut pipeline = Pipeline::new();
    let bytes = pipeline.emit(module, &WasmBackend::new())?;
    validate_component(&bytes)?;

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config)?;
    let component = Component::from_binary(&engine, &bytes)?;
    let mut linker = Linker::<()>::new(&engine);
    // formalang's prelude declares `pub extern fn assert(condition: Boolean)`;
    // every program parsed via `compile_to_ir_*` imports it. Wire to a host
    // function that traps on `false` so failed assertions surface as wasmtime
    // traps to the test caller.
    linker.root().func_wrap("assert", |_store, (cond,): (bool,)| {
        if cond { Ok(()) } else { Err(wasmtime::Error::msg("assert(false)")) }
    })?;
    let mut store = Store::new(&engine, ());
    let instance = linker.instantiate(&mut store, &component)?;
    let run = instance.get_typed_func::<(), (i32,)>(&mut store, "run")?;

    // 10 -> Add(5) -> 15 -> Inc -> 16 -> double() -> 32
    let (got,) = run.call(&mut store, ())?;
    if got != 32 {
        return Err(format!("run() = {got}, want 32").into());
    }
    Ok(())
}
